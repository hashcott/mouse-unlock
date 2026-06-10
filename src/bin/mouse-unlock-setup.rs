//! mouse-unlock-setup — a small terminal UI (ratatui) to configure and manage the daemon.
//!
//! Record your click pattern by clicking inside this terminal window (terminal mouse
//! capture, no /dev/input needed at setup time). The pattern is stored as an Argon2 hash
//! in a root-only (0600) config — never in plaintext. You can also pick an optional USB
//! second factor, then save the config, install the service, do both, or uninstall.
//!
//! Privileged actions (writing /etc, systemctl, copying the binary) need root,
//! so run it as:  sudo mouse-unlock-setup

use std::fs;
use std::io::{self, Stdout};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use argon2::password_hash::SaltString;
use argon2::{Algorithm, Argon2, Params, PasswordHasher, Version};
use rand_core::OsRng;

use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

const SERVICE_TEMPLATE: &str = include_str!("../../mouse-unlock.service");
const CONFIG_PATH: &str = "/etc/mouse-unlock.conf";
const DAEMON_DEST: &str = "/usr/local/bin/mouse-unlock";
const SERVICE_DEST: &str = "/etc/systemd/system/mouse-unlock.service";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Click {
    Left,
    Right,
    Middle,
}

impl Click {
    fn label(self) -> char {
        match self {
            Click::Left => 'L',
            Click::Right => 'R',
            Click::Middle => 'M',
        }
    }
    fn color(self) -> Color {
        match self {
            Click::Left => Color::Green,
            Click::Right => Color::Cyan,
            Click::Middle => Color::Yellow,
        }
    }
}

#[derive(PartialEq)]
enum Mode {
    Normal,
    Recording,
    EditTimeout,
    EditCmd,
    UsbSelect,
}

struct UsbDev {
    spec: String, // vendor:product[:serial]
    name: String,
}

struct App {
    existing_hash: Option<String>,
    new_pattern: Option<Vec<Click>>, // recorded this session
    timeout_ms: u64,
    unlock_cmd: String,
    max_failures: u32,
    lockout_base_ms: u64,
    lockout_max_ms: u64,
    require_usb: Option<String>,
    mode: Mode,
    record_buf: Vec<Click>,
    input: String,
    usb_list: Vec<UsbDev>,
    status: String,
    should_quit: bool,
}

impl App {
    fn new() -> App {
        let c = load_config();
        App {
            existing_hash: c.pattern_hash,
            new_pattern: None,
            timeout_ms: c.timeout_ms,
            unlock_cmd: c.unlock_cmd,
            max_failures: c.max_failures,
            lockout_base_ms: c.lockout_base_ms,
            lockout_max_ms: c.lockout_max_ms,
            require_usb: c.require_usb,
            mode: Mode::Normal,
            record_buf: Vec::new(),
            input: String::new(),
            usb_list: Vec::new(),
            status: String::from("ready — press [r] to record a click pattern"),
            should_quit: false,
        }
    }

    /// The Argon2 hash to write: a freshly recorded pattern, or the existing one.
    fn resolve_hash(&self) -> Result<String, String> {
        match &self.new_pattern {
            Some(p) if !p.is_empty() => {
                let s: String = p.iter().map(|c| c.label()).collect();
                hash_pattern(&s)
            }
            Some(_) => Err("recorded pattern is empty".into()),
            None => self
                .existing_hash
                .clone()
                .ok_or_else(|| "no pattern set — press [r] to record one".into()),
        }
    }

    fn render_config(&self, hash: &str) -> String {
        format!(
            "# mouse-unlock configuration (managed by mouse-unlock-setup)\n\
             # pattern_hash is an Argon2 hash of your click sequence — keep this file 0600.\n\
             \n\
             pattern_hash = {hash}\n\
             timeout_ms = {}\n\
             unlock_cmd = {}\n\
             \n\
             # Brute-force lockout (failures counted only while the screen is locked):\n\
             max_failures = {}\n\
             lockout_base_ms = {}\n\
             lockout_max_ms = {}\n\
             \n\
             # Optional second factor: only unlock if this USB device is present\n\
             # (vendor:product[:serial]). Empty = disabled.\n\
             require_usb = {}\n",
            self.timeout_ms,
            self.unlock_cmd,
            self.max_failures,
            self.lockout_base_ms,
            self.lockout_max_ms,
            self.require_usb.as_deref().unwrap_or(""),
        )
    }

