//! Differential test against the `alac` dev-dependency oracle.
//!
//! This is the one place in this crate that touches the `alac` crate at
//! all, and it is used exactly as `Cargo.toml`'s doc comment on the
//! dependency and this crate's own top-level doc describe: as a black box.
//! No source file of the `alac` crate is read anywhere in this repository —
//! only its public `StreamInfo`/`Decoder` API, called here with real bytes
//! and compared against real output.
//!
//! # What this validates, and what it does not
//!
//! - **Does validate**: this crate's `AlacCookie` parser derives the same
//!   sample rate, channel count and bit depth from a real magic cookie that
//!   an independent decoder (`alac::StreamInfo::from_cookie`) derives from
//!   the identical bytes. That is a genuine two-implementation cross-check
//!   of the one part of this crate that is spec-derived rather than
//!   original (see `cookie.rs`).
//! - **Does not validate** bitstream compatibility of the compressed audio
//!   payload: this crate's `frame_codec` is its own original framing (see
//!   the crate's top-level doc), so it cannot and does not attempt to
//!   decode `alac`'s packet bytes as its own payload. Instead, the real
//!   packet is decoded once by the oracle to obtain genuine, real-world
//!   PCM content, which is then round-tripped through *this* crate's own
//!   encoder and decoder — a realistic-signal round-trip, not an
//!   interop claim.
//!
//! This whole file is test code exercising the oracle and this crate's own
//! public API with known-good fixtures, not the untrusted-input surface the
//! workspace's `unwrap_used`/`expect_used`/`indexing_slicing` lints protect.
//!
//! The cookie and packet bytes below were extracted 2026-08-28 from
//! `ffmpeg -f lavfi -i "sine=frequency=440:duration=0.2" -ac 1 -c:a alac
//! mono.m4a`'s `stsd`/`stsz`/`stco`/`mdat` boxes via a throwaway Python MP4
//! box walk (not any container-parsing crate) — see the crate's closing
//! report for the exact recipe. `cookie.rs`'s `real_ffmpeg_mono_cookie` test
//! pins the same cookie bytes independently.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "integration test code over trusted fixture data"
)]

use vaco_chlayout::ChannelLayout;
use vaco_codec_alac::{AlacCookie, AlacDecoder, AlacEncoder};
use vaco_codec_core::{Decoder, Encoder};
use vaco_core::Timestamp;
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_sampfmt::SampleFmt;

