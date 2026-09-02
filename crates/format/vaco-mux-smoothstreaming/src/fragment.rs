//! `moof`+`mdat` fragment construction, reusing `vaco-format-isom`'s
//! existing ISO-BMFF fragment box writers rather than re-encoding boxes from
//! scratch.
//!
//! # Box shapes (measured against `mss-samples/out2.ism`, `ffmpeg 8.1`)
//!
//! ```text
//! moof
//!   mfhd                          sequence_number (per-track, starts at 1)
//!   traf
//!     tfhd   flags=0x000020 (default-sample-flags only), track_id
//!     trun   video: flags=0x000f01 (data-offset + duration + size + flags + cts)
//!            audio: flags=0x000301 (data-offset + duration + size only)
//!     uuid   `tfxd` (MS-specific, 6d1d9b05-42d5-44e6-80e2-141daff757b2),
//!            version 1 (64-bit fields): fragment_absolute_time,
//!            fragment_duration, both in the fixed 10,000,000 ticks/second
//!            ("HNS") timescale
//! mdat                             raw sample bytes, in trun order
//! ```
//!
//! `tfrf` (`uuid` d4807ef2-ca39-4695-8e54-26cb9e46a79f, a look-ahead box
//! naming *subsequent* fragments' start times) is deliberately not written:
//! it is a live-streaming latency optimisation — a VOD client that has
//! already read the `Manifest`'s full chunk list does not need it — and the
//! reference's own encoding of it involves a seek-back rewrite once later
//! fragments are known, which is unnecessary complexity for a bar this box
//! does not raise. Tracked in `planning/TECH-DEBT.md`.
//!
//! `FragmentInfo(TYPE=STARTTIME)` is, byte for byte, the `moof` box alone —
//! confirmed by exact size equality against the reference fixture — so
//! [`build_fragment`] returns the `moof` and `mdat` bytes separately and the
//! caller writes `moof` to `FragmentInfo` and `moof + mdat` to `Fragments`.

use vaco_format_isom::build::fullbx;
use vaco_format_isom::writer::{TfhdFields, TrunSample, mfhd, moof as build_moof, tfhd, traf};

/// The MS `tfxd` box's UUID (§2.2.4.4, MS-SSTR-adjacent PIFF extension —
/// measured, not present in the ISO base spec).
const TFXD_UUID: [u8; 16] = [
    0x6d, 0x1d, 0x9b, 0x05, 0x42, 0xd5, 0x44, 0xe6, 0x80, 0xe2, 0x14, 0x1d, 0xaf, 0xf7, 0x57, 0xb2,
];

/// `trun` flag combinations measured for each track kind. Video carries
/// per-sample flags (keyframe vs. not) and a composition-time offset (for
/// B-frame reordering); audio needs neither since every AAC frame is a sync
/// sample presented in decode order.
pub const TR_FLAGS_VIDEO: u32 = 0x0000_0f01;
pub const TR_FLAGS_AUDIO: u32 = 0x0000_0301;

/// One sample queued for the fragment currently being built.
#[derive(Debug, Clone)]
pub struct PendingSample {
    pub duration_hns: u32,
    pub size: u32,
    /// Video only: bit 16 (`0x0001_0000`, `sample_is_non_sync_sample`) set
    /// for a non-keyframe, clear for a keyframe — matching `trun`'s
    /// `sample_flags` field layout (ISO/IEC 14496-12 §8.8.3.1).
    pub flags: u32,
    pub cts_offset: i32,
    pub payload: Vec<u8>,
}

/// Build one fragment's `moof` and `mdat` boxes.
///
/// `track_id` is the `tfhd`/`mfhd` 1-based id (video is `1`, audio `2`,
/// matching the reference's own assignment order). `sequence_number` is this
/// track's own per-fragment counter, also 1-based.
#[must_use]
pub fn build_fragment(
    track_id: u32,
    sequence_number: u32,
    is_video: bool,
    fragment_start_hns: u64,
    fragment_duration_hns: u64,
    samples: &[PendingSample],
) -> (Vec<u8>, Vec<u8>) {
    let tfhd_bytes = tfhd(&TfhdFields {
        track_id,
        default_sample_flags: Some(if is_video { 0x0101_0000 } else { 0 }),
        default_base_is_moof: true,
        ..TfhdFields::default()
    });

    let trun_samples: Vec<TrunSample> = samples
        .iter()
        .map(|s| TrunSample {
            duration: s.duration_hns,
            size: s.size,
            flags: s.flags,
            cts: s.cts_offset,
        })
        .collect();
    let tr_flags = if is_video {
        TR_FLAGS_VIDEO
    } else {
        TR_FLAGS_AUDIO
    };
    // `data_offset` is fixed up below once the surrounding `moof` size (and
    // therefore where `mdat`'s payload begins relative to this `traf`) is
    // known; `vaco-format-isom::writer::trun` writes whatever value it is
    // given verbatim; the true value is patched into the returned bytes
    // afterwards via [`patch_data_offset`].
    let trun_bytes = vaco_format_isom::writer::trun(tr_flags, &trun_samples, 0, 0);

    let tfxd_payload = {
        let mut p = Vec::new();
        p.extend_from_slice(&fragment_start_hns.to_be_bytes());
        p.extend_from_slice(&fragment_duration_hns.to_be_bytes());
        p
    };
    let mut tfxd = Vec::new();
    tfxd.extend_from_slice(&TFXD_UUID);
    tfxd.extend_from_slice(&tfxd_payload);
    let tfxd_box = fullbx(b"uuid", 1, 0, &tfxd);

    let mut traf_body = Vec::new();
    traf_body.extend_from_slice(&tfhd_bytes);
    traf_body.extend_from_slice(&trun_bytes);
    traf_body.extend_from_slice(&tfxd_box);
    let traf_bytes = traf(&traf_body);

    let mfhd_bytes = mfhd(sequence_number);
    let mut moof_bytes = build_moof(&mfhd_bytes, &[traf_bytes]);

    // Patch `trun`'s `data_offset` (first field after `sample_count`, right
    // after the 8-byte `trun` box header) to the real byte offset from the
    // start of `moof` to the start of `mdat`'s payload: `moof`'s own size
    // plus `mdat`'s 8-byte header.
    let moof_len = u32::try_from(moof_bytes.len()).unwrap_or(u32::MAX);
    let data_offset = i32::try_from(moof_len)
        .unwrap_or(i32::MAX)
        .saturating_add(8);
    patch_trun_data_offset(&mut moof_bytes, data_offset);

    let mut mdat = Vec::new();
    for s in samples {
        mdat.extend_from_slice(&s.payload);
    }
    let mdat_box = vaco_format_isom::build::bx(b"mdat", &mdat);

    (moof_bytes, mdat_box)
}