    fn save_config(&mut self) -> Result<String, String> {
        let hash = self.resolve_hash()?;
        let path = config_path();
        fs::write(&path, self.render_config(&hash)).map_err(|e| format!("write {path}: {e}"))?;
        // Secret hash -> restrict to root only.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod {path}: {e}"))?;
        // The new pattern is now persisted as the existing one.
        self.existing_hash = Some(hash);
        self.new_pattern = None;
        if is_root() {
            let _ = sh("systemctl try-restart mouse-unlock.service");
        }
        Ok(format!("config saved to {path} (0600)"))
    }

    fn install_service(&self) -> Result<String, String> {
        require_root()?;
        let daemon = find_daemon_binary()
            .ok_or("daemon binary not found — run `cargo build --release` first")?;
        let dest = Path::new(DAEMON_DEST);
        let same = dest.exists() && daemon.canonicalize().ok() == dest.canonicalize().ok();
        if !same {
            fs::copy(&daemon, dest).map_err(|e| format!("copy daemon binary: {e}"))?;
        }
        fs::set_permissions(dest, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod binary: {e}"))?;
        fs::write(SERVICE_DEST, SERVICE_TEMPLATE).map_err(|e| format!("write service: {e}"))?;
        sh("systemctl daemon-reload")?;
        sh("systemctl enable --now mouse-unlock.service")?;
        Ok("service installed and started".into())
    }

    fn save_and_install(&mut self) -> Result<String, String> {
        let saved = self.save_config()?;
        let installed = self.install_service()?;
        Ok(format!("{saved}; {installed}"))
    }

    fn uninstall_service(&self) -> Result<String, String> {
        require_root()?;
        let _ = sh("systemctl disable --now mouse-unlock.service");
        let _ = fs::remove_file(SERVICE_DEST);
        let _ = sh("systemctl daemon-reload");
        let _ = fs::remove_file(DAEMON_DEST);
        Ok(format!(
            "service & binary removed (config kept at {CONFIG_PATH})"
        ))
    }
}

fn apply(app: &mut App, r: Result<String, String>) {
    app.status = match r {
        Ok(m) => format!("OK — {m}"),
        Err(e) => format!("ERROR — {e}"),
    };
}

fn main() -> io::Result<()> {
    let mut terminal = setup_terminal()?;

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        original_hook(info);
    }));

    let mut app = App::new();
    let res = run_app(&mut terminal, &mut app);
    restore_terminal()?;
    if let Err(e) = res {
        eprintln!("error: {e}");
    }
    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => on_key(app, k.code),
                Event::Mouse(m) => on_mouse(app, m.kind),
                _ => {}
            }
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