/// The real 24-byte `ALACSpecificConfig` from `mono.m4a`'s `stsd` box.
const REAL_COOKIE_HEX: &str = "000010000010280a0e01000000002004000ac4400000ac44";
/// The real first ALAC packet (4096 mono samples) from `mono.m4a`'s `mdat`.
const REAL_PACKET_HEX: &str = "00000000000f0c01840033ff9fff85ffbe006a0ff80ffffc07fbfc2ff7ddc3b401000302000010304082840c1430610400\
82084186438c61886618c4332052693384274232585658740a840da29ddda498936e26deaaf2f705575aa809b013943069\
440a889898006838e38d345686e3138c4ef513121b70686ab95c60d0e571cad55b63b18d660eec7a9821a7071c1dee0e3b\
da680000005bfa5b490304c4c9579c2ad8038e340382a02a2bb72a368ad31c55ade98c8e571a06e0aa0dc1356ae524d906\
12b4c4dda63802c881a626863bd4718860d0d56ab5575adeb2272aeb55131a6950ddc2538edb05490d56984c91c71aab2a\
03831d5391380544d34c1c165a1aae5718e0a921a18d1504a92681aae32572b4c705575a75a006098255a92c076d55d69b\
44ababa6870ae310c69149c1da4039486956b122b4d500e355c2b4dc1a71b8cc7242a5204c55aab60d015d83834d34c130\
4ef52503951371c1cae3059aabddd5d4695201a1acaab4a0a944cae265762cba0556da001c6801c31dcb4aa54434c55cae\
3704c680062189a13949ea40625574934ebd2abce65d5eeea0ef10c5529307090690da02b8d5698260d02dc13132a38374\
3b51ca1030437059ccd5355c5bbc8aaea2706329a490080ad31572b55cae52ad3701a551340e34392d524d380c8c156aae\
aa0359aad31a183836ab2a0243b6ef22ad5698b2dcabad355226d0e509d83bbb8992934304c6802a0d569aab600c6a52db\
49ab48ab18086d2ae3762ce5698e009d583409eae3b06862cd55b1c069c6ab8c4d370001a2a4b062192af2eaf2f20a966a\
aea086b2db0153452910eca49a62cd31a77b40d34c4d369a70a43127043446869cad355a6ef701c0686aaddb4269087072\
922ad838e034e34571a6218a92681c971c69cba42a85f4ab4c59aad66b226aae900d0dd6552490f4312ad55d5d6aaf39bd\
66b6c6b2262070763bbb82a556c698d3437152cbaba89838d354099584481c64638b359aababaa834d3234304e149b69c8\
d09d5d698313456a800686980b12a4d14f9a8964075655d404ddf518ab4d65e4556db1318609210ec6aadaad3556d52a07\
790155d5b00007ae5da1a1aababa49aad55d0c256b2eb5900a8d14180471c76d3077500135426e2abab2a21349a48a9100\
aa0955d5d6aaf7acd5576e340c4aa271c71a6940a2f1320d34c4c99aad5698d02c8229054aa4ee4ce4a4ab556c7164bdc6\
9a0184698268069cbd259a0698aaf2f2f10a855134c54a34d834d36ad269d95749066ab9522c89cad30597574206d0c777\
121c195c0ab1a6a8155d6aa081809ca1a0c5692991304c001c1c71c71a2b40d3138c4ef9231036e0d031571834395c6aae\
adb1d8ca598ef51906d0d3838e0ef7071ded340000002dff000996d240c1313255e70cb600e38d0301341515db951b4569\
8e2ad6f4c6966b7797578935506e09ab5265ca126c25698aada63800d3431a4d0c77a8e310c1a1aad569b838c4e55d6aa2\
1b4d2637a426276d82a486ab55aa992395cad659501c18eb1c89c1dd44d34c59aae65c1a71c609aa4818374524952556c6\
ab8c8d6698e0b2f34eb4d30054d232291a4edaabad3689575aa707165d480c10d087a9103430156b122b4c0071acd15aa8\
98a9149b1a50cd2189a62ab60d015c71a69800e26398d0aa0e343704c59aad52a4da1355126d00d21dde93b0a4865686b3\
95ccba6862abad544d544d303145115180d31572b8dc131a015410dc013949ea5898468ad34ebb5579acbabddd41de206a\
a5260e170690c0071aad304c1a05b826265468281e9471a13011575a6e38355dadde44d54409b40d448040b20269aae572\
9569b80e2a8081c60e48a806a0323055aabaa80d66ab4c68698828ac7a4243b62ad15a626b2dcabad395226d0e509c0777\
20d4a4c4c131a01906ab4d56980a935296da4d5a4558304369571bb1672b4c7004da01a0a90769c8aadbbda5575aab4e35\
5c6269b8000d15258310c9579757979054b355757975155b602a68a5221d94934c59a634e0e0d34c4d544d38526f06a003\
90069cad355a6efa80e0343556e5271a1dda8d055956c1c701a71a2b8d310c1a8c6aa807071c6e2612b55a69b898aad8aa\
d80003418e027a18956aaeaeb5579cdeb35b63596cad0383b1dddc134ed8d31a686e26b2eaea260e34c6819984013488c7\
166b35575755069a6468609c1a663b900542701aae3456a950aaeb5574935513948a7ce5c54ec6c557501377d462ad56b2\
f20e0d898c6e5a4f80";

fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .filter_map(|i| s.get(i..i + 2))
        .filter_map(|b| u8::from_str_radix(b, 16).ok())
        .collect()
}

#[test]
fn cookie_parser_agrees_with_the_alac_crate_on_a_real_cookie() {
    let cookie_bytes = from_hex(REAL_COOKIE_HEX);

    let mine = AlacCookie::parse(&cookie_bytes).expect("this crate's cookie parser");
    let oracle = alac::StreamInfo::from_cookie(&cookie_bytes).expect("alac crate's cookie parser");

    assert_eq!(mine.config.sample_rate, oracle.sample_rate());
    assert_eq!(mine.config.bit_depth, oracle.bit_depth());
    assert_eq!(
        u32::from(mine.config.num_channels),
        u32::from(oracle.channels())
    );
    assert_eq!(mine.layout(), ChannelLayout::MONO);
}

