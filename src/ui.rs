//! Terminal UI — a calm greyscale look.
//!
//! A monochrome block "RCHAT" banner, plain borders, muted grey text, and a
//! status bar that shows the live peer roster for the whole mesh.

use crate::history::History;
use crate::net::{NetEvent, NetHandle};
use anyhow::Result;
use chrono::{Local, TimeZone};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use std::collections::BTreeMap;
use std::io::stdout;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

// ── muted greyscale palette ─────────────────────────────────────────────────
const FG: Color = Color::Rgb(205, 205, 205); // normal text
const BRIGHT: Color = Color::Rgb(235, 235, 235); // our own name / emphasis
const PEER: Color = Color::Rgb(165, 165, 170); // peer names
const DIM: Color = Color::Rgb(120, 120, 125); // timestamps / hints
const FRAME: Color = Color::Rgb(85, 85, 90); // borders / separators
const SYS: Color = Color::Rgb(110, 110, 115); // system lines
const ONLINE: Color = Color::Rgb(150, 170, 150); // soft green-grey
const OFFLINE: Color = Color::Rgb(160, 120, 120); // soft red-grey

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Own,
    Peer,
    System,
}

pub struct Entry {
    pub kind: Kind,
    pub ts: i64,
    pub nick: String,
    pub body: String,
}

struct PeerInfo {
    nick: Option<String>,
    online: bool,
}

pub struct App {
    pub nick: String,
    pub own_ip: Ipv4Addr,
    peers: BTreeMap<Ipv4Addr, PeerInfo>,
    pub input: String,
    pub entries: Vec<Entry>,
    pub scroll: u16,
    pub should_quit: bool,
    counter: u64,
}

impl App {
    pub fn new(nick: String, own_ip: Ipv4Addr) -> Self {
        App {
            nick,
            own_ip,
            peers: BTreeMap::new(),
            input: String::new(),
            entries: Vec::new(),
            scroll: 0,
            should_quit: false,
            counter: 0,
        }
    }

    fn next_id(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    pub fn push(&mut self, kind: Kind, nick: &str, body: &str, ts: i64) {
        self.entries.push(Entry {
            kind,
            ts,
            nick: nick.to_string(),
            body: body.to_string(),
        });
        self.scroll = 0;
    }

    pub fn system(&mut self, body: &str) {
        let ts = Local::now().timestamp();
        self.push(Kind::System, "*", body, ts);
    }

    fn peer_up(&mut self, ip: Ipv4Addr) {
        self.peers
            .entry(ip)
            .or_insert(PeerInfo { nick: None, online: false })
            .online = true;
    }

    fn peer_nick(&mut self, ip: Ipv4Addr, nick: String) {
        self.peers
            .entry(ip)
            .or_insert(PeerInfo { nick: None, online: true })
            .nick = Some(nick);
    }

    fn peer_down(&mut self, ip: Ipv4Addr) {
        if let Some(p) = self.peers.get_mut(&ip) {
            p.online = false;
        }
    }

    fn online_count(&self) -> usize {
        self.peers.values().filter(|p| p.online).count()
    }

    fn label_for(&self, ip: Ipv4Addr) -> String {
        self.peers
            .get(&ip)
            .and_then(|p| p.nick.clone())
            .unwrap_or_else(|| ip.to_string())
    }
}

/// Run the whole TUI loop until the user quits.
pub fn run(mut app: App, net: NetHandle, history: History) -> Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let res = event_loop(&mut terminal, &mut app, &net, &history);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    net: &NetHandle,
    history: &History,
) -> Result<()> {
    let tick = Duration::from_millis(120);
    let mut last_blink = Instant::now();
    let mut cursor_on = true;

    loop {
        while let Ok(ev) = net.events.try_recv() {
            match ev {
                NetEvent::PeerUp(ip) => {
                    app.peer_up(ip);
                    app.system(&format!("{ip} connected"));
                }
                NetEvent::PeerNick(ip, nick) => {
                    app.peer_nick(ip, nick.clone());
                    app.system(&format!("{nick} is here"));
                }
                NetEvent::Message(ts, nick, body) => {
                    app.push(Kind::Peer, &nick, &body, ts);
                    history.append("in", &nick, &body, ts);
                }
                NetEvent::PeerDown(ip) => {
                    let label = app.label_for(ip);
                    app.peer_down(ip);
                    app.system(&format!("{label} left"));
                }
                NetEvent::Info(s) => app.system(&s),
            }
        }

        if last_blink.elapsed() >= Duration::from_millis(500) {
            cursor_on = !cursor_on;
            last_blink = Instant::now();
        }

        terminal.draw(|f| draw(f, app, cursor_on))?;

        if event::poll(tick)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Char('c') if ctrl => app.should_quit = true,
                    KeyCode::Esc => app.should_quit = true,
                    KeyCode::Enter => send_current(app, net, history),
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::PageUp => app.scroll = app.scroll.saturating_add(3),
                    KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(3),
                    KeyCode::Char(c) => app.input.push(c),
                    _ => {}
                }
            }
        }

        if app.should_quit {
            net.send_bye();
            break;
        }
    }
    Ok(())
}