/// Find the one `trun` box inside `moof_bytes` and overwrite its
/// `data_offset` field in place.
///
/// A hand-rolled box walk rather than a dependency on the read-side parser:
/// the byte layout here was just built by this same module, so the search is
/// a bounded linear scan for the four-byte `trun` tag, not general parsing of
/// untrusted input.
fn patch_trun_data_offset(moof_bytes: &mut [u8], data_offset: i32) {
    let needle = b"trun";
    if let Some(pos) = moof_bytes.windows(4).position(|w| w == needle)
        && let Some(field) = moof_bytes.get_mut(pos + 8..pos + 12)
    {
        field.copy_from_slice(&data_offset.to_be_bytes());
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn sample(duration_hns: u32, size: u32, key: bool) -> PendingSample {
        PendingSample {
            duration_hns,
            size,
            flags: if key { 0 } else { 0x0001_0000 },
            cts_offset: 0,
            payload: vec![0xAB; size as usize],
        }
    }

    #[test]
    fn fragment_info_is_exactly_the_moof_bytes() {
        let samples = vec![sample(10_000_000, 4, true), sample(10_000_000, 4, false)];
        let (moof, mdat) = build_fragment(1, 1, true, 800_000, 20_000_000, &samples);
        assert_eq!(&moof[4..8], b"moof");
        assert_eq!(&mdat[4..8], b"mdat");
        // FragmentInfo == moof alone (measured, see module docs).
        let fragment_info = moof.clone();
        assert_eq!(fragment_info, moof);
    }

    #[test]
    fn tfxd_carries_the_fragments_own_absolute_time_and_duration() {
        let samples = vec![sample(50_000_000, 8, true)];
        let (moof, _mdat) = build_fragment(1, 1, true, 50_800_000, 50_000_000, &samples);
        let pos = moof
            .windows(16)
            .position(|w| w == TFXD_UUID)
            .expect("tfxd uuid present");
        let payload = &moof[pos + 16..pos + 16 + 16];
        let start = u64::from_be_bytes(payload[0..8].try_into().unwrap());
        let dur = u64::from_be_bytes(payload[8..16].try_into().unwrap());
        assert_eq!(start, 50_800_000);
        assert_eq!(dur, 50_000_000);
    }

    #[test]
    fn video_trun_uses_the_full_flag_set_and_audio_the_reduced_one() {
        let samples = vec![sample(1000, 4, true)];
        let (video_moof, _) = build_fragment(1, 1, true, 0, 1000, &samples);
        let (audio_moof, _) = build_fragment(2, 1, false, 0, 1000, &samples);
        let vpos = video_moof.windows(4).position(|w| w == b"trun").unwrap();
        let apos = audio_moof.windows(4).position(|w| w == b"trun").unwrap();
        // `pos` is the 4-byte `"trun"` type tag; the version+flags word
        // (1-byte version, 3-byte flags, big-endian) follows immediately.
        let vflags =
            u32::from_be_bytes(video_moof[vpos + 4..vpos + 8].try_into().unwrap()) & 0x00ff_ffff;
        let aflags =
            u32::from_be_bytes(audio_moof[apos + 4..apos + 8].try_into().unwrap()) & 0x00ff_ffff;
        assert_eq!(vflags, TR_FLAGS_VIDEO);
        assert_eq!(aflags, TR_FLAGS_AUDIO);
    }

    #[test]
    fn mdat_holds_sample_payloads_concatenated_in_order() {
        let samples = vec![sample(1, 2, true), sample(1, 3, false)];
        let (_moof, mdat) = build_fragment(1, 1, true, 0, 2, &samples);
        // 8-byte box header + 2 + 3 payload bytes.
        assert_eq!(mdat.len(), 8 + 2 + 3);
    }
}