#[test]
fn real_world_pcm_from_the_oracle_round_trips_through_this_crates_own_codec() {
    let cookie_bytes = from_hex(REAL_COOKIE_HEX);
    let packet_bytes = from_hex(REAL_PACKET_HEX);

    let info = alac::StreamInfo::from_cookie(&cookie_bytes).expect("alac crate's cookie parser");
    let mut oracle_decoder = alac::Decoder::new(info.clone());
    let mut out = vec![0i16; (info.max_samples_per_packet() as usize) * (info.channels() as usize)];
    let real_pcm: Vec<i32> = oracle_decoder
        .decode_packet(&packet_bytes, &mut out)
        .expect("alac crate failed to decode a real ffmpeg-produced packet")
        .iter()
        .map(|&s| i32::from(s))
        .collect();
    assert_eq!(
        real_pcm.len(),
        4096,
        "the real fixture packet is exactly one 4096-sample frame"
    );

    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_audio(
        &mut budget,
        SampleFmt::S16P,
        ChannelLayout::MONO,
        real_pcm.len() as u32,
        info.sample_rate(),
    )
    .expect("alloc_audio");
    {
        let mut plane = frame.plane_mut(0).expect("plane 0");
        let row = plane.row_mut(0).expect("row 0");
        for (i, &s) in real_pcm.iter().enumerate() {
            if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                dst.copy_from_slice(&(s as i16).to_le_bytes());
            }
        }
    }
    frame.pts = Timestamp::new(0);

    let mut enc = AlacEncoder::new(Limits::permissive());
    enc.send_frame(Some(&frame)).expect("send_frame");
    let packet = enc.receive_packet().expect("receive_packet");
    // A real regression guard for the whole point of a *lossless
    // compressor*: `frame_codec::encode` used to emit escape-mode
    // (verbatim) packets exactly `real_pcm.len() * 2` bytes long -- valid
    // and interoperable, but no smaller than raw 16-bit PCM. This asserts
    // the adaptive predictor + Rice coder path is actually active and
    // buys a real reduction on real content, not just a spec-legal packet
    // that happens to round-trip.
    let raw_pcm_bytes = real_pcm.len() * 2;
    assert!(
        packet.payload().len() * 4 < raw_pcm_bytes * 3,
        "expected real compression on real content: {} encoded bytes vs {raw_pcm_bytes} bytes of raw 16-bit PCM",
        packet.payload().len()
    );
    let mut budget2 = Budget::new(Limits::permissive());
    let packet = Packet::from_slice(&mut budget2, packet.payload()).expect("packet from_slice");

    let mut dec = AlacDecoder::new(Limits::permissive());
    dec.send_packet(Some(&packet)).expect("send_packet");
    let decoded = dec.receive_frame().expect("receive_frame");
    let FrameData::Audio {
        planes, samples, ..
    } = &decoded.data
    else {
        unreachable!("audio frame")
    };
    assert_eq!(*samples, real_pcm.len() as u32);
    let plane = planes.first().expect("plane 0");
    // 16-bit real-world content: `frame_codec::decode` matches its output
    // `SampleFmt` to the packet's actual bit depth (S16P here), not
    // always S32P -- see that function's doc for why always-S32P was a
    // real, measured bug (`vaco-resample`'s S32->S16 narrowing assumes
    // S32P is always full-scale).
    let got: Vec<i32> = plane
        .data
        .as_slice()
        .chunks_exact(2)
        .map(|c| i32::from(i16::from_le_bytes(c.try_into().expect("2 bytes"))))
        .collect();
    assert_eq!(got, real_pcm);
}

