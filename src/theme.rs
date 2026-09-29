//! Light and dark dashboard palettes.
//!
//! `auto` follows the same signals Claude Code, Codex, and Cursor Agent use:
//! the terminal's reported background first, then the operating system
//! appearance when the terminal does not answer.

use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use ratatui::style::Color;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ThemePreference {
    /// Follow the terminal background, then the operating system appearance.
    Auto,
    /// Always use the dark palette.
    Dark,
    /// Always use the light palette.
    Light,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorScheme {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Palette {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub selected_bg: Color,
    pub selected_fg: Color,
    pub accent: Color,
    pub attention: Color,
    pub complete: Color,
}

impl Palette {
    pub const fn dark() -> Self {
        Self {
            bg: Color::Rgb(24, 26, 27),
            fg: Color::Rgb(205, 205, 205),
            dim: Color::Rgb(145, 145, 145),
            selected_bg: Color::Rgb(58, 60, 61),
            selected_fg: Color::White,
            accent: Color::Rgb(89, 194, 201),
            attention: Color::Rgb(232, 191, 72),
            complete: Color::Rgb(101, 187, 120),
        }
    }

    /// Neutral white with the greys and blue of VS Code's default light theme,
    /// so the dashboard matches a light editor instead of tinting warm. Every
    /// foreground keeps WCAG AA contrast (4.5:1) on both backgrounds, since
    /// dim, accent, and state spans also sit on the selected row.
    pub const fn light() -> Self {
        Self {
            bg: Color::Rgb(255, 255, 255),
            fg: Color::Rgb(59, 59, 59),
            dim: Color::Rgb(102, 102, 102),
            selected_bg: Color::Rgb(232, 232, 232),
            selected_fg: Color::Rgb(30, 30, 30),
            accent: Color::Rgb(0, 95, 184),
            attention: Color::Rgb(135, 90, 0),
            complete: Color::Rgb(36, 112, 36),
        }
    }

    pub fn for_scheme(scheme: ColorScheme) -> Self {
        match scheme {
            ColorScheme::Dark => Self::dark(),
            ColorScheme::Light => Self::light(),
        }
    }
}

std::thread_local! {
    static ACTIVE: std::cell::Cell<Palette> = const { std::cell::Cell::new(Palette::dark()) };
}

pub fn set_active_palette(palette: Palette) {
    ACTIVE.with(|slot| slot.set(palette));
}

pub fn active_palette() -> Palette {
    ACTIVE.with(|slot| slot.get())
}

pub fn resolve(preference: ThemePreference) -> ColorScheme {
    match preference {
        ThemePreference::Dark => ColorScheme::Dark,
        ThemePreference::Light => ColorScheme::Light,
        ThemePreference::Auto => detect_color_scheme().unwrap_or(ColorScheme::Dark),
    }
}

/// Follows operating-system appearance changes while the dashboard runs.
///
/// The terminal cannot be re-asked for its background once the event loop
/// owns stdin, so the watcher polls the OS appearance instead and reports only
/// changes. The scheme chosen at startup therefore stands until the user
/// actually switches appearance, even when a dark terminal sits on a light
/// desktop.
pub struct SchemeWatcher {
    changes: mpsc::Receiver<ColorScheme>,
    stop: Arc<AtomicBool>,
}

impl SchemeWatcher {
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    /// Starts polling for `auto`; explicit preferences never change.
    pub fn spawn(preference: ThemePreference) -> Option<Self> {
        if preference != ThemePreference::Auto {
            return None;
        }
        let (tx, changes) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        thread::Builder::new()
            .name("theme-watcher".into())
            .spawn(move || {
                let mut previous = os_color_scheme();
                while !stop_flag.load(Ordering::Relaxed) {
                    thread::sleep(Self::POLL_INTERVAL);
                    if let Some(scheme) = scheme_change(&mut previous, os_color_scheme()) {
                        if tx.send(scheme).is_err() {
                            break;
                        }
                    }
                }
            })
            .ok()?;
        Some(Self { changes, stop })
    }

    /// The most recent appearance change since the last call, if any.
    pub fn take_change(&self) -> Option<ColorScheme> {
        let mut latest = None;
        while let Ok(scheme) = self.changes.try_recv() {
            latest = Some(scheme);
        }
        latest
    }
}

impl Drop for SchemeWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Reports `observed` only when it is a real reading that differs from the
/// last real reading. A failed probe neither changes the theme nor forgets
/// what was seen before.
fn scheme_change(
    previous: &mut Option<ColorScheme>,
    observed: Option<ColorScheme>,
) -> Option<ColorScheme> {
    let scheme = observed?;
    if *previous == Some(scheme) {
        return None;
    }
    let first_reading = previous.is_none();
    *previous = Some(scheme);
    if first_reading {
        None
    } else {
        Some(scheme)
    }
}

/// Terminal background, then `COLORFGBG`, then the OS appearance.
pub fn detect_color_scheme() -> Option<ColorScheme> {
    if let Some((red, green, blue)) = query_terminal_background() {
        return Some(scheme_from_rgb(red, green, blue));
    }
    if let Some(scheme) = scheme_from_colorfgbg(std::env::var("COLORFGBG").ok().as_deref()) {
        return Some(scheme);
    }
    os_color_scheme()
}

pub fn scheme_from_rgb(red: u8, green: u8, blue: u8) -> ColorScheme {
    let luminance =
        (0.299 * f32::from(red) + 0.587 * f32::from(green) + 0.114 * f32::from(blue)) / 255.0;
    if luminance > 0.5 {
        ColorScheme::Light
    } else {
        ColorScheme::Dark
    }
}

/// rxvt-style `COLORFGBG=foreground;background`, using the last color index.
pub fn scheme_from_colorfgbg(value: Option<&str>) -> Option<ColorScheme> {
    let index: u8 = value?.split(';').next_back()?.trim().parse().ok()?;
    match index {
        0..=6 | 8 => Some(ColorScheme::Dark),
        7 | 15 => Some(ColorScheme::Light),
        _ => None,
    }
}

#[cfg(any(test, target_os = "macos"))]
pub(crate) fn scheme_from_apple_interface_style(stdout: &str, stderr: &str) -> Option<ColorScheme> {
    if stdout.trim().eq_ignore_ascii_case("Dark") {
        Some(ColorScheme::Dark)
    } else if stderr.to_ascii_lowercase().contains("does not exist") {
        Some(ColorScheme::Light)
    } else {
        None
    }
}

#[cfg(any(test, target_os = "windows"))]
pub(crate) fn scheme_from_apps_use_light_theme(output: &str) -> Option<ColorScheme> {
    let token = output
        .lines()
        .find(|line| line.contains("AppsUseLightTheme"))?
        .split_whitespace()
        .next_back()?;
    match token {
        "0x0" | "0x00000000" | "0" => Some(ColorScheme::Dark),
        "0x1" | "0x00000001" | "1" => Some(ColorScheme::Light),
        _ => None,
    }
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
pub(crate) fn scheme_from_gnome_color_scheme(value: &str) -> Option<ColorScheme> {
    match value.trim().trim_matches('\'') {
        "prefer-dark" => Some(ColorScheme::Dark),
        "prefer-light" | "default" => Some(ColorScheme::Light),
        _ => None,
    }
}

fn os_color_scheme() -> Option<ColorScheme> {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("defaults");
        command.args(["read", "-g", "AppleInterfaceStyle"]);
        let output = command_output(command, Duration::from_millis(500))?;
        return scheme_from_apple_interface_style(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        );
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("reg");
        command.args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "/v",
            "AppsUseLightTheme",
        ]);
        let output = command_output(command, Duration::from_millis(500))?;
        return scheme_from_apps_use_light_theme(&String::from_utf8_lossy(&output.stdout));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut command = Command::new("gsettings");
        command.args(["get", "org.gnome.desktop.interface", "color-scheme"]);
        let output = command_output(command, Duration::from_millis(500))?;
        if !output.status.success() {
            return None;
        }
        return scheme_from_gnome_color_scheme(&String::from_utf8_lossy(&output.stdout));
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

fn command_output(mut command: Command, timeout: Duration) -> Option<std::process::Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
}

fn query_terminal_background() -> Option<(u8, u8, u8)> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return None;
    }
    #[cfg(unix)]
    {
        query_terminal_background_unix()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(unix)]
