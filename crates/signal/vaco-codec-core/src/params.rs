//! Codec parameters: the container-level description of a stream.

use crate::CodecId;
use vaco_chlayout::ChannelLayout;
use vaco_color::ColorInfo;
use vaco_core::{MediaType, Rational};
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

/// What a container knows about a stream before anything is decoded.
///
/// This is the boundary type between `vaco-format-core` and `vaco-codec-core`,
/// and it is what `vaco-probe` reports for `-show_streams`.
#[derive(Debug, Clone, Default)]
pub struct CodecParameters {
    pub media_type: Option<MediaType>,
    pub codec_id: Option<CodecId>,
    /// The container's own four-character code, preserved verbatim because
    /// ffprobe prints it.
    pub codec_tag: Option<[u8; 4]>,
    /// Out-of-band configuration: `SPS`/`PPS`, `AudioSpecificConfig`, and similar.
    pub extradata: Option<Vec<u8>>,
    pub bit_rate: Option<u64>,
    pub profile: Option<Profile>,
    pub level: Option<Level>,
    pub video: Option<VideoParameters>,
    pub audio: Option<AudioParameters>,
}

#[derive(Debug, Clone, Default)]
pub struct VideoParameters {
    pub width: u32,
    pub height: u32,
    /// Dimensions before display cropping.
    pub coded_width: u32,
    pub coded_height: u32,
    pub format: Option<PixFmt>,
    pub sample_aspect_ratio: Rational,
    pub frame_rate: Rational,
    pub color: ColorInfo,
    pub field_order: FieldOrder,
    /// Reorder depth; non-zero means dts differs from pts.
    pub has_b_frames: u8,
}

#[derive(Debug, Clone, Default)]
pub struct AudioParameters {
    pub sample_rate: u32,
    pub format: Option<SampleFmt>,
    pub layout: Option<ChannelLayout>,
    pub bits_per_raw_sample: Option<u8>,
    /// Encoder priming samples to discard.
    pub initial_padding: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FieldOrder {
    #[default]
    Progressive,
    TopFirst,
    BottomFirst,
    TopCodedFirst,
    BottomCodedFirst,
    Unknown,
}

/// A codec profile. The numeric value is the codec's own; the name is for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub value: i32,
    pub name: &'static str,
}

/// A codec level, in whatever units the codec's specification uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level(pub i32);