/// The genuine interop proof this crate's earlier, self-invented framing
/// could never attempt: decode a real ffmpeg-produced ALAC packet with
/// *this crate's own decoder* (no oracle involved on this side at all) and
/// check it against the same bytes decoded by the independent `alac`
/// crate. Two different decoders, two different implementations, same
/// real-world compressed bytes, same PCM out.
#[test]
fn this_crates_own_decoder_reads_a_real_ffmpeg_alac_packet_bit_for_bit() {
    let cookie_bytes = from_hex(REAL_COOKIE_HEX);
    let packet_bytes = from_hex(REAL_PACKET_HEX);

    let info = alac::StreamInfo::from_cookie(&cookie_bytes).expect("alac crate's cookie parser");
    let mut oracle_decoder = alac::Decoder::new(info.clone());
    let mut out = vec![0i16; (info.max_samples_per_packet() as usize) * (info.channels() as usize)];
    let oracle_pcm: Vec<i32> = oracle_decoder
        .decode_packet(&packet_bytes, &mut out)
        .expect("alac crate failed to decode a real ffmpeg-produced packet")
        .iter()
        .map(|&s| i32::from(s))
        .collect();

    let mut budget = Budget::new(Limits::permissive());
    let mut dec = AlacDecoder::new(Limits::permissive());
    dec.set_extradata(&cookie_bytes)
        .expect("set_extradata on a real cookie");
    let mut packet = Packet::from_slice(&mut budget, &packet_bytes).expect("packet from_slice");
    packet.pts = Timestamp::new(0);
    dec.send_packet(Some(&packet))
        .expect("this crate's decoder must accept a real ffmpeg packet");
    let decoded = dec.receive_frame().expect("receive_frame");
    let FrameData::Audio {
        planes, samples, ..
    } = &decoded.data
    else {
        unreachable!("audio frame")
    };
    assert_eq!(*samples, oracle_pcm.len() as u32);
    let plane = planes.first().expect("plane 0");
    // Same S16P-for-16-bit reasoning as the round-trip test above.
    let mine: Vec<i32> = plane
        .data
        .as_slice()
        .chunks_exact(2)
        .map(|c| i32::from(i16::from_le_bytes(c.try_into().expect("2 bytes"))))
        .collect();
    assert_eq!(
        mine, oracle_pcm,
        "this crate's own decoder must reproduce the alac crate's decode of the same real ffmpeg packet, sample for sample"
    );
}

/// Feed *this crate's own encoder's* packet bytes into the independent
/// `alac` crate decoder -- the missing half of the interop proof: the
/// existing tests in this file check that this crate's *decoder* accepts
/// real ffmpeg-produced bytes, and that a real-world signal survives this
/// crate's own encode+decode round trip, but nothing here previously fed
/// this crate's own *encoder* output to any decoder other than this
/// crate's own. If a real, independent ALAC decoder also rejects it, the
/// defect is in this crate's encoder, not just in how ffmpeg's demuxer/
/// decoder in particular reacts to it.
#[test]
fn this_crates_own_encoder_output_is_accepted_by_the_oracle_decoder() {
    use vaco_codec_alac::AlacEncoder;
    use vaco_codec_core::Encoder;

    let samples: Vec<i16> = (0..4096)
        .map(|i| (((i * 37) % 3001) - 1500) as i16)
        .collect();
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_audio(
        &mut budget,
        vaco_sampfmt::SampleFmt::S16P,
        vaco_chlayout::ChannelLayout::MONO,
        samples.len() as u32,
        44100,
    )
    .expect("alloc_audio");
    {
        let mut plane = frame.plane_mut(0).expect("plane 0");
        let row = plane.row_mut(0).expect("row 0");
        for (i, &s) in samples.iter().enumerate() {
            if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                dst.copy_from_slice(&s.to_le_bytes());
            }
        }
    }

    let mut enc = AlacEncoder::new(Limits::permissive());
    enc.prime_audio(
        44100,
        vaco_chlayout::ChannelLayout::MONO,
        vaco_sampfmt::SampleFmt::S16P,
    );
    enc.send_frame(Some(&frame)).expect("send_frame");
    let packet = enc.receive_packet().expect("receive_packet");
    let cookie = enc.extradata().expect("cookie after prime_audio");
    eprintln!("cookie = {cookie:02x?}");
    eprintln!("packet len = {}", packet.payload().len());
    eprintln!(
        "packet first bytes = {:02x?}",
        &packet.payload()[..packet.payload().len().min(16)]
    );

    let info = alac::StreamInfo::from_cookie(&cookie)
        .expect("alac crate must parse this crate's own cookie");
    let mut oracle_decoder = alac::Decoder::new(info.clone());
    let mut out = vec![0i16; (info.max_samples_per_packet() as usize) * (info.channels() as usize)];
    let result = oracle_decoder.decode_packet(packet.payload(), &mut out);
    match &result {
        Ok(pcm) => eprintln!("oracle decoded {} samples ok", pcm.len()),
        Err(e) => eprintln!("oracle decode FAILED: {e:?}"),
    }
    let oracle_pcm: Vec<i32> = result
        .expect("independent alac crate must decode this crate's own encoder output")
        .iter()
        .map(|&s| i32::from(s))
        .collect();
    let expected: Vec<i32> = samples.iter().map(|&s| i32::from(s)).collect();
    assert_eq!(
        oracle_pcm, expected,
        "oracle decode of our own encoder's output must match the source samples"
    );
}