fn send_current(app: &mut App, net: &NetHandle, history: &History) {
    let body = app.input.trim().to_string();
    app.input.clear();
    if body.is_empty() {
        return;
    }
    let ts = Local::now().timestamp();
    let id = app.next_id();
    net.send_msg(id, ts, &app.nick, &body);
    let nick = app.nick.clone();
    app.push(Kind::Own, &nick, &body, ts);
    history.append("out", &nick, &body, ts);
}

// ── rendering ───────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App, cursor_on: bool) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status
            Constraint::Min(3),    // chat
            Constraint::Length(3), // input
        ])
        .split(f.area());

    draw_status(f, app, root[0]);
    draw_chat(f, app, root[1]);
    draw_input(f, app, root[2], cursor_on);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let n = app.online_count();
    let (dot, dot_color) = if n > 0 { ("●", ONLINE) } else { ("○", OFFLINE) };

    // Roster of online peer labels (names where known).
    let roster: Vec<String> = app
        .peers
        .iter()
        .filter(|(_, p)| p.online)
        .map(|(ip, p)| p.nick.clone().unwrap_or_else(|| ip.to_string()))
        .collect();
    let roster_str = if roster.is_empty() {
        "—".to_string()
    } else {
        roster.join(", ")
    };

    let spans = vec![
        Span::styled("you ", Style::default().fg(DIM)),
        Span::styled(
            format!("{} ", app.nick),
            Style::default().fg(BRIGHT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{}  ", app.own_ip), Style::default().fg(DIM)),
        Span::styled("│ ", Style::default().fg(FRAME)),
        Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
        Span::styled(
            format!("{n} online  "),
            Style::default().fg(FG),
        ),
        Span::styled("│ ", Style::default().fg(FRAME)),
        Span::styled(roster_str, Style::default().fg(PEER)),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_chat(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(FRAME))
        .title(Span::styled(" chat ", Style::default().fg(DIM)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let width = inner.width.max(1) as usize;
    let height = inner.height as usize;

    let mut all: Vec<Line> = Vec::new();
    for e in &app.entries {
        for l in render_entry(e, width) {
            all.push(l);
        }
    }

    let total = all.len();
    let scroll = app.scroll as usize;
    let end = total.saturating_sub(scroll);
    let start = end.saturating_sub(height);
    let view: Vec<Line> = all[start..end].to_vec();

    f.render_widget(Paragraph::new(view), inner);
}

fn render_entry(e: &Entry, width: usize) -> Vec<Line<'static>> {
    let time = hhmm(e.ts);
    let (nick_color, body_color, name) = match e.kind {
        Kind::Own => (BRIGHT, FG, e.nick.as_str()),
        Kind::Peer => (PEER, FG, e.nick.as_str()),
        Kind::System => (SYS, SYS, "*"),
    };

    let prefix_plain = format!("{time} {name} │ ");
    let prefix_len = prefix_plain.chars().count();
    let avail = width.saturating_sub(prefix_len).max(8);

    let wrapped = wrap_text(&e.body, avail);
    let mut lines = Vec::new();
    for (i, chunk) in wrapped.iter().enumerate() {
        if i == 0 {
            let spans = vec![
                Span::styled(format!("{time} "), Style::default().fg(DIM)),
                Span::styled(
                    name.to_string(),
                    Style::default().fg(nick_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" │ ", Style::default().fg(FRAME)),
                Span::styled(chunk.clone(), Style::default().fg(body_color)),
            ];
            lines.push(Line::from(spans));
        } else {
            let pad = " ".repeat(prefix_len);
            lines.push(Line::from(vec![
                Span::raw(pad),
                Span::styled(chunk.clone(), Style::default().fg(body_color)),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(prefix_plain, Style::default().fg(SYS))));
    }
    lines
}

fn draw_input(f: &mut Frame, app: &App, area: Rect, cursor_on: bool) {
    let cursor = if cursor_on { "▏" } else { " " };
    let line = Line::from(vec![
        Span::styled("› ", Style::default().fg(DIM)),
        Span::styled(app.input.clone(), Style::default().fg(FG)),
        Span::styled(cursor.to_string(), Style::default().fg(FG)),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(FRAME))
        .title(Span::styled(
            " Enter send · PgUp/PgDn scroll · Esc quit ",
            Style::default().fg(DIM),
        ))
        .title_alignment(Alignment::Right);
    f.render_widget(Paragraph::new(line).block(block), area);
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn hhmm(ts: i64) -> String {
    match Local.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%H:%M").to_string(),
        None => "--:--".to_string(),
    }
}

/// Word-aware wrap to `width` columns (falls back to hard splits).
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let mut cur = String::new();
        let mut cur_len = 0usize;
        for word in raw.split(' ') {
            let wlen = word.chars().count();
            if wlen > width {
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }
                let mut buf = String::new();
                for c in word.chars() {
                    if buf.chars().count() == width {
                        lines.push(std::mem::take(&mut buf));
                    }
                    buf.push(c);
                }
                cur = buf;
                cur_len = cur.chars().count();
                continue;
            }
            let add = if cur.is_empty() { wlen } else { wlen + 1 };
            if cur_len + add > width {
                lines.push(std::mem::take(&mut cur));
                cur = word.to_string();
                cur_len = wlen;
            } else {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(word);
                cur_len += add;
            }
        }
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

