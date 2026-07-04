//! `stacksaw schema [<name>]` — print JSON Schema for machine consumers (§10).

use schemars::schema_for;
use stacksaw_ssp::types::{
    EditBegin, EditFinish, ErrorEnvelope, Finding, Snapshot, Staircase,
};

/// The set of named schemas the CLI can print.
pub const NAMES: &[&str] = &[
    "snapshot",
    "staircase",
    "finding",
    "edit-begin",
    "edit-finish",
    "error",
];

/// Print the JSON Schema for `name`, or the list of names when `None`.
pub fn print(name: Option<&str>) -> i32 {
    let Some(name) = name else {
        for n in NAMES {
            println!("{n}");
        }
        return 0;
    };
    let schema = match name {
        "snapshot" => serde_json::to_value(schema_for!(Snapshot)),
        "staircase" => serde_json::to_value(schema_for!(Staircase)),
        "finding" => serde_json::to_value(schema_for!(Finding)),
        "edit-begin" => serde_json::to_value(schema_for!(EditBegin)),
        "edit-finish" => serde_json::to_value(schema_for!(EditFinish)),
        "error" => serde_json::to_value(schema_for!(ErrorEnvelope)),
        other => {
            eprintln!("unknown schema {other:?}; known: {}", NAMES.join(", "));
            return 2;
        }
    };
    match schema {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            0
        }
        Err(e) => {
            eprintln!("schema error: {e}");
            3
        }
    }
}