#[test]
fn escape_mode_manual_packet_is_accepted_by_the_oracle_decoder() {
    use vaco_bitstream::BitWriter;
    use vaco_limits::{Budget, Limits};

    let samples: Vec<i16> = (0..4096)
        .map(|i| (((i * 37) % 3001) - 1500) as i16)
        .collect();
    let mut budget = Budget::new(Limits::permissive());
    let mut w = BitWriter::with_capacity(&mut budget, 16000).unwrap();
    w.put(3, 0); // ID_SCE
    w.put(4, 0); // instance tag
    w.put(12, 0); // unused
    w.put(4, 0b0001); // partialFrame=0 (full frame_length), bytesShifted=00, escape=1
    for &s in &samples {
        w.put_signed(16, i32::from(s));
    }
    w.put(3, 7); // ID_END
    w.align_zero();
    let bytes = w.finish();
    eprintln!("escape packet bytes.len() = {}", bytes.len());

    let cookie = crate_cookie_bytes();
    let info = alac::StreamInfo::from_cookie(&cookie).expect("cookie parse");
    let mut oracle_decoder = alac::Decoder::new(info.clone());
    let mut out = vec![0i16; (info.max_samples_per_packet() as usize) * (info.channels() as usize)];
    let result = oracle_decoder.decode_packet(&bytes, &mut out);
    match &result {
        Ok(pcm) => eprintln!("ESCAPE MODE: oracle decoded {} samples ok", pcm.len()),
        Err(e) => eprintln!("ESCAPE MODE: oracle decode FAILED: {e:?}"),
    }
    let pcm = result.expect("oracle must decode escape-mode packet");
    let expected: Vec<i16> = samples;
    assert_eq!(pcm, expected);
}

fn crate_cookie_bytes() -> Vec<u8> {
    use vaco_codec_alac::AlacSpecificConfig;
    // 40/10/14: this crate's own `rice::{PB0,MB0,KB0}` -- `pub(crate)`, so
    // restated literally here rather than named, from an external test.
    AlacSpecificConfig::for_encode(44100, 1, 16, 40, 10, 14)
        .write_bare()
        .to_vec()
}

#[test]
fn escape_mode_partial_frame_manual_packet_is_accepted_by_the_oracle_decoder() {
    use vaco_bitstream::BitWriter;
    use vaco_limits::{Budget, Limits};

    let samples: Vec<i16> = (0..2944)
        .map(|i| (((i * 37) % 3001) - 1500) as i16)
        .collect();
    let mut budget = Budget::new(Limits::permissive());
    let mut w = BitWriter::with_capacity(&mut budget, 16000).unwrap();
    w.put(3, 0); // ID_SCE
    w.put(4, 0); // instance tag
    w.put(12, 0); // unused
    w.put(4, 0b1001); // partialFrame=1 (short final block), bytesShifted=00, escape=1
    w.put(32, samples.len() as u32);
    for &s in &samples {
        w.put_signed(16, i32::from(s));
    }
    w.put(3, 7); // ID_END
    w.align_zero();
    let bytes = w.finish();
    eprintln!("partial escape packet bytes.len() = {}", bytes.len());

    let cookie = crate_cookie_bytes();
    let info = alac::StreamInfo::from_cookie(&cookie).expect("cookie parse");
    let mut oracle_decoder = alac::Decoder::new(info.clone());
    let mut out = vec![0i16; (info.max_samples_per_packet() as usize) * (info.channels() as usize)];
    let result = oracle_decoder.decode_packet(&bytes, &mut out);
    match &result {
        Ok(pcm) => eprintln!(
            "PARTIAL ESCAPE MODE: oracle decoded {} samples ok",
            pcm.len()
        ),
        Err(e) => eprintln!("PARTIAL ESCAPE MODE: oracle decode FAILED: {e:?}"),
    }
    let pcm = result.expect("oracle must decode partial escape-mode packet");
    assert_eq!(pcm, samples);
}

