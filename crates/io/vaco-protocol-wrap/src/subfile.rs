//! `subfile:` — a byte range of an inner URL.
//!
//! # Grammar
//!
//! `subfile,,start,N,end,M,,:inner-url`. The comma-delimited option prefix
//! lives in [`Url::args`] (rule S3 of `vaco-protocol-core`'s URL grammar) —
//! this module's job is to parse *that* string, which is genuinely odd and was
//! probed rather than guessed (D7): a plain `subfile,start,0,end,10:url`
//! (single commas, no wrapping pair) is refused by the reference with "Error
//! parsing options string", so the leading and trailing empty fields are not
//! cosmetic.
//!
//! Measured against `ffmpeg 8.1` (see [`parse_args`] for what each finding
//! became):
//!
//! * The two double-commas are mandatory: `subfile,start,0,end,10:x` fails.
//! * `start`/`end` may appear in either order.
//! * `end` is an **exclusive** offset, not a length: `start,100,end,300`
//!   yields exactly 200 bytes, byte-identical to the source range
//!   `[100, 300)`.
//! * `end` omitted (or `0`, its `AVOption` default) means "through EOF".
//! * `start` omitted means `0`.
//! * `end < start` (once `end` is genuinely set) is refused: "end before
//!   start".
//! * `start` at or beyond the inner size yields an immediately empty read, not
//!   an error.
//!
//! # Security
//!
//! `subfile:`'s inner URL is exactly the kind of thing rule U2/the whitelist
//! gate exist for: a `subfile:` reference inside an index built from a
//! document another party controls is unremarkable (extracting a JPEG frame
//! range from a Motion-JPEG file, say), so the inner open must not get a freer
//! environment than the one `subfile:` itself was opened under. This module
//! never constructs a new [`ProtocolEnv`]; it reuses the one it was given,
//! which is what makes the nested open exactly one level deeper rather than a
//! reset privilege check. See the crate docs for the measured whitelist
//! behaviour this implies (no default grant — the inner scheme must be
//! whitelisted explicitly, same as every protocol in this crate).

use vaco_io::{MediaSource, Seekability};
use vaco_opts::Dict;
use vaco_protocol_core::{
    IoFlags, Protocol, ProtocolDesc, ProtocolEnv, ProtocolError, ProtocolFlags, Result, Url,
};

/// A parsed `start`/`end` pair. `end` is `None` for "through EOF" — kept
/// distinct from `Some(0)`, which is never itself the *result* of parsing
/// (0 in the URL is the "unset" sentinel and normalises to `None` at parse
/// time), so a caller can't confuse the two later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub end: Option<u64>,
}

/// Parse the `args` field of a `subfile,,start,N,end,M,,:` URL.
///
/// `args` still carries its leading separator, as `vaco_protocol_core::Url`
/// documents (e.g. `,,start,1024,end,4096,,`), so this expects and strips the
/// two doubled commas rather than a bare `key,value,key,value` list.
///
/// # Errors
/// [`ProtocolError::Malformed`] for anything that does not match the grammar,
/// or where `end` is set and less than `start`.
pub fn parse_args(args: &str) -> Result<Range> {
    let malformed = |detail| ProtocolError::Malformed {
        scheme: "subfile",
        detail,
    };

    // The mandatory wrapping pair: a leading `,` (already consumed by the
    // splitter putting the scheme's own separator into `args` — see below)
    // and both an opening and closing empty field. Measured:
    // `subfile,start,0,end,10:x` — one leading comma only — is refused.
    let Some(inner) = args.strip_prefix(',').and_then(|s| s.strip_suffix(',')) else {
        return Err(malformed(
            "expected the form ,,start,N,end,M,, (both doubled commas)",
        ));
    };
    // After stripping one comma off each end, a *second* leading and trailing
    // empty field must remain: `args` for `subfile,,start,0,end,10,,:x` is
    // `,start,0,end,10,` at this point, and its own split on `,` starts and
    // ends with an empty string.
    let fields: Vec<&str> = inner.split(',').collect();
    let (first, rest_and_last) = fields.split_first().ok_or_else(|| malformed("empty"))?;
    let (last, middle) = rest_and_last
        .split_last()
        .ok_or_else(|| malformed("empty"))?;
    if !first.is_empty() || !last.is_empty() {
        return Err(malformed(
            "expected the form ,,start,N,end,M,, (both doubled commas)",
        ));
    }
    if middle.is_empty() || !middle.len().is_multiple_of(2) {
        return Err(malformed("expected key,value pairs between the commas"));
    }

    let mut start: u64 = 0;
    let mut end: Option<u64> = None;
    for pair in middle.chunks(2) {
        let [key, value] = pair else {
            return Err(malformed("expected key,value pairs between the commas"));
        };
        let n: u64 = value
            .parse()
            .map_err(|_| malformed("start/end must be non-negative integers"))?;
        match *key {
            "start" => start = n,
            // `0` is the `AVOption` default and measured to mean "through
            // EOF", so it normalises to `None` here rather than `Some(0)`,
            // which would instead mean an empty range.
            "end" if n == 0 => end = None,
            "end" => end = Some(n),
            _ => return Err(malformed("unknown subfile option (expected start or end)")),
        }
    }

    if let Some(e) = end
        && e < start
    {
        return Err(malformed("end before start"));
    }

    Ok(Range { start, end })
}