fn query_terminal_background_unix() -> Option<(u8, u8, u8)> {
    use std::os::fd::AsRawFd;

    let fd = io::stdin().as_raw_fd();
    let mut original = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
        return None;
    }
    let mut raw = original;
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_cc[libc::VMIN] = 0;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return None;
    }
    let _guard = TermiosGuard { fd, original };

    let mut stdout = io::stdout().lock();
    stdout.write_all(b"\x1b]11;?\x1b\\").ok()?;
    stdout.flush().ok()?;
    drop(stdout);

    let mut buffer = [0u8; 160];
    let mut filled = 0usize;
    let started = Instant::now();
    let timeout = Duration::from_millis(100);
    while started.elapsed() < timeout && filled < buffer.len() {
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let remaining = timeout.saturating_sub(started.elapsed()).as_millis() as i32;
        if unsafe { libc::poll(&mut pollfd, 1, remaining.max(1)) } <= 0 {
            break;
        }
        let read = unsafe {
            libc::read(
                fd,
                buffer[filled..].as_mut_ptr().cast(),
                buffer.len() - filled,
            )
        };
        if read <= 0 {
            break;
        }
        filled += read as usize;
        if parse_osc11(&buffer[..filled]).is_some() {
            break;
        }
    }
    parse_osc11(&buffer[..filled])
}

