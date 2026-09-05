//! `pcm_rechunk`: re-slice a raw-PCM packet stream into fixed-size chunks.
//!
//! # What is measured, not assumed
//!
//! `ffmpeg -h bsf=pcm_rechunk` declares three options: `nb_out_samples`
//! (default `1024`), `pad` (default `true`), `frame_rate` (default `0/1`,
//! meaning unused). [`vaco_format_core::mux::BsfProvider::open`] has no
//! per-instance option string, so this crate can only ever build the
//! bare-name, default-option behaviour —
//! `1024`-sample chunks, zero-padded.
//!
//! Measured directly: a 2205-sample mono `pcm_s16le` stream (0.05 s at
//! 44100 Hz) through bare `-bsf:a pcm_rechunk` produced three packets of
//! exactly `1024` samples each (`pts` `0`, `1024`, `2048`) — `2205` real
//! samples followed by `867` zero-valued padding samples in the last packet,
//! confirmed byte for byte: the first `2205 * 2` bytes of the concatenated
//! output equal the original raw bytes exactly, and every byte after that is
//! `0x00`. This is a plain re-slice of the interleaved byte stream, not a
//! resample: no bytes are reordered or recomputed, only regrouped.
//!
//! # Frame size
//!
//! `nb_out_samples` counts *frames* (one sample per channel), so a chunk is
//! `1024 * channels * bytes_per_sample` bytes. `bytes_per_sample` is looked
//! up from the codec id — the subset of `ffmpeg -h bsf=pcm_rechunk`'s
//! `Supported codecs` line this workspace's [`CodecId`] has variants for
//! (`pcm_f16le`, `pcm_f24le`, `pcm_s24daud`, `pcm_s64le/be` and `pcm_sga`
//! are in the reference's list but have no `CodecId` here — refused, not
//! guessed at).
//!
//! # Timestamps
//!
//! `pts`/`duration` are derived assuming the container's time base is
//! `1 / sample_rate` — true of every raw-PCM producer in this workspace
//! (`vaco-demux-raw`'s `pcm` module states it directly), which is what makes
//! "advance `pts` by `nb_out_samples` ticks per chunk" the same thing as
//! "advance by `nb_out_samples` samples". A caller whose PCM stream uses a
//! different time base (this workspace never produces one) would get a
//! `pts` that no longer lines up — a documented assumption, not a silent one.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "pcm_rechunk",
    long_name: "PCM packet reformatting",
    build,
};

/// The reference's default `nb_out_samples`.
const NB_OUT_SAMPLES: u64 = 1024;

/// Bytes one sample occupies, for the [`CodecId`] variants this crate covers.
/// `None` for anything `ffmpeg -h bsf=pcm_rechunk` lists that has no
/// [`CodecId`] variant in this workspace.
fn bytes_per_sample(id: CodecId) -> Option<usize> {
    Some(match id {
        CodecId::PcmU8
        | CodecId::PcmS8
        | CodecId::PcmAlaw
        | CodecId::PcmMulaw
        | CodecId::PcmVidc => 1,
        CodecId::PcmS16le | CodecId::PcmS16be | CodecId::PcmU16le | CodecId::PcmU16be => 2,
        CodecId::PcmS24le | CodecId::PcmS24be | CodecId::PcmU24le | CodecId::PcmU24be => 3,
        CodecId::PcmS32le
        | CodecId::PcmS32be
        | CodecId::PcmU32le
        | CodecId::PcmU32be
        | CodecId::PcmF32le
        | CodecId::PcmF32be => 4,
        CodecId::PcmF64le | CodecId::PcmF64be => 8,
        _ => return None,
    })
}

fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    let Some(id) = params.codec_id else {
        return Err(Error::Unsupported("pcm_rechunk: no codec id"));
    };
    let Some(sample_bytes) = bytes_per_sample(id) else {
        return Err(Error::Unsupported("pcm_rechunk: not a covered PCM codec"));
    };
    let channels = params
        .audio
        .as_ref()
        .and_then(|a| a.layout.as_ref())
        .map_or(1, |l| l.channels.max(1));
    let sample_rate = params.audio.as_ref().map_or(0, |a| a.sample_rate);
    let frame_bytes = sample_bytes * channels as usize;
    Ok(Box::new(MappedFilter::new(PcmRechunk {
        frame_bytes,
        sample_rate,
        buffer: Vec::new(),
        base: None,
        chunks_emitted: 0,
        budget: Budget::new(Limits::permissive()),
    })))
}

struct PcmRechunk {
    /// Bytes one frame (one sample per channel) occupies.
    frame_bytes: usize,
    sample_rate: u32,
    buffer: Vec<u8>,
    /// The metadata of the first packet ever seen — every emitted chunk's
    /// `stream_index`/`flags` come from here, and its `pts` anchors the
    /// running per-chunk timestamp (see the module docs on the time-base
    /// assumption this rests on).
    base: Option<Packet>,
    chunks_emitted: u64,
    budget: Budget,
}

impl PcmRechunk {
    fn chunk_bytes(&self) -> usize {
        self.frame_bytes
            .saturating_mul(usize::try_from(NB_OUT_SAMPLES).unwrap_or(usize::MAX))
    }