#[test]
fn escape_mode_explicit_count_full_frame_is_accepted_by_the_oracle_decoder() {
    use vaco_bitstream::BitWriter;
    use vaco_limits::{Budget, Limits};

    let samples: Vec<i16> = (0..4096)
        .map(|i| (((i * 37) % 3001) - 1500) as i16)
        .collect();
    let mut budget = Budget::new(Limits::permissive());
    let mut w = BitWriter::with_capacity(&mut budget, 16000).unwrap();
    w.put(3, 0); // ID_SCE
    w.put(4, 0); // instance tag
    w.put(12, 0); // unused
    w.put(4, 0b1001); // partialFrame=1 (explicit count, even though == frame_length), escape=1
    w.put(32, samples.len() as u32);
    for &s in &samples {
        w.put_signed(16, i32::from(s));
    }
    w.put(3, 7); // ID_END
    w.align_zero();
    let bytes = w.finish();

    let cookie = crate_cookie_bytes();
    let info = alac::StreamInfo::from_cookie(&cookie).expect("cookie parse");
    let mut oracle_decoder = alac::Decoder::new(info.clone());
    let mut out = vec![0i16; (info.max_samples_per_packet() as usize) * (info.channels() as usize)];
    let result = oracle_decoder.decode_packet(&bytes, &mut out);
    match &result {
        Ok(pcm) => eprintln!(
            "EXPLICIT+FULL ESCAPE MODE: oracle decoded {} samples ok",
            pcm.len()
        ),
        Err(e) => eprintln!("EXPLICIT+FULL ESCAPE MODE: oracle decode FAILED: {e:?}"),
    }
    let pcm = result.expect("oracle must decode explicit-count full-frame escape-mode packet");
    assert_eq!(pcm, samples);
}

#[test]
fn stereo_escape_mode_chan_bits_equals_bit_depth_is_accepted_by_the_oracle_decoder() {
    use vaco_bitstream::BitWriter;
    use vaco_codec_alac::AlacSpecificConfig;
    use vaco_limits::{Budget, Limits};

    let left: Vec<i16> = (0..2048)
        .map(|i| (((i * 37) % 3001) - 1500) as i16)
        .collect();
    let right: Vec<i16> = (0..2048)
        .map(|i| (((i * 59) % 2001) - 1000) as i16)
        .collect();
    let mut budget = Budget::new(Limits::permissive());
    let mut w = BitWriter::with_capacity(&mut budget, 16000).unwrap();
    w.put(3, 1); // ID_CPE
    w.put(4, 0);
    w.put(12, 0);
    w.put(4, 0b1001); // partialFrame=1, escape=1
    w.put(32, left.len() as u32);
    for i in 0..left.len() {
        w.put_signed(16, i32::from(left[i])); // chan_bits = bit_depth (16), NOT +1
        w.put_signed(16, i32::from(right[i]));
    }
    w.put(3, 7); // ID_END
    w.align_zero();
    let bytes = w.finish();

    let cookie = AlacSpecificConfig::for_encode(44100, 2, 16, 40, 10, 14)
        .write_bare()
        .to_vec();
    let info = alac::StreamInfo::from_cookie(&cookie).expect("cookie parse");
    let mut oracle_decoder = alac::Decoder::new(info.clone());
    let mut out = vec![0i16; (info.max_samples_per_packet() as usize) * (info.channels() as usize)];
    let result = oracle_decoder.decode_packet(&bytes, &mut out);
    match &result {
        Ok(pcm) => eprintln!(
            "STEREO ESCAPE (chan_bits=bit_depth): oracle decoded {} samples ok",
            pcm.len()
        ),
        Err(e) => eprintln!("STEREO ESCAPE (chan_bits=bit_depth): oracle decode FAILED: {e:?}"),
    }
    let pcm =
        result.expect("oracle must decode stereo escape-mode packet with chan_bits=bit_depth");
    let expected: Vec<i16> = left
        .iter()
        .zip(right.iter())
        .flat_map(|(&l, &r)| [l, r])
        .collect();
    assert_eq!(pcm, expected);
}

