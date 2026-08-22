//! The data behind `-h filter=x`, `-h encoder=y` and `-h full`.
//!
//! `vaco-opts` supplies the facts; `vaco-cli-core` supplies the layout. Option
//! names, types, ranges, defaults and the flag column are interface facts and
//! must match the reference tool (D9). Help *strings* are ours, written fresh.

use crate::{ConstDesc, OptFlags, OptKind, OptRangeDisplay, Schema};

/// One row of help output, plus the named constants printed beneath it.
#[derive(Debug, Clone, Copy)]
pub struct HelpEntry<'a> {
    pub name: &'a str,
    pub aliases: &'a [&'a str],
    pub kind: OptKind,
    pub flags_column: [u8; 11],
    pub help: &'a str,
    pub default_repr: &'a str,
    pub range: Option<OptRangeDisplay>,
    pub consts: &'a [ConstDesc],
}

/// Every option of `schema` that passes `filter`, in declaration order.
///
/// An empty `filter` passes everything, which is what `-h full` wants.
#[must_use]
pub fn help_entries(schema: &'static Schema, filter: OptFlags) -> Vec<HelpEntry<'static>> {
    schema
        .options
        .iter()
        .filter(|o| filter.is_empty() || o.flags.intersects(filter))
        .map(|o| HelpEntry {
            name: o.name,
            aliases: o.aliases,
            kind: o.kind,
            flags_column: o.flags.column(),
            help: o.help,
            default_repr: o.default_repr,
            range: o.range,
            consts: o.consts,
        })
        .collect()
}

/// [`help_entries`] over the schema and, depth first, its children.
#[must_use]
pub fn help_entries_recursive(
    schema: &'static Schema,
    filter: OptFlags,
) -> Vec<(&'static str, HelpEntry<'static>)> {
    let mut out: Vec<(&'static str, HelpEntry<'static>)> = help_entries(schema, filter)
        .into_iter()
        .map(|e| (schema.class_name, e))
        .collect();
    for c in schema.children {
        out.extend(help_entries_recursive(c, filter));
    }
    out
}

/// Doc-comment-derived help text keeps the leading space `///` inserts.
/// Rendering trims it; the raw string stays in the descriptor so a snapshot
/// diff shows exactly what the macro produced.
#[must_use]
pub fn trim_doc(s: &str) -> &str {
    s.trim()
}