    fn emit_chunk(&mut self, bytes: &[u8], out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(base) = self.base.as_ref() else {
            return Ok(());
        };
        let mut np = Packet::from_slice(&mut self.budget, bytes)?;
        np.stream_index = base.stream_index;
        np.flags = base.flags;
        let offset_ticks = self.chunks_emitted.saturating_mul(NB_OUT_SAMPLES);
        np.pts = base.pts.ticks().map_or(Timestamp::NONE, |t| {
            Timestamp::new(t.saturating_add_unsigned(offset_ticks))
        });
        np.dts = np.pts;
        np.duration = i64::try_from(NB_OUT_SAMPLES)
            .ok()
            .zip(i32::try_from(self.sample_rate).ok())
            .and_then(|(ticks, rate)| Duration::from_ticks(ticks, Rational::new(1, rate)))
            .unwrap_or(Duration::ZERO);
        out.push_back(np);
        self.chunks_emitted = self.chunks_emitted.saturating_add(1);
        Ok(())
    }
}

impl PacketMap for PcmRechunk {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else {
            // End of stream: pad the remainder (default `pad=true`) and
            // flush it as a final, short-of-nominal chunk.
            if !self.buffer.is_empty() {
                let chunk_bytes = self.chunk_bytes();
                self.buffer.resize(chunk_bytes, 0);
                let bytes = std::mem::take(&mut self.buffer);
                self.emit_chunk(&bytes, out)?;
            }
            return Ok(());
        };
        if self.base.is_none() {
            self.base = Some(p.clone());
        }
        self.buffer.extend_from_slice(p.payload());
        let chunk_bytes = self.chunk_bytes();
        if chunk_bytes == 0 {
            return Ok(());
        }
        while self.buffer.len() >= chunk_bytes {
            let rest = self.buffer.split_off(chunk_bytes);
            let bytes = std::mem::replace(&mut self.buffer, rest);
            self.emit_chunk(&bytes, out)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_chlayout::ChannelLayout;
    use vaco_codec_core::AudioParameters;

    fn params(sample_rate: u32) -> CodecParameters {
        let mut p = CodecParameters::audio().with_codec(CodecId::PcmS16le);
        p.audio = Some(AudioParameters {
            sample_rate,
            layout: Some(ChannelLayout::MONO),
            ..AudioParameters::default()
        });
        p
    }

    fn budget() -> Budget {
        Budget::new(Limits::strict())
    }

    fn pkt(bytes: &[u8]) -> Packet {
        Packet::from_slice(&mut budget(), bytes).unwrap()
    }

    /// The measured shape: 2205 mono s16le samples in, three 1024-sample
    /// chunks out, the tail zero-padded, no bytes reordered.
    #[test]
    fn a_short_stream_is_rechunked_and_the_tail_is_zero_padded() {
        let samples: Vec<i16> = (0..2205).map(|i| i as i16).collect();
        let mut raw = Vec::new();
        for s in &samples {
            raw.extend_from_slice(&s.to_le_bytes());
        }
        let mut input = pkt(&raw);
        input.pts = vaco_core::Timestamp::new(0);
        let mut f = (DESC.build)(&params(44_100)).unwrap();
        f.send_packet(Some(&input)).unwrap();
        f.send_packet(None).unwrap();

        let mut collected = Vec::new();
        let mut pts_seen = Vec::new();
        while let Ok(p) = f.receive_packet() {
            assert_eq!(p.duration.as_ratio(), (256, 11_025));
            pts_seen.push(p.pts.ticks());
            collected.extend_from_slice(p.payload());
        }
        assert_eq!(pts_seen, vec![Some(0), Some(1024), Some(2048)]);
        assert_eq!(collected.len(), 3 * 1024 * 2);
        assert_eq!(&collected[..raw.len()], raw.as_slice());
        assert!(collected[raw.len()..].iter().all(|&b| b == 0));
    }

    /// A stream that divides evenly needs no padding and must not gain an
    /// extra all-zero chunk.
    #[test]
    fn an_exact_multiple_needs_no_padding() {
        let raw = vec![0xABu8; 1024 * 2 * 2]; // exactly two chunks, s16le mono
        let mut f = (DESC.build)(&params(44_100)).unwrap();
        f.send_packet(Some(&pkt(&raw))).unwrap();
        f.send_packet(None).unwrap();
        let mut count = 0;
        while f.receive_packet().is_ok() {
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[test]
    fn stereo_doubles_the_frame_size() {
        let mut p = params(44_100);
        p.audio.as_mut().unwrap().layout = Some(ChannelLayout::STEREO);
        let raw = vec![0x11u8; 1024 * 2 * 2]; // exactly one stereo chunk
        let mut f = (DESC.build)(&p).unwrap();
        f.send_packet(Some(&pkt(&raw))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload().len(), 1024 * 2 * 2);
        assert!(
            f.receive_packet().is_err(),
            "must not have split a single chunk"
        );
    }

    #[test]
    fn an_uncovered_pcm_variant_is_refused() {
        // `Pcm` (the bulk/generic variant) carries no width this filter can
        // size a chunk from.
        let params = CodecParameters::audio().with_codec(CodecId::Pcm);
        assert!((DESC.build)(&params).is_err());
    }

    /// Falsifies "no padding is needed" by checking the naive alternative
    /// (truncating the short last chunk instead of padding it) really would
    /// produce a different byte count than what is measured.
    #[test]
    fn falsified_truncating_the_last_chunk_would_be_a_different_size() {
        let padded_len = 3 * 1024 * 2;
        let truncated_len = 2205 * 2;
        assert_ne!(padded_len, truncated_len);
    }
}
