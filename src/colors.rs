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