/// A [`MediaSource`] windowed onto `[start, end)` of `inner`.
///
/// Not a `RawSource` + `PeekSource` pair like `vaco-protocol-file`'s types:
/// `subfile:` needs to *translate* every position by `start` and clamp every
/// read at `end`, which sits above what `PeekSource` does (supply a peek
/// window over an otherwise-untouched stream of positions) rather than beside
/// it.
pub struct SubfileSource {
    inner: Box<dyn MediaSource>,
    /// Absolute offset into `inner` that this source's position `0` maps to.
    start: u64,
    /// Absolute offset into `inner` this source refuses to read past, or
    /// `None` for "through EOF".
    end: Option<u64>,
    /// This source's own logical position, always `<= len()` when known.
    pos: u64,
}

impl std::fmt::Debug for SubfileSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubfileSource")
            .field("start", &self.start)
            .field("end", &self.end)
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl SubfileSource {
    /// Wrap `inner`, presenting only `[range.start, range.end)`.
    ///
    /// # Errors
    /// Propagates a failed initial seek to `range.start`.
    pub fn new(mut inner: Box<dyn MediaSource>, range: Range) -> vaco_core::Result<Self> {
        if range.start > 0 {
            inner.seek(range.start)?;
        }
        Ok(Self {
            inner,
            start: range.start,
            end: range.end,
            pos: 0,
        })
    }

    /// Remaining bytes in the window from the current position, when the
    /// window has a known end.
    fn remaining(&self) -> Option<u64> {
        let end = self.end?;
        let abs = self.start.saturating_add(self.pos);
        Some(end.saturating_sub(abs.min(end)))
    }
}

impl MediaSource for SubfileSource {
    fn read(&mut self, buf: &mut [u8]) -> vaco_core::Result<usize> {
        let want = match self.remaining() {
            Some(0) => return Ok(0),
            Some(r) => buf.len().min(usize::try_from(r).unwrap_or(usize::MAX)),
            None => buf.len(),
        };
        let Some(dst) = buf.get_mut(..want) else {
            return Ok(0);
        };
        let n = self.inner.read(dst)?;
        self.pos = self.pos.saturating_add(n as u64);
        Ok(n)
    }

    fn seek(&mut self, pos: u64) -> vaco_core::Result<u64> {
        let target = self.start.saturating_add(pos);
        let clamped = self.end.map_or(target, |e| target.min(e));
        let at = self.inner.seek(clamped)?;
        self.pos = at.saturating_sub(self.start);
        Ok(self.pos)
    }

    fn position(&self) -> u64 {
        self.pos
    }

    fn size(&self) -> Option<u64> {
        match self.end {
            Some(e) => Some(e.saturating_sub(self.start)),
            None => self
                .inner
                .size()
                .map(|total| total.saturating_sub(self.start)),
        }
    }

    fn seekability(&self) -> Seekability {
        self.inner.seekability()
    }

    fn peek(&mut self, len: usize) -> vaco_core::Result<&[u8]> {
        let want = match self.remaining() {
            Some(r) => len.min(usize::try_from(r).unwrap_or(usize::MAX)),
            None => len,
        };
        self.inner.peek(want)
    }
}

/// The `subfile:` protocol. Read-only: a byte range only makes sense to carve
/// out of something that already exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubfileProtocol;

impl Protocol for SubfileProtocol {
    fn open(
        &self,
        url: &Url,
        flags: IoFlags,
        opts: &Dict,
        env: &ProtocolEnv<'_>,
    ) -> Result<Box<dyn MediaSource>> {
        let range = parse_args(&url.args)?;
        // Same `env`: the depth increment and the whitelist check both happen
        // inside `registry.open`, exactly once, for this one nested open.
        let inner = env.registry.open(&url.rest, flags, opts, env)?;
        let windowed = SubfileSource::new(inner, range)?;
        Ok(Box::new(windowed))
    }
}

