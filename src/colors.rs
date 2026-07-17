//! Tiny ANSI color helper. No dependencies.
//!
//! Honors NO_COLOR (https://no-color.org) and only colorizes when stderr is a
//! TTY. Tone choices follow brew / gh CLI: muted, no neon. Cyan for accents,
//! green for success, yellow for warnings, dim for hints.

use std::env;
use std::io::IsTerminal;

fn color_enabled() -> bool {
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if env::var_os("FORCE_COLOR").is_some() {
        return true;
    }
    std::io::stderr().is_terminal()
}

fn wrap(open: &str, close: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{open}m{s}\x1b[{close}m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    wrap("1", "22", s)
}
pub fn dim(s: &str) -> String {
    wrap("2", "22", s)
}
pub fn green(s: &str) -> String {
    wrap("32", "39", s)
}
pub fn yellow(s: &str) -> String {
    wrap("33", "39", s)
}
pub fn cyan(s: &str) -> String {
    wrap("36", "39", s)
}
pub fn red(s: &str) -> String {
    wrap("31", "39", s)
}

/// Monocle brand orange (#f97316, DESIGN.md §2 Primary/Orange/500) via
/// truecolor ANSI. Reserved for the handful of deliberate brand touchpoints —
/// today, just `monocle agent`'s "▸ agent" mode banner (`monocle chat`'s "▸
/// chat" banner deliberately stays cyan, see `commands::repl::mode_banner`) —
/// NOT a general-purpose accent color; the rest of this CLI intentionally
/// stays muted/cyan (gh/brew convention) so it never fights the user's
/// terminal theme.
pub fn orange(s: &str) -> String {
    wrap("38;2;249;115;22", "39", s)
}

#[cfg(test)]
mod tests {
    use super::orange;
    use std::env;
    use std::sync::Mutex;

    // `FORCE_COLOR` is process-wide state and `cargo test` runs unit tests on
    // parallel threads within one process — guard the mutation so a future
    // test that reads/sets NO_COLOR/FORCE_COLOR (or asserts exact-equality on
    // colored output) can't race with this one.
    static COLOR_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn orange_wraps_with_truecolor_sgr_when_forced() {
        let _guard = COLOR_ENV_LOCK.lock().unwrap();
        env::set_var("FORCE_COLOR", "1");
        let out = orange("agent");
        env::remove_var("FORCE_COLOR");
        assert!(out.contains("38;2;249;115;22"));
    }
}
