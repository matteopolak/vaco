//! Fuzz `vaco-textformat`'s escaping tables and the writers built on them.
//!
//! Container metadata reaches these functions verbatim — a Matroska tag value
//! is attacker-controlled text that lands in `escape_ini` unmodified — so this
//! is the crate's untrusted-input boundary even though nothing here parses.
//!
//! Three classes of finding:
//!
//! * a panic or an arithmetic overflow anywhere in the escape or writer path;
//! * an escape that does not round-trip, which means a consumer cannot recover
//!   the original value;
//! * a writer emitting its own item separator unescaped, which would tear a
//!   record in half for anything splitting on it.
//! fuzz-crate: vaco-textformat

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_textformat::escape::{
    DEFAULT_REPLACEMENT, StringValidation, escape_c, escape_csv, escape_flat, escape_ini,
    escape_json, escape_xml, unescape_c, unescape_csv, unescape_flat, unescape_ini, unescape_json,
    unescape_xml, validate_xml,
};
use vaco_textformat::sections::SectionId;
use vaco_textformat::{FormatOpts, TextFormat, writers};

/// Whether `s` contains `sep` outside a backslash escape.
fn has_bare(s: &str, sep: char) -> bool {
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            it.next();
        } else if c == sep {
            return true;
        }
    }
    false
}

fuzz_target!(|data: &[u8]| {
    // Split the input into a key and a value so key sanitisation is exercised
    // with hostile bytes too.
    let (key, value) = data.split_at(data.len() / 2);
    let (Ok(key), Ok(value)) = (str::from_utf8(key), str::from_utf8(value)) else {
        return;
    };

    for sep in ['|', ',', '@', ':', '='] {
        let e = escape_c(value, sep);
        assert_eq!(unescape_c(&e, sep).as_deref(), Some(value), "escape_c {sep:?}");
        assert!(!has_bare(&e, sep), "escape_c left a bare {sep:?}: {e:?}");

        let e = escape_csv(value, sep);
        assert_eq!(unescape_csv(&e).as_deref(), Some(value), "escape_csv {sep:?}");
        if value.contains([sep, '"', '\n', '\r']) {
            assert!(e.starts_with('"') && e.ends_with('"'), "unquoted: {e:?}");
        }
    }

    let e = escape_ini(value);
    assert_eq!(unescape_ini(&e).as_deref(), Some(value), "escape_ini");

    let e = escape_json(value);
    assert_eq!(unescape_json(&e).as_deref(), Some(value), "escape_json");

    let e = escape_flat(value);
    assert_eq!(unescape_flat(&e).as_deref(), Some(value), "escape_flat");

    for mode in [
        StringValidation::Fail,
        StringValidation::Ignore,
        StringValidation::Replace,
    ] {
        if let Some(v) = validate_xml(value, mode, DEFAULT_REPLACEMENT) {
            let e = escape_xml(&v);
            assert_eq!(unescape_xml(&e).as_deref(), Some(v.as_str()), "escape_xml");
        }
    }

    // And the whole writer path, which is where a state-machine bug shows up.
    for spec in writers::NAMES {
        let Ok(writer) = writers::make(spec) else {
            continue;
        };
        let mut tf = TextFormat::new(writer, Vec::new(), FormatOpts::default());
        let mut go = || -> vaco_textformat::Result<()> {
            tf.open(SectionId::ROOT)?;
            tf.open(SectionId::STREAMS)?;
            tf.open(SectionId::STREAM)?;
            tf.int("index", 0)?;
            tf.open(SectionId::STREAM_TAGS)?;
            tf.tag(key, value)?;
            tf.close()?;
            tf.open(SectionId::STREAM_SIDE_DATA_LIST)?;
            tf.open_typed(SectionId::STREAM_SIDE_DATA, value)?;
            tf.str("side_data_type", value)?;
            tf.close()?;
            tf.close()?;
            tf.close()?;
            tf.close()?;
            tf.close()
        };
        go().expect("writing to a Vec cannot fail");
        let out = tf.finish().expect("writing to a Vec cannot fail");
        assert!(out.ends_with(b"\n"), "{spec}: unterminated record");
    }
});
