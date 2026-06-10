//! mouse-unlock — ultra-light daemon that unlocks the Linux screen with a mouse-click pattern.
//!
//! How it works:
//!  - Reads raw mouse events from /dev/input/* via evdev (blocking read() -> 0% CPU when idle).
//!  - Matches a secret click sequence (e.g. L L R R L).
//!  - On match -> runs the unlock command (default: `loginctl unlock-sessions`,
//!    which works on KDE/GNOME/XFCE... under both Wayland and X11).
//!
//! Security note: the click sequence can be observed and replayed by onlookers.
//! This is a convenience tool, NOT a strong security mechanism.

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use evdev::{Device, InputEventKind, Key};

/// A single mouse click type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Click {
    Left,
    Right,
    Middle,
}

impl Click {
    fn from_key(k: Key) -> Option<Click> {
        match k {
            Key::BTN_LEFT => Some(Click::Left),
            Key::BTN_RIGHT => Some(Click::Right),
            Key::BTN_MIDDLE => Some(Click::Middle),
            _ => None,
        }
    }

    fn label(self) -> char {
        match self {
            Click::Left => 'L',
            Click::Right => 'R',
            Click::Middle => 'M',
        }
    }
}

struct Config {
    pattern: Vec<Click>,
    timeout: Duration,
    unlock_cmd: String,
}

fn main() {
    let mut config_path = String::from("/etc/mouse-unlock.conf");
    let mut test_mode = false;

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--test" | "-t" => test_mode = true,
            "--config" | "-c" => {
                i += 1;
                if i < args.len() {
                    config_path = args[i].clone();
                }
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            other => eprintln!("[mouse-unlock] ignoring unknown argument: {other}"),
        }
        i += 1;
    }

    let config = load_config(&config_path);
    if config.pattern.is_empty() {
        eprintln!("[mouse-unlock] ERROR: empty pattern. Check {config_path}");
        std::process::exit(1);
    }

    let pattern_str: String = config.pattern.iter().map(|c| c.label()).collect();
    eprintln!(
        "[mouse-unlock] starting | pattern={} | timeout={}ms | test={}",
        pattern_str,
        config.timeout.as_millis(),
        test_mode
    );

    // Find every device that has mouse buttons (external mice + touchpads).
    let mut mice: Vec<(PathBuf, Device)> = Vec::new();
    for (path, dev) in evdev::enumerate() {
        if dev
            .supported_keys()
            .is_some_and(|keys| keys.contains(Key::BTN_LEFT))
        {
            mice.push((path, dev));
        }
    }

    if mice.is_empty() {
        eprintln!(
            "[mouse-unlock] ERROR: no mouse devices found.\n\
             Run as root (or be in the 'input' group) to read /dev/input/*."
        );
        std::process::exit(1);
    }

    // One blocking-read thread per device (sleeps until a click -> no CPU usage).
    let (tx, rx) = mpsc::channel::<Click>();
    for (path, mut dev) in mice {
        eprintln!("[mouse-unlock] listening on: {}", path.display());
        let tx = tx.clone();
        thread::spawn(move || loop {
            match dev.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        // Only consider button PRESS events (value == 1).
                        if ev.value() == 1 {
                            if let InputEventKind::Key(k) = ev.kind() {
                                if let Some(c) = Click::from_key(k) {
                                    if tx.send(c).is_err() {
                                        return; // matcher has exited
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[mouse-unlock] device {} error: {e}", path.display());
                    return;
                }
            }
        });
    }
    drop(tx); // only the threads keep tx; main keeps rx

    // Matching loop (sliding window the size of the pattern).
    let plen = config.pattern.len();
    let mut buf: VecDeque<Click> = VecDeque::with_capacity(plen);
    let mut last = Instant::now();

    for click in rx {
        let now = Instant::now();
        if now.duration_since(last) > config.timeout {
            buf.clear(); // too long between clicks -> start over
        }
        last = now;

        if buf.len() == plen {
            buf.pop_front();
        }
        buf.push_back(click);

        if test_mode {
            let s: String = buf.iter().map(|c| c.label()).collect();
            eprintln!("[mouse-unlock] buffer = {s}");
        }

        if buf.len() == plen && buf.iter().copied().eq(config.pattern.iter().copied()) {
            eprintln!("[mouse-unlock] >>> PATTERN MATCHED");
            if test_mode {
                eprintln!("[mouse-unlock] (test) would run: {}", config.unlock_cmd);
            } else {
                run_unlock(&config.unlock_cmd);
            }
            buf.clear();
        }
    }
}

fn run_unlock(cmd: &str) {
    // Run via `sh -c` to allow command chains / fallbacks in the config.
    match Command::new("sh").arg("-c").arg(cmd).status() {
        Ok(s) => eprintln!("[mouse-unlock] unlock command exited: {s}"),
        Err(e) => eprintln!("[mouse-unlock] failed to run unlock command: {e}"),
    }
}

fn load_config(path: &str) -> Config {
    let mut pattern = parse_pattern("LLRRL");
    let mut timeout = Duration::from_millis(2000);
    let mut unlock_cmd = String::from("loginctl unlock-sessions");

    match fs::read_to_string(path) {
        Ok(content) => {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let (k, v) = (k.trim(), v.trim());
                    match k {
                        "pattern" => {
                            let p = parse_pattern(v);
                            if !p.is_empty() {
                                pattern = p;
                            }
                        }
                        "timeout_ms" => {
                            if let Ok(ms) = v.parse::<u64>() {
                                timeout = Duration::from_millis(ms);
                            }
                        }
                        "unlock_cmd" => {
                            if !v.is_empty() {
                                unlock_cmd = v.to_string();
                            }
                        }
                        _ => eprintln!("[mouse-unlock] unknown config key: {k}"),
                    }
                }
            }
        }
        Err(_) => eprintln!("[mouse-unlock] {path} not found, using defaults"),
    }

    Config {
        pattern,
        timeout,
        unlock_cmd,
    }
}

/// Parse a string like "LLRRL" or "L,R,M" into Vec<Click> (separators are ignored).
fn parse_pattern(s: &str) -> Vec<Click> {
    s.chars()
        .filter_map(|c| match c.to_ascii_uppercase() {
            'L' => Some(Click::Left),
            'R' => Some(Click::Right),
            'M' => Some(Click::Middle),
            _ => None,
        })
        .collect()
}

fn print_help() {
    println!(
        "mouse-unlock — unlock the Linux screen with a mouse-click pattern\n\n\
         USAGE:\n  \
           mouse-unlock [--config <path>] [--test]\n\n\
         OPTIONS:\n  \
           -c, --config <path>  Config file path (default /etc/mouse-unlock.conf)\n  \
           -t, --test           Print the click buffer, do NOT actually unlock\n  \
           -h, --help           Show this help\n\n\
         CONFIG (key = value):\n  \
           pattern     = LLRRL                    (L=left, R=right, M=middle)\n  \
           timeout_ms  = 2000                     (max time between two clicks)\n  \
           unlock_cmd  = loginctl unlock-sessions (command to run on match)"
    );
}