/// The stereo counterpart of
/// `this_crates_own_encoder_output_is_accepted_by_the_oracle_decoder`: feed
/// this crate's own encoder's *stereo* packet bytes to the independent
/// `alac` crate decoder. Stereo escape mode uses a different `chan_bits`
/// than mono (`bit_depth`, not `bit_depth + 1` -- see `frame_codec::decode`'s
/// escape-mode doc), so mono passing this check does not imply stereo does.
#[test]
fn this_crates_own_stereo_encoder_output_is_accepted_by_the_oracle_decoder() {
    use vaco_codec_alac::AlacEncoder;
    use vaco_codec_core::Encoder;

    let left: Vec<i16> = (0..3000)
        .map(|i| (((i * 37) % 3001) - 1500) as i16)
        .collect();
    let right: Vec<i16> = (0..3000)
        .map(|i| (((i * 59) % 2001) - 1000) as i16)
        .collect();
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_audio(
        &mut budget,
        vaco_sampfmt::SampleFmt::S16P,
        vaco_chlayout::ChannelLayout::STEREO,
        left.len() as u32,
        44100,
    )
    .expect("alloc_audio");
    {
        let mut plane = frame.plane_mut(0).expect("plane 0");
        let row = plane.row_mut(0).expect("row 0");
        for (i, &s) in left.iter().enumerate() {
            if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                dst.copy_from_slice(&s.to_le_bytes());
            }
        }
    }
    {
        let mut plane = frame.plane_mut(1).expect("plane 1");
        let row = plane.row_mut(0).expect("row 0");
        for (i, &s) in right.iter().enumerate() {
            if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                dst.copy_from_slice(&s.to_le_bytes());
            }
        }
    }

    let mut enc = AlacEncoder::new(Limits::permissive());
    enc.prime_audio(
        44100,
        vaco_chlayout::ChannelLayout::STEREO,
        vaco_sampfmt::SampleFmt::S16P,
    );
    enc.send_frame(Some(&frame)).expect("send_frame");
    let packet = enc.receive_packet().expect("receive_packet");
    let cookie = enc.extradata().expect("cookie after prime_audio");

    let info = alac::StreamInfo::from_cookie(&cookie)
        .expect("alac crate must parse this crate's own cookie");
    let mut oracle_decoder = alac::Decoder::new(info.clone());
    let mut out = vec![0i16; (info.max_samples_per_packet() as usize) * (info.channels() as usize)];
    let result = oracle_decoder.decode_packet(packet.payload(), &mut out);
    match &result {
        Ok(pcm) => eprintln!("STEREO oracle decoded {} interleaved samples ok", pcm.len()),
        Err(e) => eprintln!("STEREO oracle decode FAILED: {e:?}"),
    }
    let oracle_pcm =
        result.expect("independent alac crate must decode this crate's own stereo encoder output");
    let expected: Vec<i16> = left
        .iter()
        .zip(right.iter())
        .flat_map(|(&l, &r)| [l, r])
        .collect();
    assert_eq!(
        oracle_pcm, expected,
        "oracle decode of our own stereo encoder output must match the source samples, interleaved"
    );
}

