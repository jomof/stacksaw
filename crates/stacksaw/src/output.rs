//! Output formatting and exit codes for the scriptable CLI (§10).

use std::str::FromStr;

use serde::Serialize;
use stacksaw_ssp::types::ErrorEnvelope;

/// CLI exit codes (§10). Retained as documentation of the contract even where
/// handlers currently return raw `i32`.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum ExitCode {
    Ok = 0,
    Findings = 1,
    Usage = 2,
    RepoError = 3,
    DaemonError = 4,
    LockTimeout = 5,
    MutationAborted = 10,
}

/// Selected output format (§10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
    Jsonl,
}

impl FromStr for Format {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "text" => Ok(Format::Text),
            "json" => Ok(Format::Json),
            "jsonl" => Ok(Format::Jsonl),
            other => Err(format!("unknown output format {other:?}")),
        }
    }
}

/// Print a value as a single JSON envelope (pretty when a TTY, compact else).
pub fn print_json<T: Serialize>(value: &T) {
    let text = if is_tty() {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .unwrap_or_else(|e| format!("{{\"error\":{{\"code\":\"serialize\",\"message\":{e:?}}}}}"));
    println!("{text}");
}

/// Print each item on its own line (jsonl streaming).
#[allow(dead_code)]
pub fn print_jsonl<T: Serialize>(items: &[T]) {
    for item in items {
        if let Ok(line) = serde_json::to_string(item) {
            println!("{line}");
        }
    }
}

/// Print a structured error to stderr in the `{"error":{...}}` shape (§10).
pub fn print_json_error(code: &str, message: &str) {
    let env = ErrorEnvelope::new(code, message);
    if let Ok(text) = serde_json::to_string(&env) {
        eprintln!("{text}");
    }
}

/// Determinism: no ANSI when stdout is not a TTY (§10).
pub fn is_tty() -> bool {
    #[cfg(unix)]
    {
        // isatty(1)
        unsafe { isatty(1) == 1 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(unix)]
extern "C" {
    fn isatty(fd: i32) -> i32;
}
