//! Logging. Everything goes to stderr with a stable prefix; herdr captures plugin
//! stdout/stderr into `herdr plugin log list`, and the detached `watch` daemon has its
//! own stdio redirected into `<state_dir>/reopen.log` by `daemon::ensure_running`.

use std::io::Write;

pub const PREFIX: &str = "[reopen]";

pub fn emit(level: &str, msg: &str) {
    let _ = writeln!(std::io::stderr(), "{PREFIX} {level} {msg}");
}

#[macro_export]
macro_rules! linfo {
    ($($arg:tt)*) => { $crate::log::emit("info", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! lwarn {
    ($($arg:tt)*) => { $crate::log::emit("warn", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! lerror {
    ($($arg:tt)*) => { $crate::log::emit("error", &format!($($arg)*)) };
}
