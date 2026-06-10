//! mouse-unlock — ultra-light daemon that unlocks the Linux screen with a mouse-click pattern.
//!
//! How it works:
//!  - Reads raw mouse events from /dev/input/* via evdev (blocking read() -> 0% CPU when idle).
//!  - A "secret" is a sequence of left/right/middle clicks. The secret is NOT stored in
//!    plaintext: the config holds an Argon2 hash, and the file is root-only (0600).
//!  - An attempt = the clicks entered before a pause (> timeout). On each attempt the daemon
//!    hashes the entered sequence and compares it to the stored hash.
//!  - On a match it runs the unlock command (default: `loginctl unlock-sessions`, which works
//!    on KDE/GNOME/XFCE under both Wayland and X11).
//!
//! Hardening:
//!  - Brute-force lockout with exponential backoff after too many wrong attempts.
//!  - Failures are only counted while the screen is actually locked, so normal clicking
//!    while unlocked never triggers a lockout (and Argon2 is skipped when unlocked).
//!  - Optional second factor: only unlock when a configured USB device is present.
//!
//! Security note: the click sequence can still be observed and replayed by onlookers.
//! Treat it as convenience + the optional USB factor, not as a strong password.

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use argon2::{Argon2, PasswordHash, PasswordVerifier};
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

/// Upper bound on how many clicks we keep, so a long click storm can't grow memory.
const MAX_BUF: usize = 64;

struct Config {
    pattern_hash: String,
    timeout: Duration,
    unlock_cmd: String,
    max_failures: u32,
    lockout_base_ms: u64,
    lockout_max_ms: u64,
    require_usb: Option<String>,
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
    eprintln!(
        "[mouse-unlock] starting | timeout={}ms | max_failures={} | usb_factor={} | test={}",
        config.timeout.as_millis(),
        config.max_failures,
        config.require_usb.is_some(),
        test_mode
    );
    if config.pattern_hash.is_empty() {
        eprintln!(
            "[mouse-unlock] WARNING: no pattern configured. \
             Run `sudo mouse-unlock-setup` to set one, then restart the service."
        );
    }

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

    run_matcher(&config, test_mode, rx);
}

