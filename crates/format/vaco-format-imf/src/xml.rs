//! A generic, bounded XML tree — the one `quick-xml` pass every CPL/PKL/
//! ASSETMAP element parser in this crate walks afterwards.
//!
//! Unlike an MPD (`vaco-demux-dash::tree`, the module this one is modelled
//! on — genuinely the same shape, kept as a separate small copy rather than
//! a shared dependency since `crates/format/` may not depend on another
//! `crates/format/` crate for a generic utility this size without a shared
//! home for it; see this crate's own docs, "How to change it"), IMF's XML
//! documents (ST 2067-3 CPL, ST 2067-2/ST 429-8 PKL, ST 429-9 ASSETMAP)
//! carry most of their real data in **element text content**
//! (`<Id>urn:uuid:...</Id>`, `<EditRate>24 1</EditRate>`), not attributes —
//! [`XmlNode::text`] is exercised far more here than in the MPD case.

use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, Event};
use quick_xml::{XmlVersion, events::BytesStart};
use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// One XML element, namespace prefix stripped. IMF documents commonly
/// declare a default namespace (`xmlns="http://www.smpte-ra.org/..."`)
/// rather than a prefixed one, so most real files never need the stripping
/// at all — it is kept anyway since a prefixed variant is legal XML and
/// costs nothing extra to handle.
#[derive(Debug, Clone, Default)]
pub struct XmlNode {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<XmlNode>,
    /// Concatenated text content, trimmed of leading/trailing whitespace —
    /// most IMF leaf elements are exactly one text-only value
    /// (`<IntrinsicDuration>144</IntrinsicDuration>`), and real documents
    /// indent with newlines a naive concatenation would otherwise leave in.
    pub text: String,
}

/// Nodes parsed before refusing the document as a possible resource
/// exhaustion attempt — the same defence `vaco-demux-dash::tree` uses for
/// the same reason (a document with millions of trivial elements is a more
/// direct denial-of-service than any one element's own declared size).
pub const MAX_NODES: u64 = 1 << 18;

impl XmlNode {
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
    pub fn child(&self, name: &str) -> Option<&XmlNode> {
        self.children.iter().find(|c| c.name == name)
    }

    /// The first child element named `name`, its own trimmed text content.
    #[must_use]
    pub fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.as_str())
    }

    /// Every child element named `name`, in document order.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a XmlNode> {
        self.children.iter().filter(move |c| c.name == name)
    }
}

/// Strip a namespace prefix (`cpl:Id` -> `Id`).
fn local_name(raw: &str) -> String {
    raw.rsplit_once(':')
        .map_or_else(|| raw.to_owned(), |(_, n)| n.to_owned())
}

/// Trim `s` in place. The equivalent assignment from `trim()` reallocates,
/// and this runs once per element.
fn trim_in_place(s: &mut String) {
    s.truncate(s.trim_end().len());
    let start = s.len() - s.trim_start().len();
    s.drain(..start);
}

/// The replacement text of one `&...;` reference. `quick-xml` reports general
/// and character references as their own events instead of folding them into
/// the surrounding `Text`, so a parser that matches only `Text` drops them.
fn entity_text(r: &BytesRef<'_>) -> Option<String> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c.to_string());
    }
    resolve_predefined_entity(r).map(str::to_owned)
}

fn read_attrs(e: &BytesStart<'_>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for a in e.attributes().flatten() {
        let key = local_name(a.key.as_ref());
        let value = a
            .normalized_value(XmlVersion::Implicit1_0)
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_default();
        out.push((key, value));
    }
    out
}

/// Parse `xml` into one [`XmlNode`] tree, bounded by `budget`'s fuel and
/// [`MAX_NODES`].
///
/// # Errors
/// [`Error::InvalidData`] for malformed XML or a document with no root
/// element; [`Error::LimitExceeded`] past [`MAX_NODES`] or the budget's own
/// fuel.
pub fn parse(xml: &str, budget: &mut Budget) -> Result<XmlNode> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<XmlNode> = vec![XmlNode::default()];
    let mut buf = Vec::new();
    let mut count = 0u64;
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|_| Error::InvalidData("malformed IMF XML document"))?;
        match event {
            Event::Eof => break,
            Event::Start(e) => {
                count = count.saturating_add(1);
                if count > MAX_NODES {
                    return Err(Error::LimitExceeded {
                        limit: "imf_xml_nodes",
                        requested: count,
                        cap: MAX_NODES,
                    });
                }
                budget.consume_fuel(1)?;
                stack.push(XmlNode {
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
                        limit: "imf_xml_nodes",
                        requested: count,
                        cap: MAX_NODES,
                    });
                }
                budget.consume_fuel(1)?;
                let node = XmlNode {
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
                    && let Some(mut node) = stack.pop()
                    && let Some(parent) = stack.last_mut()
                {
                    trim_in_place(&mut node.text);
                    parent.children.push(node);
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if let Some(top) = stack.last_mut()
                    && let Some(text) = entity_text(&r)
                {
                    top.text.push_str(&text);
                }
            }
            _ => {}
        }
        buf.clear();
    }
    let mut root = stack
        .pop()
        .ok_or(Error::InvalidData("empty IMF XML document"))?;
    let mut node = root
        .children
        .pop()
        .ok_or(Error::InvalidData("IMF XML document has no root element"))?;
    node.text = node.text.trim().to_owned();
    Ok(node)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn parses_attributes_nesting_and_text() {
        let xml = r"<CompositionPlaylist><Id>urn:uuid:x</Id><EditRate>24 1</EditRate></CompositionPlaylist>";
        let mut b = Budget::new(Limits::permissive());
        let cpl = parse(xml, &mut b).unwrap();
        assert_eq!(cpl.name, "CompositionPlaylist");
        assert_eq!(cpl.child_text("Id"), Some("urn:uuid:x"));
        assert_eq!(cpl.child_text("EditRate"), Some("24 1"));
    }

    #[test]
    fn strips_namespace_prefixes() {
        let xml = r#"<cpl:CompositionPlaylist xmlns:cpl="urn:x"><cpl:Id>y</cpl:Id></cpl:CompositionPlaylist>"#;
        let mut b = Budget::new(Limits::permissive());
        let cpl = parse(xml, &mut b).unwrap();
        assert_eq!(cpl.name, "CompositionPlaylist");
        assert_eq!(cpl.child_text("Id"), Some("y"));
    }

    #[test]
    fn malformed_xml_is_rejected() {
        let mut b = Budget::new(Limits::permissive());
        assert!(parse("<CPL><Id>", &mut b).is_err());
        assert!(parse("not xml at all", &mut b).is_err());
    }

    #[test]
    fn a_huge_flat_document_is_bounded() {
        let mut xml = String::from("<CPL>");
        for _ in 0..(MAX_NODES + 10) {
            xml.push_str("<X/>");
        }
        xml.push_str("</CPL>");
        let mut b = Budget::new(Limits::permissive());
        assert!(parse(&xml, &mut b).is_err());
    }
}
