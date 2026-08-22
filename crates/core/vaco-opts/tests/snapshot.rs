//! A snapshot of the rendered schema, so any change to the option model shows
//! up as a reviewable diff rather than as a silent behaviour change.
//!
//! The rendering below is deliberately close in shape to what `-h full` prints
//! — name, type, flag column, help, default, range, then the unit's named
//! constants — because the `-h full` differential harness (plan 11 §6.9) will
//! diff exactly these facts against the reference tool. This test does not
//! *validate* the flag-column layout; it *pins* it, so that when the harness
//! lands, any divergence is one diff away from being seen.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use core::fmt::Write as _;

use support::AllKinds;
use vaco_opts::{OptFlags, Schema, help_entries, schema_of};

fn render(schema: &'static Schema) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} — {}", schema.class_name, schema.class_help);
    for e in help_entries(schema, OptFlags::empty()) {
        let column = String::from_utf8(e.flags_column.to_vec()).unwrap();
        let _ = write!(
            out,
            "  {:<12} {:<12} {column}  {}",
            e.name,
            e.kind.type_name(),
            e.help
        );
        if !e.aliases.is_empty() {
            let _ = write!(out, " (aka {})", e.aliases.join(", "));
        }
        if !e.default_repr.is_empty() {
            let _ = write!(out, " (default {})", e.default_repr);
        }
        if let Some(r) = e.range {
            let _ = write!(out, " (from {} to {})", r.min, r.max);
        }
        out.push('\n');
        for c in e.consts {
            let _ = writeln!(
                out,
                "     {:<12} {:<12}       {}",
                c.name,
                c.value
                    .as_i64()
                    .map_or_else(|| c.value.as_f64().to_string(), |v| v.to_string()),
                c.help.trim()
            );
        }
    }
    for c in schema.children {
        out.push('\n');
        out.push_str(&render(c));
    }
    out
}

#[test]
fn schema_snapshot() {
    insta::assert_snapshot!(render(schema_of::<AllKinds>()));
}