/// Stereo content designed to force mid/side selection: near-identical
/// channels (a sine plus a tiny per-sample offset on the right), the exact
/// shape `choose_stereo_mix` exists to detect and switch on. Verifies two
/// things a synthetic uncorrelated-channel test cannot: that the encoder
/// actually chooses the mid/side candidate here (not just that either
/// candidate round-trips), and that the independent `alac` crate oracle
/// decodes the *matrixed* (`mixres != 0`) bitstream correctly -- this
/// crate's own decoder used to hardcode `unmix(u, v, 0, 0)` regardless of
/// what a packet's header actually said, a latent bug invisible until an
/// encoder (this one) finally emitted a non-zero `mixres` to decode back.
#[test]
fn highly_correlated_stereo_content_is_smaller_than_independent_channels_and_the_oracle_accepts_it()
{
    use vaco_codec_alac::AlacEncoder;
    use vaco_codec_core::Encoder;

    let n = 4096;
    let left: Vec<i16> = (0..n)
        .map(|i| {
            let x = f64::from(i) * 0.05;
            (x.sin() * 12000.0) as i16
        })
        .collect();
    // Right is left plus a small, mostly-constant offset -- correlated,
    // not identical (identical channels are the easy case; this is closer
    // to real, imperfectly-correlated stereo material).
    let right: Vec<i16> = left
        .iter()
        .enumerate()
        .map(|(i, &l)| l.saturating_add(20 + i16::try_from(i % 7).unwrap_or(0)))
        .collect();

    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_audio(
        &mut budget,
        SampleFmt::S16P,
        ChannelLayout::STEREO,
        n as u32,
        44100,
    )
    .expect("alloc_audio");
    {
        let mut plane = frame.plane_mut(0).expect("plane 0");
        let row = plane.row_mut(0).expect("row 0");
        for (i, &s) in left.iter().enumerate() {
            if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                dst.copy_from_slice(&s.to_le_bytes());
            }
        }
    }
    {
        let mut plane = frame.plane_mut(1).expect("plane 1");
        let row = plane.row_mut(0).expect("row 0");
        for (i, &s) in right.iter().enumerate() {
            if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                dst.copy_from_slice(&s.to_le_bytes());
            }
        }
    }

    let mut enc = AlacEncoder::new(Limits::permissive());
    enc.prime_audio(44100, ChannelLayout::STEREO, SampleFmt::S16P);
    enc.send_frame(Some(&frame)).expect("send_frame");
    let packet = enc.receive_packet().expect("receive_packet");
    let cookie = enc.extradata().expect("cookie after prime_audio");

    // Same content encoded as two independent (uncorrelated by construction
    // of the comparison, not the content) mono streams, for the size
    // comparison: this is what this crate produced *before* stereo
    // decorrelation, and the whole point is that correlated content must
    // beat it now.
    let encode_mono = |samples: &[i16]| -> usize {
        let mut b = Budget::new(Limits::permissive());
        let mut f = Frame::alloc_audio(
            &mut b,
            SampleFmt::S16P,
            ChannelLayout::MONO,
            samples.len() as u32,
            44100,
        )
        .expect("alloc_audio");
        {
            let mut plane = f.plane_mut(0).expect("plane 0");
            let row = plane.row_mut(0).expect("row 0");
            for (i, &s) in samples.iter().enumerate() {
                if let Some(dst) = row.get_mut(i * 2..i * 2 + 2) {
                    dst.copy_from_slice(&s.to_le_bytes());
                }
            }
        }
        let mut e = AlacEncoder::new(Limits::permissive());
        e.send_frame(Some(&f)).expect("send_frame");
        e.receive_packet().expect("receive_packet").payload().len()
    };
    let independent_total = encode_mono(&left) + encode_mono(&right);
    eprintln!(
        "stereo (decorrelated) = {} bytes, two independent mono encodes = {independent_total} bytes",
        packet.payload().len()
    );
    assert!(
        packet.payload().len() < independent_total,
        "correlated stereo content must compress smaller as one decorrelated CPE element than as two independent channels: {} >= {independent_total}",
        packet.payload().len()
    );

    let info = alac::StreamInfo::from_cookie(&cookie)
        .expect("alac crate must parse this crate's own cookie");
    let mut oracle_decoder = alac::Decoder::new(info.clone());
    let mut out = vec![0i16; (info.max_samples_per_packet() as usize) * (info.channels() as usize)];
    let oracle_pcm = oracle_decoder
        .decode_packet(packet.payload(), &mut out)
        .expect("independent alac crate must decode this crate's matrixed stereo output");
    let mut expected = Vec::new();
    for i in 0..left.len() {
        expected.push(left[i]);
        expected.push(right[i]);
    }
    assert_eq!(
        oracle_pcm, expected,
        "oracle decode of matrixed stereo must match the source, interleaved, sample-exact"
    );
}
