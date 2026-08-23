//! RFC 4566 §5 line-oriented SDP parser.
//!
//! SDP is a text format an RTSP `DESCRIBE` response body hands over verbatim
//! from whatever server sent it — every line here is attacker-controlled.
//! The parser is deliberately permissive about *line content* (an
//! unparseable `o=`/`c=` line is simply left as `None` rather than failing
//! the whole session, matching how forgiving real RTSP servers' SDP tends to
//! be) but bounded in *shape*: [`parse`] never recurses, allocates one
//! `String`/`Vec` per line at most, and always terminates in the number of
//! lines in the input.

use super::types::{Attribute, Connection, MediaDescription, Origin, SessionDescription};

/// Parse a complete SDP session description.
///
/// # Errors
/// [`vaco_core::Error::InvalidData`] if the text contains no `v=0` line at
/// all (RFC 4566 §5: `v=` is always first) — the one structural requirement
/// this parser enforces, because a body that fails even that is not SDP and
/// should not silently become an empty session with zero media.
pub fn parse(text: &str) -> vaco_core::Result<SessionDescription> {
    let mut lines = normalize_lines(text);
    let Some(first) = lines.first() else {
        return Err(vaco_core::Error::InvalidData("empty SDP body"));
    };
    if !first.starts_with("v=") {
        return Err(vaco_core::Error::InvalidData(
            "SDP body does not start with a v= line",
        ));
    }
    lines.remove(0);

    let mut sess = SessionDescription::default();
    // Session-level lines, up to the first `m=`.
    let split_at = lines
        .iter()
        .position(|l| l.starts_with("m="))
        .unwrap_or(lines.len());
    let (session_lines, media_lines) = lines.split_at(split_at);

    for line in session_lines {
        apply_session_line(&mut sess, line);
    }

    let mut current: Option<MediaDescription> = None;
    for line in media_lines {
        if let Some(rest) = line.strip_prefix("m=") {
            if let Some(m) = current.take() {
                sess.media.push(m);
            }
            current = Some(parse_media_line(rest));
            continue;
        }
        if let Some(m) = current.as_mut() {
            apply_media_line(m, line);
        }
    }
    if let Some(m) = current.take() {
        sess.media.push(m);
    }

    Ok(sess)
}

/// Split into non-empty, CR-stripped lines. RFC 4566 mandates `<CR><LF>` but
/// real servers (and every `.sdp` file this crate has been handed in
/// testing) also send bare `<LF>`, so this splits on `\n` and trims a
/// trailing `\r`.
fn normalize_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .collect()
}

fn split_type(line: &str) -> Option<(char, &str)> {
    let mut chars = line.chars();
    let ty = chars.next()?;
    let rest = chars.as_str().strip_prefix('=')?;
    Some((ty, rest))
}

fn parse_origin(value: &str) -> Option<Origin> {
    let mut it = value.split_whitespace();
    Some(Origin {
        username: it.next()?.to_owned(),
        session_id: it.next()?.to_owned(),
        session_version: it.next()?.to_owned(),
        net_type: it.next()?.to_owned(),
        addr_type: it.next()?.to_owned(),
        address: it.next()?.to_owned(),
    })
}

fn parse_connection(value: &str) -> Option<Connection> {
    let mut it = value.split_whitespace();
    let net_type = it.next()?.to_owned();
    let addr_type = it.next()?.to_owned();
    let addr_field = it.next()?;
    let mut parts = addr_field.split('/');
    let address = parts.next()?.to_owned();
    let ttl = parts.next().and_then(|s| s.parse().ok());
    let count = parts.next().and_then(|s| s.parse().ok());
    Some(Connection {
        net_type,
        addr_type,
        address,
        ttl,
        count,
    })
}

fn parse_bandwidth(value: &str) -> Option<(String, u64)> {
    let (bwtype, n) = value.split_once(':')?;
    Some((bwtype.to_owned(), n.trim().parse().ok()?))
}

fn parse_attribute(value: &str) -> Attribute {
    match value.split_once(':') {
        Some((name, val)) => Attribute {
            name: name.to_owned(),
            value: Some(val.to_owned()),
        },
        None => Attribute {
            name: value.to_owned(),
            value: None,
        },
    }
}