fn on_key(app: &mut App, code: KeyCode) {
    match app.mode {
        Mode::Normal => on_key_normal(app, code),
        Mode::Recording => match code {
            KeyCode::Enter => {
                if app.record_buf.is_empty() {
                    app.status = "nothing recorded".into();
                } else {
                    let n = app.record_buf.len();
                    app.new_pattern = Some(app.record_buf.clone());
                    app.status =
                        format!("recorded a {n}-click pattern (unsaved) — press [1] to save");
                }
                app.mode = Mode::Normal;
            }
            KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.status = "recording cancelled".into();
            }
            KeyCode::Backspace => {
                app.record_buf.pop();
            }
            _ => {}
        },
        Mode::EditTimeout => match code {
            KeyCode::Enter => {
                match app.input.parse::<u64>() {
                    Ok(n) if n > 0 => {
                        app.timeout_ms = n;
                        app.status = format!("timeout set to {n} ms");
                    }
                    _ => app.status = "invalid timeout (must be a positive number)".into(),
                }
                app.mode = Mode::Normal;
            }
            KeyCode::Esc => app.mode = Mode::Normal,
            KeyCode::Backspace => {
                app.input.pop();
            }
            KeyCode::Char(c) if c.is_ascii_digit() => app.input.push(c),
            _ => {}
        },
        Mode::EditCmd => match code {
            KeyCode::Enter => {
                if app.input.trim().is_empty() {
                    app.status = "unlock command cannot be empty".into();
                } else {
                    app.unlock_cmd = app.input.trim().to_string();
                    app.status = "unlock command updated".into();
                }
                app.mode = Mode::Normal;
            }
            KeyCode::Esc => app.mode = Mode::Normal,
            KeyCode::Backspace => {
                app.input.pop();
            }
            KeyCode::Char(c) => app.input.push(c),
            _ => {}
        },
        Mode::UsbSelect => match code {
            KeyCode::Esc => app.mode = Mode::Normal,
            KeyCode::Char('c') => {
                app.require_usb = None;
                app.status = "USB factor disabled".into();
                app.mode = Mode::Normal;
            }
            KeyCode::Char(d) if d.is_ascii_digit() && d != '0' => {
                let idx = (d as usize) - ('1' as usize);
                if let Some(dev) = app.usb_list.get(idx) {
                    app.require_usb = Some(dev.spec.clone());
                    app.status = format!("USB factor set: {} ({})", dev.name, dev.spec);
                    app.mode = Mode::Normal;
                }
            }
            _ => {}
        },
    }
}

fn on_key_normal(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('r') => {
            app.record_buf.clear();
            app.mode = Mode::Recording;
            app.status = "recording — click in this window; Enter=save, Esc=cancel".into();
        }
        KeyCode::Char('c') => {
            app.new_pattern = None;
            app.status = "discarded the unsaved recording".into();
        }
        KeyCode::Char('t') => {
            app.input = app.timeout_ms.to_string();
            app.mode = Mode::EditTimeout;
        }
        KeyCode::Char('u') => {
            app.input = app.unlock_cmd.clone();
            app.mode = Mode::EditCmd;
        }
        KeyCode::Char('k') => {
            app.usb_list = list_usb();
            app.mode = Mode::UsbSelect;
            app.status = "pick a USB device by number, [c] disable, [Esc] cancel".into();
        }
        KeyCode::Char('1') => {
            let r = app.save_config();
            apply(app, r);
        }
        KeyCode::Char('2') => {
            let r = app.install_service();
            apply(app, r);
        }
        KeyCode::Char('3') => {
            let r = app.save_and_install();
            apply(app, r);
        }
        KeyCode::Char('4') => {
            let r = app.uninstall_service();
            apply(app, r);
        }
        _ => {}
    }
}

fn on_mouse(app: &mut App, kind: MouseEventKind) {
    if app.mode != Mode::Recording {
        return;
    }
    let click = match kind {
        MouseEventKind::Down(MouseButton::Left) => Click::Left,
        MouseEventKind::Down(MouseButton::Right) => Click::Right,
        MouseEventKind::Down(MouseButton::Middle) => Click::Middle,
        _ => return,
    };
    app.record_buf.push(click);
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(11),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(f.area());

    f.render_widget(
        Paragraph::new(info_lines(app)).block(block(" Mouse Unlock — Setup ")),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(action_lines(app))
            .block(block(" Actions "))
            .wrap(Wrap { trim: true }),
        chunks[1],
    );
    f.render_widget(
        Paragraph::new(app.status.clone())
            .block(block(" Status "))
            .wrap(Wrap { trim: true }),
        chunks[2],
    );
}

fn block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))
}

fn label(text: &str) -> Span<'static> {
    Span::styled(
        text.to_string(),
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )
}

fn click_spans(clicks: &[Click]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, c) in clicks.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            c.label().to_string(),
            Style::default().fg(c.color()).add_modifier(Modifier::BOLD),
        ));
    }
    spans
}

