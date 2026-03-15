# netface

P2P ASCII webcam + audio over WebRTC. No servers, no accounts — two people, one binary.

Before your peer connects you see your own camera full-screen. The moment they connect the view snaps to a 50/50 left (you) / right (them) split.

```
┌────────────────────┬────────────────────┐
│                    │                    │
│    you             │    them            │
│                    │                    │
└────────────────────┴────────────────────┘
 Connected to @alice  │  Ctrl-C to quit
```

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/tristanMatthias/netface/main/install.sh | sh
```

To update to the latest version at any time:

```sh
netface --update
```

## Quick start

**Alice** registers a handle and waits:
```sh
netface --register-handle alice
netface --listen
```

**Bob** calls her:
```sh
netface --handle @alice
```

That's it. Once connected it's live ASCII video + audio — no text chat, no accounts, no middlemen.

---

You can also share your identity directly without a handle:
```sh
# Alice shares her npub
netface --show-identity

# Bob calls using the npub
netface --handle npub1...
```

## Options

| Flag | Description |
|------|-------------|
| `-H, --handle` | Connect to a peer by handle (`@alice`) or npub |
| `--listen` | Wait for an incoming call |
| `--register-handle` | Register a human-readable handle for your identity |
| `--show-identity` | Print your npub for sharing |
| `--camera` | Webcam device index (default: `0`) |
| `--fps` | Capture frame rate (default: `15`) |
| `-t, --theme` | Character theme (default: `detailed`) |
| `-c, --color-mode` | Color mode (default: `matrix`) |
| `--no-audio` | Disable microphone / speakers |
| `--no-bg-removal` | Disable AI background removal |
| `--update` | Update netface to the latest release |

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

**Color modes:**

| Mode | Description |
|------|-------------|
| `matrix` | Black to green gradient (default) |
| `original` | Actual pixel colors |
| `mono-green` | Classic green terminal |
| `mono-amber` | Retro amber terminal |
| `cyberpunk` | Magenta to cyan gradient |
| `sunset` | Purple to orange gradient |
| `ice` | Dark blue to white gradient |
| `sepia` | Warm brownish tint |

```sh
netface --handle @alice --theme blocks --color-mode original
netface --handle @alice --color-mode cyberpunk
```

### Configuration

Settings persist in `~/.config/netface/config.toml`. CLI flags override config values.

```sh
netface --init-config     # create default config
netface --example-config  # print example config
```

## Build from source

```sh
cargo build --release
```

**Linux** requires `libasound2-dev` and `libv4l-dev`. **macOS** will prompt for camera and microphone access on first run.
