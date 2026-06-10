# mouse-unlock

An **ultra-light** daemon that unlocks the Linux screen with a **secret mouse-click pattern**, written in **Rust**.

## Why is it light?

- **Event-driven**: reads `/dev/input/*` with a blocking `read()` → the process **sleeps at 0% CPU** until a click arrives. No polling, no wasted power.
- **No runtime / no GC**: a static Rust binary, ~1–2 MB resident RAM (vs ~15–30 MB for Python).
- The release profile is size-optimized (`opt-level="z"`, `lto`, `strip`, `panic=abort`).

## Environment support

By default it uses `loginctl unlock-sessions` (systemd-logind) → works on **KDE, GNOME, XFCE…** under both **Wayland and X11**. Change `unlock_cmd` in the config if your desktop needs a different command.

## Install

```bash
git clone <repo> && cd noname
sudo bash install.sh
```

Requires [Rust/cargo](https://rustup.rs) to build. This builds and installs **two** binaries:

- `mouse-unlock` — the lightweight background daemon (auto-starts at boot).
- `mouse-unlock-setup` — a terminal UI to configure and manage it.

## Setup UI (`mouse-unlock-setup`)

A small terminal interface (ratatui) for configuring everything — no need to edit files by hand:

```bash
sudo mouse-unlock-setup
```

```
╭ Mouse Unlock — Setup ──────────────────────────╮
│ Pattern : L L R R L                            │
│ Timeout : 2000 ms                              │
│ Unlock  : loginctl unlock-sessions             │
│ Config  : /etc/mouse-unlock.conf   root: yes   │
╰────────────────────────────────────────────────╯
╭ Actions ───────────────────────────────────────╮
│ [r] Record pattern     [c] Clear pattern       │
│ [t] Edit timeout       [u] Edit unlock cmd     │
│ [1] Save config        [2] Install service     │
│ [3] Save + Install     [4] Uninstall service   │
│ [q] Quit                                       │
╰────────────────────────────────────────────────╯
```

- **Record your pattern by clicking** Left / Right / Middle directly in the terminal window (uses terminal mouse capture — no `/dev/input` access needed at setup time).
- Actions: **save config only**, **install service only**, **save + install**, or **uninstall** the service.
- Privileged actions (writing `/etc`, `systemctl`, copying the binary) need root → run with `sudo`. Without root it still lets you edit and saves the config to `./mouse-unlock.conf` in the current directory.

## Configuration — `/etc/mouse-unlock.conf`

```ini
pattern    = LLRRL                    # L=left, R=right, M=middle
timeout_ms = 2000                     # max time between two clicks
unlock_cmd = loginctl unlock-sessions # command to run on match
```

After editing: `sudo systemctl restart mouse-unlock`

## Test mode (does not actually unlock)

```bash
sudo ./target/release/mouse-unlock --test
```

Prints the click buffer in real time so you can dial in your pattern.

## Monitoring

```bash
systemctl status mouse-unlock
journalctl -u mouse-unlock -f
```

## CI & releases

GitHub Actions workflows:

- **Build** (`build.yml`) — runs on pull requests: format check, clippy (`-D warnings`), release build, artifact upload.
- **Auto Release** (`auto-release.yml`) — on every push to `master`: lints, **auto-bumps the patch version**, updates `Cargo.toml`/`Cargo.lock`, tags `vX.Y.Z`, builds, and publishes a GitHub Release with a `.tar.gz` (binaries + service + config + README) and a `.sha256`. Add `[skip release]` to a commit message to skip a run.
- **Release** (`release.yml`) — manual path: publishes a release when you push a `v*` tag yourself (or via *Run workflow*).

The first auto-release uses the version currently in `Cargo.toml`; each subsequent push to `master` bumps the patch number.

## ⚠️ Security warning

The click sequence can be **observed and replayed** by onlookers. This is a **convenience** tool, NOT a strong security mechanism. Use it on a personal machine; do not treat it as a password replacement in sensitive environments.

## License

MIT