#[cfg(unix)]
struct TermiosGuard {
    fd: i32,
    original: libc::termios,
}

#[cfg(unix)]
impl Drop for TermiosGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

pub(crate) fn parse_osc11(bytes: &[u8]) -> Option<(u8, u8, u8)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let rest = text.get(text.find("rgb:")? + 4..)?;
    let payload_end = rest
        .find(|character: char| {
            character == '\u{7}'
                || character == '\\'
                || character == '\u{1b}'
                || character.is_whitespace()
        })
        .unwrap_or(rest.len());
    let mut channels = rest[..payload_end].split('/');
    let red = scale_hex(channels.next()?)?;
    let green = scale_hex(channels.next()?)?;
    let blue = scale_hex(channels.next()?)?;
    if channels.next().is_some() {
        return None;
    }
    Some((red, green, blue))
}

fn scale_hex(hex: &str) -> Option<u8> {
    if hex.is_empty()
        || hex.len() > 4
        || !hex.chars().all(|character| character.is_ascii_hexdigit())
    {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let max = (1u32 << (hex.len() * 4)) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_changes_are_reported_only_after_a_real_switch() {
        let mut previous = None;
        assert_eq!(scheme_change(&mut previous, None), None);
        assert_eq!(scheme_change(&mut previous, Some(ColorScheme::Dark)), None);
        assert_eq!(scheme_change(&mut previous, Some(ColorScheme::Dark)), None);
        assert_eq!(scheme_change(&mut previous, None), None);
        assert_eq!(
            scheme_change(&mut previous, Some(ColorScheme::Light)),
            Some(ColorScheme::Light)
        );
        assert_eq!(scheme_change(&mut previous, Some(ColorScheme::Light)), None);
        assert_eq!(
            scheme_change(&mut previous, Some(ColorScheme::Dark)),
            Some(ColorScheme::Dark)
        );
    }

    #[test]
    fn explicit_themes_never_start_a_watcher() {
        assert!(SchemeWatcher::spawn(ThemePreference::Dark).is_none());
        assert!(SchemeWatcher::spawn(ThemePreference::Light).is_none());
    }

    #[test]
    fn explicit_preferences_do_not_inspect_the_environment() {
        assert_eq!(resolve(ThemePreference::Dark), ColorScheme::Dark);
        assert_eq!(resolve(ThemePreference::Light), ColorScheme::Light);
    }

    #[test]
    fn luminance_splits_the_built_in_backgrounds() {
        assert_eq!(scheme_from_rgb(24, 26, 27), ColorScheme::Dark);
        assert_eq!(scheme_from_rgb(255, 255, 255), ColorScheme::Light);
        assert_eq!(scheme_from_rgb(127, 127, 127), ColorScheme::Dark);
        assert_eq!(scheme_from_rgb(128, 128, 128), ColorScheme::Light);
    }

    #[test]
    fn colorfgbg_uses_the_background_index() {
        assert_eq!(scheme_from_colorfgbg(Some("15;0")), Some(ColorScheme::Dark));
        assert_eq!(
            scheme_from_colorfgbg(Some("0;15")),
            Some(ColorScheme::Light)
        );
        assert_eq!(scheme_from_colorfgbg(Some("0;7")), Some(ColorScheme::Light));
        assert_eq!(scheme_from_colorfgbg(Some("default")), None);
        assert_eq!(scheme_from_colorfgbg(None), None);
    }

    #[test]
    fn osc11_accepts_two_and_four_digit_channels() {
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
            Some((255, 255, 255))
        );
        assert_eq!(
            parse_osc11(b"\x1b]11;rgb:181a/1b1b/1c1c\x07"),
            Some((24, 27, 28))
        );
        assert_eq!(parse_osc11(b"\x1b]11;rgb:ff/00/00\x07"), Some((255, 0, 0)));
        assert_eq!(parse_osc11(b"not a color"), None);
    }

    #[test]
    fn operating_system_reports_map_to_one_scheme() {
        assert_eq!(
            scheme_from_apple_interface_style("Dark\n", ""),
            Some(ColorScheme::Dark)
        );
        assert_eq!(
            scheme_from_apple_interface_style(
                "",
                "The domain/default pair of (kCFPreferencesAnyApplication, AppleInterfaceStyle) does not exist\n"
            ),
            Some(ColorScheme::Light)
        );
        assert_eq!(
            scheme_from_apps_use_light_theme("    AppsUseLightTheme    REG_DWORD    0x1\n"),
            Some(ColorScheme::Light)
        );
        assert_eq!(
            scheme_from_apps_use_light_theme("    AppsUseLightTheme    REG_DWORD    0x0\n"),
            Some(ColorScheme::Dark)
        );
        assert_eq!(
            scheme_from_gnome_color_scheme("'prefer-dark'\n"),
            Some(ColorScheme::Dark)
        );
        assert_eq!(
            scheme_from_gnome_color_scheme("'default'\n"),
            Some(ColorScheme::Light)
        );
    }

    #[test]
    fn light_palette_keeps_selected_text_dark() {
        let light = Palette::light();
        assert_ne!(light.bg, Palette::dark().bg);
        assert_ne!(light.selected_fg, Color::White);
        assert_eq!(Palette::dark().selected_fg, Color::White);
        assert_eq!(light.bg, Color::Rgb(255, 255, 255));
        assert_eq!(scheme_from_rgb(255, 255, 255), ColorScheme::Light);
    }

    fn relative_luminance(color: Color) -> f64 {
        let Color::Rgb(red, green, blue) = color else {
            panic!("light palette colors are RGB: {color:?}");
        };
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
    }

    fn contrast_ratio(first: Color, second: Color) -> f64 {
        let (first, second) = (relative_luminance(first), relative_luminance(second));
        (first.max(second) + 0.05) / (first.min(second) + 0.05)
    }

    #[test]
    fn light_palette_text_meets_wcag_aa_on_both_backgrounds() {
        let light = Palette::light();
        let foregrounds = [
            ("fg", light.fg),
            ("dim", light.dim),
            ("selected_fg", light.selected_fg),
            ("accent", light.accent),
            ("attention", light.attention),
            ("complete", light.complete),
        ];
        for (name, foreground) in foregrounds {
            for (background_name, background) in
                [("bg", light.bg), ("selected_bg", light.selected_bg)]
            {
                let ratio = contrast_ratio(foreground, background);
                assert!(
                    ratio >= 4.5,
                    "{name} on {background_name} is {ratio:.2}:1, below 4.5:1"
                );
            }
        }
    }
}
