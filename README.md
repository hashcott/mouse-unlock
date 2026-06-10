# mouse-unlock

> Unlock your Linux screen with a **secret mouse-click pattern** — an ultra-light, written-in-Rust daemon.

[![Build](https://github.com/hashcott/mouse-unlock/actions/workflows/build.yml/badge.svg)](https://github.com/hashcott/mouse-unlock/actions/workflows/build.yml)
[![Release](https://github.com/hashcott/mouse-unlock/actions/workflows/auto-release.yml/badge.svg)](https://github.com/hashcott/mouse-unlock/actions/workflows/auto-release.yml)
[![Latest release](https://img.shields.io/github/v/release/hashcott/mouse-unlock?sort=semver)](https://github.com/hashcott/mouse-unlock/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Click a pattern like **Left-Left-Right-Right-Left** on your mouse and the screen unlocks — no password typing. A small Rust daemon with a tiny footprint.

---

## Table of contents

- [Features](#features)
- [Why so light?](#why-so-light)
- [Install](#install)
  - [Quick install (prebuilt, recommended)](#quick-install-prebuilt-recommended)
  - [From source](#from-source)
  - [Manual (from a release tarball)](#manual-from-a-release-tarball)
- [Usage](#usage)
  - [Setup UI](#setup-ui)
  - [Configuration](#configuration)
  - [Test mode](#test-mode)
  - [Monitoring](#monitoring)
- [Environment support](#environment-support)
- [Uninstall](#uninstall)
- [CI & releases](#ci--releases)
- [Security](#-security)
- [Contributing](#contributing)
- [License](#license)

## Features

- 🪶 **Tiny footprint** — static Rust binary (~400 KB), ~1–2 MB RAM, **0% CPU when idle**.
- 🖱️ **Click patterns** — any sequence of left / right / middle clicks, with a configurable timeout.
- 🔒 **Hashed secret** — the pattern is stored as an **Argon2 hash** in a root-only (`0600`) config, never in plaintext.
- 🛡️ **Brute-force lockout** — exponential backoff after too many wrong attempts (counted only while the screen is locked).
- 🔑 **Optional USB second factor** — only unlock when a specific USB device is plugged in.
- 🖥️ **Works everywhere** — uses `loginctl unlock-sessions` (systemd-logind): KDE, GNOME, XFCE… on both Wayland and X11.
- 🎛️ **Friendly setup** — a terminal UI (`mouse-unlock-setup`) to record the pattern by clicking and to install/uninstall the service.
- ⚙️ **Runs at boot** — installed as a systemd service.

## Why so light?

- **Event-driven**: reads `/dev/input/*` with a blocking `read()` → the process **sleeps at 0% CPU** until a click arrives. No polling, no wasted power.
- **No runtime / no GC**: a static Rust binary, ~1–2 MB resident RAM (vs ~15–30 MB for an interpreted equivalent).
- **Size-optimized** release profile (`opt-level="z"`, `lto`, `strip`, `panic="abort"`).

## Install

> Requires a Linux system with **systemd**. Prebuilt binaries are **statically linked (musl)** for **x86_64**, so they run on any distro regardless of glibc version; other architectures should [build from source](#from-source).

### Quick install (prebuilt, recommended)

No Rust toolchain needed — downloads the latest release and sets up the service:

```bash
curl -fsSL https://raw.githubusercontent.com/hashcott/mouse-unlock/master/scripts/install.sh | sudo bash
```

Pin a specific version with `VERSION`:

```bash
curl -fsSL https://raw.githubusercontent.com/hashcott/mouse-unlock/master/scripts/install.sh | sudo VERSION=v0.1.0 bash
```

### From source

Requires [Rust/cargo](https://rustup.rs):

```bash
git clone https://github.com/hashcott/mouse-unlock.git
cd mouse-unlock
sudo bash install.sh
```

This builds and installs **two** binaries:

- `mouse-unlock` — the lightweight background daemon (auto-starts at boot).
- `mouse-unlock-setup` — the terminal UI to configure and manage it.

### Manual (from a release tarball)

```bash
tar -xzf mouse-unlock-vX.Y.Z-linux-x86_64.tar.gz
cd mouse-unlock-vX.Y.Z-linux-x86_64
sudo install -m0755 mouse-unlock mouse-unlock-setup /usr/local/bin/
sudo install -m0600 mouse-unlock.conf /etc/mouse-unlock.conf
sudo install -m0644 mouse-unlock.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now mouse-unlock
sudo mouse-unlock-setup   # set your click pattern
```

## Usage

### Setup UI

A small terminal interface (ratatui) for configuring everything — no need to edit files by hand. **You must run this at least once** to set your pattern:

```bash
sudo mouse-unlock-setup
```

```
╭ Mouse Unlock — Setup ──────────────────────────╮
│ Pattern : configured (hidden, hashed)          │
│ Timeout : 2000 ms                              │
│ Unlock  : loginctl unlock-sessions             │
│ USB 2FA : (none — optional)                     │
│ Lockout : after 5 fails, 2000–300000 ms backoff │
│ Config  : /etc/mouse-unlock.conf   root: yes   │
╰────────────────────────────────────────────────╯
╭ Actions ───────────────────────────────────────╮
│ [r] Record pattern   [c] Discard recording     │
│ [t] Edit timeout     [u] Edit unlock cmd       │
│ [k] USB factor (optional)                      │
│ [1] Save  [2] Install  [3] Save+Install  [4] …  │
│ [q] Quit                                       │
╰────────────────────────────────────────────────╯
```

- **Record your pattern by clicking** Left / Right / Middle directly in the terminal window (uses terminal mouse capture — no `/dev/input` access needed at setup time). It is saved as an Argon2 hash, so the UI shows only “configured (hidden)” afterwards.
- **`[k]` USB factor** lists your plugged-in USB devices; pick one to require it as a second factor, or disable it.
- Actions: **save config only**, **install service only**, **save + install**, or **uninstall** the service.
- Privileged actions (writing `/etc`, `systemctl`, copying the binary) need root → run with `sudo`. Without root it still lets you edit and saves the config to `./mouse-unlock.conf`.

### Configuration

`/etc/mouse-unlock.conf` (root-only, `0600`). The pattern itself is set via the setup tool and stored hashed:

```ini
pattern_hash    = $argon2id$...        # written by mouse-unlock-setup — do not edit by hand
timeout_ms      = 2000                 # max time between two clicks
unlock_cmd      = loginctl unlock-sessions
max_failures    = 5                    # wrong attempts before lockout (while locked)
lockout_base_ms = 2000                 # first lockout, doubles each further failure…
lockout_max_ms  = 300000               # …capped here (5 min)
require_usb     =                      # optional: vendor:product[:serial], empty = off
```

After editing: `sudo systemctl restart mouse-unlock`

### Test mode

Prints each attempt (and whether it matches) so you can dial in your pattern (does **not** unlock):

```bash
sudo mouse-unlock --test
```

### Monitoring

```bash
systemctl status mouse-unlock
journalctl -u mouse-unlock -f
```

## Environment support

By default it runs `loginctl unlock-sessions` (systemd-logind), which works on **KDE, GNOME, XFCE…** under both **Wayland and X11**. If your desktop needs a different command, set `unlock_cmd` in the config (it runs via `sh -c`, so chains like `cmd1 || cmd2` work).

## Uninstall

```bash
sudo systemctl disable --now mouse-unlock
sudo rm -f /etc/systemd/system/mouse-unlock.service /usr/local/bin/mouse-unlock /usr/local/bin/mouse-unlock-setup
sudo systemctl daemon-reload
# optional: sudo rm /etc/mouse-unlock.conf
```

Or from the setup UI: `sudo mouse-unlock-setup` → `[4] Uninstall service`.

## CI & releases

GitHub Actions workflows:

- **Build** (`build.yml`) — on pull requests: format check, clippy (`-D warnings`), release build, artifact upload.
- **Auto Release** (`auto-release.yml`) — on every push to `master`: lints, **auto-bumps the patch version**, updates `Cargo.toml`/`Cargo.lock`, tags `vX.Y.Z`, builds, and publishes a GitHub Release with a `.tar.gz` (binaries + service + config + README) and a `.sha256`. Add `[skip release]` to a commit message to skip a run.
- **Release** (`release.yml`) — manual path: publishes a release when you push a `v*` tag yourself.

## 🔐 Security

What this project does to harden the click-unlock:

- **No plaintext secret** — the pattern is stored as an **Argon2id hash** with a random salt, in a root-only (`0600`) config. Reading the file (or a backup) does not reveal the pattern.
- **Brute-force lockout** — after `max_failures` wrong attempts the daemon backs off exponentially. Failures are only counted **while the screen is locked**, so everyday clicking never causes an accidental lockout (and Argon2 is skipped while unlocked, keeping it light).
- **Optional USB second factor** — if `require_usb` is set, a correct pattern unlocks **only** when that device is present, defeating shoulder-surfing/replay on its own.
- The daemon runs as root (it reads raw input devices and calls `loginctl`); the systemd unit adds sandboxing (`ProtectSystem`, `NoNewPrivileges`, `MemoryMax`, …).

**Honest limitation:** a click pattern is *low-entropy and visible* — a determined onlooker or camera can still observe and replay it. The hash and lockout defend the stored secret and online guessing, but they cannot stop observation. For real security, enable the **USB second factor** and treat the pattern as convenience, not a password replacement.

## Contributing

Contributions are welcome! Please:

1. Fork and create a feature branch.
2. Keep it `cargo fmt`-clean and `cargo clippy -- -D warnings`-clean (the CI enforces both).
3. Open a pull request describing the change.

```bash
cargo build              # debug build
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

## License

[MIT](LICENSE) © 2026 Harry Nguyen

## Acknowledgements

- Inspired by [DixitRam/MouseClickUnlock](https://github.com/DixitRam/MouseClickUnlock) — the original idea and Python implementation.
- Built with [evdev](https://crates.io/crates/evdev) and [ratatui](https://crates.io/crates/ratatui).