/// The registry entry for `subfile:`.
pub static SUBFILE_PROTOCOL: ProtocolDesc = ProtocolDesc {
    name: "subfile",
    long_name: "subfile",
    flags: ProtocolFlags {
        network: false,
        nested_scheme: true,
        server_capable: false,
    },
    // Measured: no implicit grant. See the crate docs.
    default_whitelist: &[],
    options: None,
    proto: &SubfileProtocol,
};

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    #[test]
    fn measured_grammar_examples_parse() {
        assert_eq!(
            parse_args(",,start,1024,end,4096,,").unwrap(),
            Range {
                start: 1024,
                end: Some(4096)
            }
        );
        // Order does not matter.
        assert_eq!(
            parse_args(",,end,4096,start,1024,,").unwrap(),
            Range {
                start: 1024,
                end: Some(4096)
            }
        );
        // `end` omitted means through EOF.
        assert_eq!(
            parse_args(",,start,100,,").unwrap(),
            Range {
                start: 100,
                end: None
            }
        );
        // `end` present as its zero default also means through EOF.
        assert_eq!(
            parse_args(",,start,0,end,0,,").unwrap(),
            Range {
                start: 0,
                end: None
            }
        );
    }

    #[test]
    fn single_comma_form_is_refused() {
        // Measured: `subfile,start,0,end,10:x` -> "Error parsing options
        // string". `Url::args` for that spelling is `,start,0,end,10`.
        assert!(parse_args(",start,0,end,10").is_err());
    }

    #[test]
    fn end_before_start_is_refused() {
        assert!(parse_args(",,start,300,end,100,,").is_err());
    }

    #[test]
    fn malformed_shapes_are_rejected_not_panicking() {
        for bad in [
            "",
            ",",
            ",,",
            ",,start,,",
            ",,start,abc,,",
            ",,bogus,1,,",
            ",,start,1,end,,",
        ] {
            assert!(parse_args(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn byte_range_is_exact_and_exclusive_of_end() {
        let data: Vec<u8> = (0u8..=255).collect();
        let src = MemorySource::new(data.clone());
        let mut sub = SubfileSource::new(
            Box::new(src),
            Range {
                start: 100,
                end: Some(150),
            },
        )
        .unwrap();
        let mut got = vec![0u8; 50];
        sub.read_exact(&mut got).unwrap();
        assert_eq!(got, data[100..150]);
        // The window is exhausted exactly at 50 bytes.
        assert_eq!(sub.read(&mut [0u8; 8]).unwrap(), 0);
    }

    #[test]
    fn start_beyond_the_source_yields_empty_not_an_error() {
        let src = MemorySource::new(vec![1, 2, 3]);
        let mut sub = SubfileSource::new(
            Box::new(src),
            Range {
                start: 1000,
                end: None,
            },
        )
        .unwrap();
        assert_eq!(sub.read(&mut [0u8; 8]).unwrap(), 0);
    }

    #[test]
    fn seek_is_relative_to_the_window_and_clamped_to_end() {
        let data: Vec<u8> = (0u8..=255).collect();
        let src = MemorySource::new(data);
        let mut sub = SubfileSource::new(
            Box::new(src),
            Range {
                start: 10,
                end: Some(20),
            },
        )
        .unwrap();
        sub.seek(5).unwrap();
        assert_eq!(sub.r8_for_test(), 15);
        // Seeking past the window's own end clamps to it, not to the inner
        // source's end.
        sub.seek(1000).unwrap();
        assert_eq!(sub.read(&mut [0u8; 1]).unwrap(), 0);
    }

    #[test]
    fn no_end_reports_size_relative_to_the_inner_source() {
        let src = MemorySource::new(vec![0u8; 1000]);
        let sub = SubfileSource::new(
            Box::new(src),
            Range {
                start: 100,
                end: None,
            },
        )
        .unwrap();
        assert_eq!(sub.size(), Some(900));
    }

    impl SubfileSource {
        /// Test helper: read one byte after a seek, for asserting position.
        fn r8_for_test(&mut self) -> u8 {
            let mut b = [0u8; 1];
            self.read_exact(&mut b).unwrap();
            b[0]
        }
    }

    proptest::proptest! {
        #[test]
        fn args_round_trip_through_the_url_splitter(start in 0u64..1_000_000, end in 0u64..1_000_000) {
            let (lo, hi) = if start <= end { (start, end) } else { (end, start) };
            let url_str = format!("subfile,,start,{lo},end,{hi},,:inner.bin");
            let u = vaco_protocol_core::split_url(&url_str);
            proptest::prop_assert_eq!(u.to_string(), url_str.clone());
            let range = parse_args(&u.args).unwrap();
            proptest::prop_assert_eq!(range.start, lo);
            if hi == 0 {
                proptest::prop_assert_eq!(range.end, None);
            } else {
                proptest::prop_assert_eq!(range.end, Some(hi));
            }
        }
    }
}
