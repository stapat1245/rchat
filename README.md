# rchat

A peer-to-peer **group** terminal chat that rides on the
[rnet](https://github.com/stapat1245/rnet) WireGuard overlay. Everyone on the
same rnet network runs `rchat` and talks in one room — no server, no cloud. The
links are already encrypted + authenticated by WireGuard. The UI is a calm
greyscale ratatui with a block-font banner.

## How it works (N-peer full mesh)

1. rnet puts every node on `10.42.0.0/24` via `mynet0` (up to 254 peers).
2. `rchat` reads its own overlay IP from `mynet0` and the full member list from
   rnet's `~/.config/mynet/peers.toml` (falling back to the classic `.1`/`.2`
   pair before that file exists).
3. Mesh rule: every node **listens**, and only **dials peers with a lower
   overlay IP** — so each pair forms exactly one TCP connection. A peer is
   identified by the overlay IP it declares in its `Hello`.
4. A message is **broadcast** to all connected peers. Messages are
   newline-delimited JSON; TCP gives ordering + reliability, WireGuard gives
   confidentiality.
5. New members that join rnet mid-session are picked up automatically — the
   dialer re-reads `peers.toml` every couple of seconds.

## Build

```bash
cargo build --release   # binary at target/release/rchat
```

## Use

On every laptop, after rnet is connected:

```bash
rchat                 # auto-discovers overlay IP + all peers, opens the room
rchat --nick alice    # override display name (default: $USER)
rchat --port 8086     # override TCP port (default 8086)
rchat status          # print own IP, discovered peers + roles, history path
```

The status bar shows the live roster: your name, how many peers are online, and
their names. Keys: **Enter** send · **PgUp/PgDn** scroll · **Esc** / **Ctrl-C** quit.

## History

Appended as JSON Lines under `~/.config/rchat/history/`:

- `group.jsonl` — rolling group history (replayed on restart)
- `session-<ts>.jsonl` — this run only

Each line: `{"ts":…,"iso":"…","dir":"in|out|event","nick":"…","body":"…"}`.

## Layout

```
src/
├── main.rs      # CLI + orchestration
├── config.rs    # own overlay IP (mynet0 → local.toml) + peer set (peers.toml)
├── proto.rs     # JSON wire protocol (hello{from,nick} / msg / ping / bye)
├── net.rs       # full-mesh hub: listener + dialer, per-conn reader/writer, broadcast
├── history.rs   # group JSONL logging under ~/.config/rchat
└── ui.rs        # greyscale ratatui UI (block banner, roster, chat + input)
```
