//! A generic, bounded XML tree — the one `quick-xml` pass every MPD element
//! parser in [`crate::mpd`] walks afterwards.
//!
//! An MPD's actual data lives entirely in **attributes**; element nesting is
//! the structure (`MPD` > `Period` > `AdaptationSet` > `Representation` >
//! `SegmentTemplate` > `SegmentTimeline` > `S`). Building one generic tree
//! first and interpreting it afterwards is far easier to get right than a
//! hand-rolled streaming state machine over `quick-xml`'s push events, at the
//! cost of holding the whole (already bounded) document in memory at once —
//! an acceptable trade for a document this small.

use quick_xml::Reader;
use quick_xml::events::Event;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// One XML element, namespace prefix stripped (an MPD's own default
/// namespace means real documents rarely have one, but some deployed
/// manifests use `mpd:MPD`-style prefixes).
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    /// Concatenated text content — only `UTCTiming`'s value and similar leaf
    /// elements need this; most MPD data is attribute-carried.
    pub text: String,
}

/// Nodes parsed before refusing the document as a possible resource
/// exhaustion attempt. An MPD naming a `SegmentTimeline` with millions of
/// `<S>` elements is a more direct `DoS` than the `@r` repeat-count one
/// (`vaco_format_adaptive::timeline::MAX_SEGMENTS` guards that one); this
/// guards the parse itself.
pub const MAX_NODES: u64 = 1 << 18;

impl Node {
    /// The first attribute named `name`.
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The first child element named `name`.
    #[must_use]
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child element named `name`, in document order.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |c| c.name == name)
    }
}

/// Strip a namespace prefix (`mpd:MPD` -> `MPD`).
fn local_name(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.rsplit_once(':')
        .map_or_else(|| s.to_string(), |(_, n)| n.to_owned())
}

fn read_attrs(e: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for a in e.attributes().flatten() {
        let key = local_name(a.key.as_ref());
        let value = a
            .unescape_value()
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_default();
        out.push((key, value));
    }
    out
}

/// Parse `xml` into one [`Node`] tree, bounded by `budget`'s fuel and
/// [`MAX_NODES`].
///
/// # Errors
/// [`Error::InvalidData`] for malformed XML or a document with no root
/// element; [`Error::LimitExceeded`] past [`MAX_NODES`] or the budget's own
/// fuel.
pub fn parse(xml: &str, budget: &mut Budget) -> Result<Node> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut buf = Vec::new();
    let mut count = 0u64;
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|_| Error::InvalidData("malformed MPD XML"))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                count = count.saturating_add(1);
                if count > MAX_NODES {
                    return Err(Error::LimitExceeded {
                        limit: "dash_mpd_nodes",
                        requested: count,
                        cap: MAX_NODES,
                    });
                }
                budget.consume_fuel(1)?;
                stack.push(Node {
                    name: local_name(e.name().as_ref()),
                    attrs: read_attrs(&e),
                    children: Vec::new(),
                    text: String::new(),
                });
            }
            Event::Empty(e) => {
                count = count.saturating_add(1);
                if count > MAX_NODES {
                    return Err(Error::LimitExceeded {
                        limit: "dash_mpd_nodes",
                        requested: count,
                        cap: MAX_NODES,
                    });
                }
                budget.consume_fuel(1)?;
                let node = Node {
                    name: local_name(e.name().as_ref()),
                    attrs: read_attrs(&e),
                    children: Vec::new(),
                    text: String::new(),
                };
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                }
            }
            Event::End(_) => {
                if stack.len() > 1
                    && let Some(node) = stack.pop()
                    && let Some(parent) = stack.last_mut()
                {
                    parent.children.push(node);
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    let decoded = t.unescape().unwrap_or_default();
                    top.text.push_str(&decoded);
                }
            }
            _ => {}
        }
        buf.clear();
    }
    let mut root = stack
        .pop()
        .ok_or(Error::InvalidData("empty MPD document"))?;
    root.children
        .pop()
        .ok_or(Error::InvalidData("MPD document has no root element"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn parses_attributes_and_nesting() {
        let xml = r#"<MPD type="static"><Period id="0"><AdaptationSet mimeType="video/mp4"/></Period></MPD>"#;
        let mut b = Budget::new(Limits::permissive());
        let mpd = parse(xml, &mut b).unwrap();
        assert_eq!(mpd.name, "MPD");
        assert_eq!(mpd.attr("type"), Some("static"));
        let period = mpd.child("Period").unwrap();
        assert_eq!(period.attr("id"), Some("0"));
        let aset = period.child("AdaptationSet").unwrap();
        assert_eq!(aset.attr("mimeType"), Some("video/mp4"));
    }

    #[test]
    fn strips_namespace_prefixes() {
        let xml = r#"<mpd:MPD xmlns:mpd="urn:x"><mpd:Period/></mpd:MPD>"#;
        let mut b = Budget::new(Limits::permissive());
        let mpd = parse(xml, &mut b).unwrap();
        assert_eq!(mpd.name, "MPD");
        assert!(mpd.child("Period").is_some());
    }

    #[test]
    fn malformed_xml_is_rejected() {
        let mut b = Budget::new(Limits::permissive());
        assert!(parse("<MPD><Period>", &mut b).is_err());
        assert!(parse("not xml at all", &mut b).is_err());
    }

    #[test]
    fn a_huge_flat_document_is_bounded() {
        let mut xml = String::from("<MPD>");
        for _ in 0..(MAX_NODES + 10) {
            xml.push_str("<Period/>");
        }
        xml.push_str("</MPD>");
        let mut b = Budget::new(Limits::permissive());
        assert!(parse(&xml, &mut b).is_err());
    }
}
