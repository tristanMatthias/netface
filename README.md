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
| `-t, --theme` | `detailed` | Character theme |
| `-m, --color-mode` | `matrix` | Color mode (auto-enables color) |
| `-b, --bg-removal` | off | AI background removal |
| `--no-audio` | off | Disable microphone / speakers |

### Themes

**Character themes** control which characters represent brightness levels:

| Theme | Characters | Description |
|-------|------------|-------------|
| `detailed` | 92 ASCII chars | High detail (default) |
| `classic` | ` .:-=+*#%@` | Simple 10-char ramp |
| `blocks` | `░▒▓█` | Unicode block elements |
| `dots` | Braille dots | Braille patterns |
| `emoji-moon` | `🌑🌒🌓🌔🌕` | Moon phases |
| `emoji-hearts` | `🖤💜💙💚💛🧡❤️` | Heart gradient |
| `emoji-fire` | `⬛🟫🟠🟡⬜` | Fire/heat squares |

**Color modes** control how colors are applied:

| Mode | Description |
|------|-------------|
| `matrix` | Black to green gradient (default) |
| `original` | Use actual pixel colors |
| `mono-green` | Classic green terminal |
| `mono-amber` | Retro amber terminal |
| `cyberpunk` | Magenta to cyan gradient |
| `sunset` | Purple to orange gradient |
| `ice` | Dark blue to white gradient |
| `sepia` | Warm brownish tint |
| `cool` | Cool bluish tint |

Example combinations:
```sh
netface --peer host:4444 --theme blocks --color-mode original
netface --peer host:4444 --theme emoji-moon --color-mode mono-white
netface --peer host:4444 --color-mode cyberpunk
```

### Configuration

Settings persist in `~/.config/netface/config.toml`. CLI flags override config values.

```sh
# Create default config
netface --init-config

# Print example config
netface --example-config
```

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
