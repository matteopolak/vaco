//! AC-3 / E-AC-3 as a resynchronising byte-stream
//! [`Parser`](vaco_codec_core::Parser).
//!
//! `vaco_format_ac3::syncinfo::parse` already tells classic AC-3 and E-AC-3
//! apart from `bsid` and reports the frame length and sample rate for both;
//! `vaco_format_ac3::bsi::Bsi::parse` reads `acmod`/`lfeon` for the channel
//! layout. Nothing here re-derives either — see the crate docs.
//!
//! The sync word is the whole sixteen bits of `0x0B77`, so a candidate needs
//! no separate structural validation the way MPEG audio's eleven-bit sync
//! does; the resync scan still advances one byte of the pair at a time (see
//! [`advance_to_sync`]) so a sync word split across two `parse` calls is
//! never lost.

use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, Parser};
use vaco_core::{Error, Result};
use vaco_format_ac3::bsi::Bsi;
use vaco_format_ac3::syncinfo::{self, FrameKind, SyncInfo};
use vaco_format_ac3::tables::acmod_layout;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// Bytes [`syncinfo::parse`] needs at minimum: the sync word, `crc1`/the
/// E-AC-3 header fields, and the `bsid` byte at offset 5.
const MIN_HEADER_LEN: usize = 6;

fn codec_for_kind(kind: FrameKind) -> CodecId {
    match kind {
        FrameKind::Ac3 => CodecId::Ac3,
        FrameKind::Eac3 => CodecId::Eac3,
    }
}

/// Fold a syncframe into the parameters a container reports.
///
/// `bit_rate` is computed from the frame's own size rather than looked up in
/// a table, which is the only way to state one for E-AC-3: its `frmsiz` is a
/// free byte count with no companion bit-rate table the way classic AC-3's
/// `frmsizecod` has. `frame_size` bytes at `samples` per `sample_rate`
/// seconds is the same arithmetic ffprobe's own number falls out of,
/// measured on a `-c:a eac3` encode with no explicit `-b:a`.
///
/// `sample_fmt` is `fltp`, measured the same way as `vaco-parse-aac` and
/// `vaco-parse-opus` document for their own codecs.
#[allow(
    clippy::integer_division,
    reason = "an average bit rate is an intentional floor over one frame, not a precision loss"
)]
fn to_codec_parameters(info: &SyncInfo, bsi: Option<&Bsi>) -> CodecParameters {
    let mut params = CodecParameters::audio().with_codec(codec_for_kind(info.kind));
    if info.samples > 0 {
        let bits = (info.frame_size as u64).saturating_mul(8);
        params.bit_rate =
            Some(bits.saturating_mul(u64::from(info.sample_rate)) / u64::from(info.samples));
    }
    let layout = bsi.map(|b| acmod_layout(b.acmod, b.lfeon));
    params.audio = Some(AudioParameters {
        sample_rate: info.sample_rate,
        format: Some(vaco_sampfmt::SampleFmt::F32P),
        layout,
        bits_per_coded_sample: None,
        bits_per_raw_sample: None,
        initial_padding: 0,
    });
    params
}

/// Splits an AC-3/E-AC-3 byte stream into syncframes.
#[derive(Debug)]
pub struct Ac3Parser {
    info: Option<SyncInfo>,
    params: Option<CodecParameters>,
    budget: Budget,
    deferred: Vec<u8>,
    synced: bool,
    frames: u64,
    resyncs: u64,
}

impl Ac3Parser {
    /// A parser that allocates packets against `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            info: None,
            params: None,
            budget: Budget::new(limits),
            deferred: Vec::new(),
            synced: false,
            frames: 0,
            resyncs: 0,
        }
    }

    /// The most recently accepted frame's `syncinfo()`.
    #[must_use]
    pub const fn sync_info(&self) -> Option<&SyncInfo> {
        self.info.as_ref()
    }

    /// Frames emitted so far.
    #[must_use]
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// How many times the parser lost sync and had to scan for a new header.
    #[must_use]
    pub const fn resyncs(&self) -> u64 {
        self.resyncs
    }

    fn accept(&mut self, frame: &[u8], info: SyncInfo) {
        if !self.synced {
            self.resyncs = self.resyncs.saturating_add(1);
        }
        let changed = self.info.map(|i| i.frame_size) != Some(info.frame_size)
            || self.info.map(|i| i.sample_rate) != Some(info.sample_rate)
            || self.info.map(|i| i.kind) != Some(info.kind);
        if changed {
            // A malformed `bsi()` (truncated frame, reserved field) does not
            // invalidate the framing `syncinfo()` already established — it
            // only means this particular frame cannot describe its own
            // channel layout, so parameters fall back to whatever an earlier
            // frame or the container already reported.
            let bsi = Bsi::parse(frame, &info).ok();
            self.params = Some(to_codec_parameters(&info, bsi.as_ref()));
        }
        self.info = Some(info);
        self.synced = true;
        self.frames = self.frames.saturating_add(1);
    }
}

