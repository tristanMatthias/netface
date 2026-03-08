# netface

P2P ASCII webcam + audio chat over raw UDP. No servers, no accounts — two people, one binary.

Before your peer connects you see your own camera full-screen. The moment their first packet arrives the view snaps to a 50/50 left (you) / right (them) split.

```
┌────────────────────┬────────────────────┐
│                    │                    │
│    you             │    them            │
│                    │                    │
└────────────────────┴────────────────────┘
 Waiting for peer on :4444  │  Ctrl-C to quit
```

## Usage

Both parties run the same binary — just tell it where the other person is:

```sh
# Alice
netface --peer bob.ip:4444

# Bob
netface --peer alice.ip:4444
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--peer` | *(required)* | Peer address `host:port` |
| `-P, --port` | `4444` | Local listen port |
| `--camera` | `0` | Webcam device index |
| `--fps` | `15` | Capture frame rate |
| `-C, --color` | off | ANSI truecolor output |
| `--no-audio` | off | Disable microphone / speakers |

## Build

```sh
cargo build --release
```

Binary ends up at `target/release/netface` (~1 MB, no runtime required).

### Cross-compile

```sh
# Add targets once
rustup target add aarch64-apple-darwin
rustup target add x86_64-unknown-linux-gnu

# Build
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-unknown-linux-gnu
```

**Linux** requires `libasound2-dev` (ALSA) for audio.
**macOS** will prompt for camera and microphone access on first run.

## Protocol

Plain UDP — no handshake, no encryption, no NAT traversal. Works on a LAN out of the box. For internet use, put both machines on a VPN or forward a UDP port.

| Layer | Detail |
|-------|--------|
| Transport | UDP (raw, connectionless) |
| Packet | 9-byte header: `type(1) + seq(4) + len(4)` + payload |
| Video | Webcam → resize → ASCII (90-char ramp) → lz4 → UDP |
| Audio | Mic → mono f32 PCM → UDP → speaker ring buffer |
