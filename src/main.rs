//! rchat — a peer-to-peer, pixelated terminal chat that rides on the rnet
//! WireGuard overlay. Two laptops on the same rnet network each run `rchat`
//! and talk directly, no server.

mod config;
mod history;
mod net;
mod proto;
mod ui;

use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use config::Session;
use history::History;
use ui::{App, Kind};

#[derive(Parser, Debug)]
#[command(
    name = "rchat",
    version,
    about = "P2P pixelated terminal chat over the rnet overlay"
)]
struct Cli {
    /// Display name (default: $USER).
    #[arg(short, long)]
    nick: Option<String>,

    /// Overlay interface to read our IP from.
    #[arg(long, default_value = "mynet0")]
    iface: String,

    /// TCP port for the chat.
    #[arg(short, long, default_value_t = 8086)]
    port: u16,

    #[command(subcommand)]
    cmd: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print discovered overlay IP, peer, role, and exit.
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let session = Session::discover(cli.nick.clone(), &cli.iface, cli.port)?;

    if let Some(Command::Status) = cli.cmd {
        print_status(&session);
        return Ok(());
    }

    run_chat(session)
}

fn print_status(s: &Session) {
    let peers = s.peer_ips();
    println!("rchat status");
    println!("  nick     : {}", s.nick);
    println!("  interface: {}", s.iface);
    println!("  own IP   : {}:{}", s.own_ip, s.port);
    if peers.is_empty() {
        println!("  peers    : (none discovered yet)");
    } else {
        println!("  peers    : {} discovered", peers.len());
        for p in &peers {
            let role = if *p < s.own_ip { "we dial" } else { "they dial" };
            println!("             {p}:{}  ({role})", s.port);
        }
    }
    println!("  history  : {}", history::base_dir().join("history").display());
}

fn run_chat(session: Session) -> Result<()> {
    let started = Local::now().timestamp();
    let history = History::open(started)?;

    let mut app = App::new(session.nick.clone(), session.own_ip);
    app.system(&format!(
        "rchat ready — you are {} ({})",
        session.own_ip, session.nick
    ));

    // Replay recent history so a restart shows prior context.
    for rec in History::load_recent(50) {
        let kind = match rec.dir.as_str() {
            "out" => Kind::Own,
            "in" => Kind::Peer,
            _ => Kind::System,
        };
        app.push(kind, &rec.nick, &rec.body, rec.ts);
    }

    let net = net::spawn(session);
    ui::run(app, net, history)
}