/// Collect clicks into attempts (separated by a > timeout pause) and act on each attempt.
fn run_matcher(config: &Config, test_mode: bool, rx: mpsc::Receiver<Click>) {
    let mut buf: VecDeque<Click> = VecDeque::with_capacity(MAX_BUF);
    let mut last_click = Instant::now();
    let mut failures: u32 = 0;
    let mut lockout_until = Instant::now();

    loop {
        if buf.is_empty() {
            // Nothing pending: block until the next click.
            match rx.recv() {
                Ok(c) => {
                    buf.push_back(c);
                    last_click = Instant::now();
                }
                Err(_) => return, // all senders gone
            }
            continue;
        }

        let elapsed = last_click.elapsed();
        if elapsed >= config.timeout {
            finalize_attempt(
                config,
                test_mode,
                &mut buf,
                &mut failures,
                &mut lockout_until,
            );
            continue;
        }

        match rx.recv_timeout(config.timeout - elapsed) {
            Ok(c) => {
                if buf.len() >= MAX_BUF {
                    buf.pop_front();
                }
                buf.push_back(c);
                last_click = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {
                finalize_attempt(
                    config,
                    test_mode,
                    &mut buf,
                    &mut failures,
                    &mut lockout_until,
                );
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn finalize_attempt(
    config: &Config,
    test_mode: bool,
    buf: &mut VecDeque<Click>,
    failures: &mut u32,
    lockout_until: &mut Instant,
) {
    let candidate: String = buf.iter().map(|c| c.label()).collect();
    buf.clear();

    if candidate.is_empty() || config.pattern_hash.is_empty() {
        return;
    }

    if test_mode {
        eprintln!("[mouse-unlock] (test) attempt = {candidate}");
        if verify_pattern(&config.pattern_hash, &candidate) {
            eprintln!("[mouse-unlock] (test) >>> pattern MATCHES");
        }
        return;
    }

    let now = Instant::now();
    if now < *lockout_until {
        let remaining = (*lockout_until - now).as_secs() + 1;
        eprintln!("[mouse-unlock] locked out (~{remaining}s left); ignoring attempt");
        return;
    }

    // Lock state is only used to gate brute-force counting. We do NOT skip unlocking
    // when it reports "unlocked": some desktops (e.g. Cinnamon) don't update logind's
    // LockedHint, and running the unlock command while already unlocked is harmless.
    let lock = screen_locked();

    if verify_pattern(&config.pattern_hash, &candidate) {
        *failures = 0;
        if let Some(spec) = &config.require_usb {
            if !usb_present(spec) {
                eprintln!(
                    "[mouse-unlock] pattern OK but required USB ({spec}) not present; refusing"
                );
                return;
            }
        }
        eprintln!("[mouse-unlock] >>> pattern matched, unlocking (locked={lock:?})");
        run_unlock(&config.unlock_cmd);
    } else if lock == Some(true) {
        *failures += 1;
        eprintln!("[mouse-unlock] wrong attempt ({} total)", failures);
        if *failures >= config.max_failures {
            let dur = lockout_duration(
                *failures,
                config.max_failures,
                config.lockout_base_ms,
                config.lockout_max_ms,
            );
            *lockout_until = now + dur;
            eprintln!(
                "[mouse-unlock] too many failures; locked out for {}s",
                dur.as_secs()
            );
        }
    }
}

fn lockout_duration(failures: u32, max_failures: u32, base_ms: u64, max_ms: u64) -> Duration {
    let over = failures.saturating_sub(max_failures).min(20);
    let mult = 1u64 << over; // 2^over, bounded by the min(20) above
    Duration::from_millis(base_ms.saturating_mul(mult).min(max_ms))
}

fn verify_pattern(hash: &str, candidate: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(candidate.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

fn run_unlock(cmd: &str) {
    // Run via `sh -c` to allow command chains / fallbacks in the config.
    match Command::new("sh").arg("-c").arg(cmd).status() {
        Ok(s) => eprintln!("[mouse-unlock] unlock command exited: {s}"),
        Err(e) => eprintln!("[mouse-unlock] failed to run unlock command: {e}"),
    }
}

/// Returns Some(true)/Some(false) if logind reports lock state, None if undetectable.
fn screen_locked() -> Option<bool> {
    let out = Command::new("loginctl")
        .args(["list-sessions", "--no-legend"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut seen = false;
    let mut locked = false;
    for line in text.lines() {
        if let Some(id) = line.split_whitespace().next() {
            if let Some(hint) = session_locked(id) {
                seen = true;
                locked |= hint;
            }
        }
    }
    if seen {
        Some(locked)
    } else {
        None
    }
}

fn session_locked(id: &str) -> Option<bool> {
    let out = Command::new("loginctl")
        .args(["show-session", id, "-p", "LockedHint", "--value"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

/// Is a USB device matching `spec` ("vendor:product" or "vendor:product:serial") plugged in?
fn usb_present(spec: &str) -> bool {
    let mut parts = spec.split(':');
    let (Some(want_vendor), Some(want_product)) = (parts.next(), parts.next()) else {
        return false;
    };
    let want_serial = parts.next();
    let want_vendor = want_vendor.trim().to_ascii_lowercase();
    let want_product = want_product.trim().to_ascii_lowercase();

    let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") else {
        return false;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let vendor = read_sys(&dir, "idVendor");
        let product = read_sys(&dir, "idProduct");
        if vendor.as_deref() == Some(want_vendor.as_str())
            && product.as_deref() == Some(want_product.as_str())
        {
            match want_serial {
                None => return true,
                Some(s) => {
                    let want = s.trim().to_ascii_lowercase();
                    if read_sys(&dir, "serial").as_deref() == Some(want.as_str()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn read_sys(dir: &std::path::Path, file: &str) -> Option<String> {
    fs::read_to_string(dir.join(file))
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
}

fn load_config(path: &str) -> Config {
    let mut pattern_hash = String::new();
    let mut timeout = Duration::from_millis(2000);
    let mut unlock_cmd = String::from("loginctl unlock-sessions");
    let mut max_failures = 5u32;
    let mut lockout_base_ms = 2000u64;
    let mut lockout_max_ms = 300_000u64;
    let mut require_usb: Option<String> = None;

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
                        "pattern_hash" => pattern_hash = v.to_string(),
                        "timeout_ms" => {
                            if let Ok(ms) = v.parse::<u64>() {
                                timeout = Duration::from_millis(ms);
                            }
                        }
                        "unlock_cmd" if !v.is_empty() => unlock_cmd = v.to_string(),
                        "max_failures" => {
                            if let Ok(n) = v.parse::<u32>() {
                                max_failures = n.max(1);
                            }
                        }
                        "lockout_base_ms" => {
                            if let Ok(n) = v.parse::<u64>() {
                                lockout_base_ms = n;
                            }
                        }
                        "lockout_max_ms" => {
                            if let Ok(n) = v.parse::<u64>() {
                                lockout_max_ms = n;
                            }
                        }
                        "require_usb" => {
                            require_usb = if v.is_empty() {
                                None
                            } else {
                                Some(v.to_string())
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Err(_) => eprintln!("[mouse-unlock] {path} not found, using defaults"),
    }

    Config {
        pattern_hash,
        timeout,
        unlock_cmd,
        max_failures,
        lockout_base_ms,
        lockout_max_ms,
        require_usb,
    }
}

fn print_help() {
    println!(
        "mouse-unlock — unlock the Linux screen with a mouse-click pattern\n\n\
         USAGE:\n  \
           mouse-unlock [--config <path>] [--test]\n\n\
         OPTIONS:\n  \
           -c, --config <path>  Config file path (default /etc/mouse-unlock.conf)\n  \
           -t, --test           Print attempts (and whether they match), do NOT unlock\n  \
           -h, --help           Show this help\n\n\
         Set your pattern with:  sudo mouse-unlock-setup"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::password_hash::SaltString;
    use argon2::PasswordHasher;

    fn hash(pattern: &str) -> String {
        // Fixed salt ("abcdefghijklmnop") so the test needs no RNG.
        let salt = SaltString::from_b64("YWJjZGVmZ2hpamtsbW5vcA").unwrap();
        Argon2::default()
            .hash_password(pattern.as_bytes(), &salt)
            .unwrap()
            .to_string()
    }

    #[test]
    fn pattern_roundtrip() {
        let h = hash("LLRRL");
        assert!(verify_pattern(&h, "LLRRL"));
        assert!(!verify_pattern(&h, "LLRRR"));
        assert!(!verify_pattern(&h, "LLRR"));
        assert!(!verify_pattern("not-a-valid-hash", "LLRRL"));
    }

    #[test]
    fn lockout_grows_then_caps() {
        assert_eq!(lockout_duration(5, 5, 2000, 300_000).as_millis(), 2000);
        assert_eq!(lockout_duration(6, 5, 2000, 300_000).as_millis(), 4000);
        assert_eq!(lockout_duration(7, 5, 2000, 300_000).as_millis(), 8000);
        assert_eq!(lockout_duration(50, 5, 2000, 300_000).as_millis(), 300_000);
    }
}