fn apply_session_line(sess: &mut SessionDescription, line: &str) {
    let Some((ty, value)) = split_type(line) else {
        return;
    };
    match ty {
        'o' => sess.origin = parse_origin(value),
        's' => sess.session_name = Some(value.to_owned()),
        'i' => sess.information = Some(value.to_owned()),
        'c' => sess.connection = parse_connection(value),
        'b' => {
            if let Some(bw) = parse_bandwidth(value) {
                sess.bandwidth.push(bw);
            }
        }
        'a' => sess.attributes.push(parse_attribute(value)),
        _ => {}
    }
}

fn parse_media_line(rest: &str) -> MediaDescription {
    let mut it = rest.split_whitespace();
    let media = it.next().unwrap_or_default().to_owned();
    let port_field = it.next().unwrap_or_default();
    let (port, port_count) = match port_field.split_once('/') {
        Some((p, c)) => (p.parse().unwrap_or(0), c.parse().ok()),
        None => (port_field.parse().unwrap_or(0), None),
    };
    let proto = it.next().unwrap_or_default().to_owned();
    let formats = it.map(str::to_owned).collect();
    MediaDescription {
        media,
        port,
        port_count,
        proto,
        formats,
        information: None,
        connection: None,
        bandwidth: Vec::new(),
        attributes: Vec::new(),
    }
}

fn apply_media_line(m: &mut MediaDescription, line: &str) {
    let Some((ty, value)) = split_type(line) else {
        return;
    };
    match ty {
        'i' => m.information = Some(value.to_owned()),
        'c' => m.connection = parse_connection(value),
        'b' => {
            if let Some(bw) = parse_bandwidth(value) {
                m.bandwidth.push(bw);
            }
        }
        'a' => m.attributes.push(parse_attribute(value)),
        _ => {}
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    const SAMPLE: &str = "v=0\r\n\
o=- 1234 1 IN IP4 192.0.2.10\r\n\
s=Example Stream\r\n\
c=IN IP4 192.0.2.10\r\n\
t=0 0\r\n\
a=control:*\r\n\
m=video 0 RTP/AVP 96\r\n\
a=rtpmap:96 H264/90000\r\n\
a=fmtp:96 packetization-mode=1\r\n\
a=control:trackID=1\r\n\
m=audio 0 RTP/AVP 97\r\n\
a=rtpmap:97 MPEG4-GENERIC/48000/2\r\n\
a=control:trackID=2\r\n";

    #[test]
    fn parses_the_full_shape() {
        let sess = parse(SAMPLE).unwrap();
        assert_eq!(sess.origin.as_ref().unwrap().address, "192.0.2.10");
        assert_eq!(sess.session_name.as_deref(), Some("Example Stream"));
        assert_eq!(sess.connection.as_ref().unwrap().address, "192.0.2.10");
        assert_eq!(sess.media.len(), 2);

        let video = &sess.media[0];
        assert_eq!(video.media, "video");
        assert_eq!(video.formats, vec!["96"]);
        assert_eq!(video.attr("control"), Some("trackID=1"));
        let rtpmap = super::super::types::parse_rtpmap(video.attr("rtpmap").unwrap()).unwrap();
        assert_eq!(rtpmap.encoding_name, "H264");

        let audio = &sess.media[1];
        assert_eq!(audio.media, "audio");
        assert_eq!(audio.attr("control"), Some("trackID=2"));
    }

    #[test]
    fn session_level_control_is_inherited_by_reading_it_directly() {
        let sess = parse(SAMPLE).unwrap();
        assert!(sess.attributes.iter().any(|a| a.is("control")));
    }

    #[test]
    fn rejects_missing_v_line() {
        assert!(parse("o=- 1 1 IN IP4 0.0.0.0\r\n").is_err());
    }

    #[test]
    fn tolerates_bare_lf_and_trailing_garbage_lines() {
        let text = "v=0\nnotaline\nm=video 0 RTP/AVP 96\na=rtpmap:96 H264/90000\n";
        let sess = parse(text).unwrap();
        assert_eq!(sess.media.len(), 1);
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics(s in ".{0,500}") {
            let _ = parse(&s);
        }
    }
}