impl Parser for Ac3Parser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if input.is_empty() {
            if self.deferred.is_empty() {
                return Ok((None, 0));
            }
            let Some(info) = syncinfo::parse(&self.deferred) else {
                self.deferred.clear();
                return Ok((None, 0));
            };
            let mut packet = Packet::from_slice(&mut self.budget, &self.deferred)?;
            packet.flags = PacketFlags::KEY;
            let frame = std::mem::take(&mut self.deferred);
            self.accept(&frame, info);
            return Ok((Some(packet), 0));
        }
        self.deferred.clear();

        let mut i = 0usize;
        while let Some(rest) = input.get(i..) {
            if rest.len() < MIN_HEADER_LEN {
                break;
            }
            let Some(info) = syncinfo::parse(rest) else {
                self.synced = false;
                i = advance_to_sync(input, i + 1);
                continue;
            };
            if info.frame_size < MIN_HEADER_LEN {
                self.synced = false;
                i = advance_to_sync(input, i + 1);
                continue;
            }
            if rest.len() < info.frame_size {
                return Ok((None, i));
            }
            if !self.synced {
                let next = rest.get(info.frame_size..).unwrap_or_default();
                match next {
                    [a, b, ..] if [*a, *b] != syncinfo::SYNCWORD => {
                        i = advance_to_sync(input, i + 1);
                        continue;
                    }
                    [a] if *a != syncinfo::SYNCWORD[0] => {
                        i = advance_to_sync(input, i + 1);
                        continue;
                    }
                    [] | [_] => {
                        if let Some(frame) = rest.get(..info.frame_size) {
                            self.deferred.extend_from_slice(frame);
                        }
                        return Ok((None, i));
                    }
                    _ => {}
                }
            }

            let Some(frame) = rest.get(..info.frame_size) else {
                return Err(Error::InvalidData("AC-3 frame slice out of range"));
            };
            let mut packet = Packet::from_slice(&mut self.budget, frame)?;
            packet.flags = PacketFlags::KEY;
            self.accept(frame, info);
            return Ok((Some(packet), i + info.frame_size));
        }
        Ok((None, i.min(input.len())))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    /// `samples / sample_rate` off the frame's own `syncinfo()`.
    fn packet_duration(&self, packet: &[u8]) -> Option<vaco_core::Rational> {
        let info = syncinfo::parse(packet)?;
        let samples = i32::try_from(info.samples).ok()?;
        let rate = i32::try_from(info.sample_rate).ok()?;
        if samples <= 0 || rate <= 0 {
            return None;
        }
        Some(vaco_core::Rational::new(samples, rate))
    }
}

/// The next offset at or after `from` that could open a sync word.
///
/// Searches for the sync word's **first byte only**, not the full two-byte
/// pair. A pair search that finds no match in the searched window reports
/// "consume it all" — which is wrong whenever the window's last byte is
/// `0x0B` and the `0x77` that would complete it has not arrived yet: a
/// single `parse` call only ever sees one chunk, so discarding that byte
/// loses the sync word across the boundary permanently. Searching for one
/// byte cannot straddle anything, matches
/// [`vaco_parse_aac`](https://docs.rs/vaco-parse-aac)'s `AdtsParser` and
/// [`crate::mpegaudio::MpegAudioParser`]'s own resync, and is what a fuzz
/// target byte-feeding this parser one chunk at a time caught: the two-byte
/// version silently dropped syncframes whose sync word landed on a chunk
/// boundary.
fn advance_to_sync(input: &[u8], from: usize) -> usize {
    match input.get(from..) {
        Some(rest) => {
            from + rest
                .iter()
                .position(|&b| b == syncinfo::SYNCWORD[0])
                .unwrap_or(rest.len())
        }
        None => input.len(),
    }
}