fn info_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    // Pattern row.
    let mut pat = vec![label("Pattern : ")];
    if app.mode == Mode::Recording {
        if app.record_buf.is_empty() {
            pat.push(Span::styled(
                "(recording…)",
                Style::default().fg(Color::Red),
            ));
        } else {
            pat.extend(click_spans(&app.record_buf));
        }
    } else if let Some(p) = &app.new_pattern {
        pat.extend(click_spans(p));
        pat.push(Span::styled(
            "  (new, unsaved)",
            Style::default().fg(Color::Yellow),
        ));
    } else if app.existing_hash.is_some() {
        pat.push(Span::styled(
            "configured (hidden, hashed)",
            Style::default().fg(Color::Green),
        ));
    } else {
        pat.push(Span::styled(
            "(not set)",
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(pat));

    lines.push(Line::from(vec![
        label("Timeout : "),
        Span::raw(format!("{} ms", app.timeout_ms)),
    ]));
    lines.push(Line::from(vec![
        label("Unlock  : "),
        Span::raw(app.unlock_cmd.clone()),
    ]));
    lines.push(Line::from(vec![
        label("USB 2FA : "),
        match &app.require_usb {
            Some(s) => Span::styled(s.clone(), Style::default().fg(Color::Green)),
            None => Span::styled("(none — optional)", Style::default().fg(Color::DarkGray)),
        },
    ]));
    lines.push(Line::from(vec![
        label("Lockout : "),
        Span::raw(format!(
            "after {} fails, {}–{} ms backoff",
            app.max_failures, app.lockout_base_ms, app.lockout_max_ms
        )),
    ]));

    let root = if is_root() {
        Span::styled("root: yes", Style::default().fg(Color::Green))
    } else {
        Span::styled(
            "root: no (sudo needed to install)",
            Style::default().fg(Color::Yellow),
        )
    };
    lines.push(Line::from(vec![
        label("Config  : "),
        Span::raw(config_display()),
        Span::raw("   "),
        root,
    ]));

    match app.mode {
        Mode::EditTimeout => lines.push(Line::from(vec![
            Span::styled("New timeout (ms): ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}_", app.input)),
        ])),
        Mode::EditCmd => lines.push(Line::from(vec![
            Span::styled("New unlock cmd: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!("{}_", app.input)),
        ])),
        _ => {}
    }

    lines
}

fn key(k: &str) -> Span<'static> {
    Span::styled(
        format!("[{k}]"),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )
}

fn action_lines(app: &App) -> Vec<Line<'static>> {
    match app.mode {
        Mode::Recording => vec![
            Line::from("Click in this window to add:  Left=L   Right=R   Middle=M"),
            Line::from(vec![
                key("Backspace"),
                Span::raw(" undo   "),
                key("Enter"),
                Span::raw(" save   "),
                key("Esc"),
                Span::raw(" cancel"),
            ]),
        ],
        Mode::EditTimeout | Mode::EditCmd => vec![
            Line::from("Type a value, then confirm."),
            Line::from(vec![
                key("Enter"),
                Span::raw(" confirm   "),
                key("Esc"),
                Span::raw(" cancel   "),
                key("Backspace"),
                Span::raw(" delete"),
            ]),
        ],
        Mode::UsbSelect => {
            let mut lines = vec![Line::from(Span::styled(
                "Select a USB device as the second factor:",
                Style::default().add_modifier(Modifier::BOLD),
            ))];
            if app.usb_list.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  (no USB devices detected)",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                for (i, dev) in app.usb_list.iter().take(9).enumerate() {
                    lines.push(Line::from(vec![
                        key(&(i + 1).to_string()),
                        Span::raw(format!(" {}  ", dev.name)),
                        Span::styled(dev.spec.clone(), Style::default().fg(Color::DarkGray)),
                    ]));
                }
            }
            lines.push(Line::from(vec![
                key("c"),
                Span::raw(" disable USB factor   "),
                key("Esc"),
                Span::raw(" cancel"),
            ]));
            lines
        }
        Mode::Normal => vec![
            Line::from(vec![
                key("r"),
                Span::raw(" Record pattern     "),
                key("c"),
                Span::raw(" Discard recording"),
            ]),
            Line::from(vec![
                key("t"),
                Span::raw(" Edit timeout       "),
                key("u"),
                Span::raw(" Edit unlock cmd"),
            ]),
            Line::from(vec![key("k"), Span::raw(" USB factor (optional)")]),
            Line::from(""),
            Line::from(vec![
                key("1"),
                Span::raw(" Save   "),
                key("2"),
                Span::raw(" Install   "),
                key("3"),
                Span::raw(" Save+Install   "),
                key("4"),
                Span::raw(" Uninstall"),
            ]),
            Line::from(vec![key("q"), Span::raw(" Quit")]),
        ],
    }
}

// ---------------------------------------------------------------------------
// Crypto / USB / system helpers
// ---------------------------------------------------------------------------

fn hash_pattern(pattern: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    // Modest params: ~8 MiB, t=3 — protects the low-entropy secret without much memory.
    let params = Params::new(8 * 1024, 3, 1, None).map_err(|e| e.to_string())?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    argon
        .hash_password(pattern.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn list_usb() -> Vec<UsbDev> {
    let mut devs = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") else {
        return devs;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let (Some(vendor), Some(product)) =
            (read_sys(&dir, "idVendor"), read_sys(&dir, "idProduct"))
        else {
            continue; // interface node, not a device
        };
        if vendor == "1d6b" {
            continue; // Linux Foundation root hubs
        }
        let serial = read_sys(&dir, "serial");
        let spec = match &serial {
            Some(s) => format!("{vendor}:{product}:{s}"),
            None => format!("{vendor}:{product}"),
        };
        let name = read_sys(&dir, "product")
            .or_else(|| read_sys(&dir, "manufacturer"))
            .unwrap_or_else(|| "unknown device".into());
        devs.push(UsbDev { spec, name });
    }
    devs
}

fn read_sys(dir: &Path, file: &str) -> Option<String> {
    fs::read_to_string(dir.join(file))
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Terminal / config plumbing
// ---------------------------------------------------------------------------

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn require_root() -> Result<(), String> {
    if is_root() {
        Ok(())
    } else {
        Err("needs root — re-run with: sudo mouse-unlock-setup".into())
    }
}

/// The actual file we read from / write to.
fn config_path() -> String {
    if is_root() {
        CONFIG_PATH.to_string()
    } else {
        "mouse-unlock.conf".to_string()
    }
}

/// A human-readable label for the config location (never used as a path).
fn config_display() -> String {
    if is_root() {
        CONFIG_PATH.to_string()
    } else {
        "mouse-unlock.conf (cwd, not root)".to_string()
    }
}

fn sh(cmd: &str) -> Result<(), String> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{cmd}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn find_daemon_binary() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("mouse-unlock"));
        }
    }
    candidates.push(PathBuf::from("target/release/mouse-unlock"));
    candidates.push(PathBuf::from("target/debug/mouse-unlock"));
    candidates.push(PathBuf::from(DAEMON_DEST));
    candidates.into_iter().find(|p| p.is_file())
}

struct LoadedConfig {
    pattern_hash: Option<String>,
    timeout_ms: u64,
    unlock_cmd: String,
    max_failures: u32,
    lockout_base_ms: u64,
    lockout_max_ms: u64,
    require_usb: Option<String>,
}

fn load_config() -> LoadedConfig {
    let mut c = LoadedConfig {
        pattern_hash: None,
        timeout_ms: 2000,
        unlock_cmd: "loginctl unlock-sessions".into(),
        max_failures: 5,
        lockout_base_ms: 2000,
        lockout_max_ms: 300_000,
        require_usb: None,
    };

    if let Ok(content) = fs::read_to_string(CONFIG_PATH) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                match k {
                    "pattern_hash" if !v.is_empty() => c.pattern_hash = Some(v.to_string()),
                    "timeout_ms" => {
                        if let Ok(n) = v.parse() {
                            c.timeout_ms = n;
                        }
                    }
                    "unlock_cmd" if !v.is_empty() => c.unlock_cmd = v.to_string(),
                    "max_failures" => {
                        if let Ok(n) = v.parse() {
                            c.max_failures = n;
                        }
                    }
                    "lockout_base_ms" => {
                        if let Ok(n) = v.parse() {
                            c.lockout_base_ms = n;
                        }
                    }
                    "lockout_max_ms" => {
                        if let Ok(n) = v.parse() {
                            c.lockout_max_ms = n;
                        }
                    }
                    "require_usb" if !v.is_empty() => c.require_usb = Some(v.to_string()),
                    _ => {}
                }
            }
        }
    }
    c
}
