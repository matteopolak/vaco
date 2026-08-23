//! Codec parameters: the container-level description of a stream.

use crate::CodecId;
use vaco_chlayout::ChannelLayout;
use vaco_color::{
    ChromaLocation, ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients,
    TransferCharacteristic,
};
use vaco_core::{MediaType, Rational};
use vaco_limits::Budget;
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
    /// Bits per component in the coded picture, when the bitstream states one.
    ///
    /// [`AudioParameters`] has carried the same field since the type was
    /// frozen, and the *video* half was the one actually printed: measured on
    /// `av.mp4`, the reference reports `bits_per_raw_sample=8` on the H.264
    /// stream and `N/A` on the AAC stream beside it — the exact opposite of
    /// what this model could express. It is a separate field from the pixel
    /// format because a 10-bit stream can be carried in a 16-bit format and
    /// the reference prints the bitstream's number, not the container's.
    pub bits_per_raw_sample: Option<u8>,
    /// The length prefix size the container's configuration record declares, in
    /// bytes; `Some(0)` for an Annex B byte stream that has no record.
    ///
    /// This is a **container** property that only a parser can read, which is
    /// why it is here and not in `vaco-format-core`: it lives inside `avcC`
    /// and `hvcC`, and reading those means parsing. `ffprobe` prints it as the
    /// H.264 decoder's private `is_avc` and `nal_length_size` options, and
    /// prints them for *every* H.264 stream — measured, `av.mp4` reports
    /// `is_avc=true nal_length_size=4` and the same content in MPEG-TS reports
    /// `is_avc=false nal_length_size=0`. `None` means the question does not
    /// apply to the codec, and nothing is printed.
    pub nal_length_size: Option<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct AudioParameters {
    pub sample_rate: u32,
    pub format: Option<SampleFmt>,
    pub layout: Option<ChannelLayout>,
    /// Bits per sample *as the container stores them*, when it says.
    ///
    /// Distinct from [`AudioParameters::bits_per_raw_sample`], and the two were
    /// confused in exactly one direction: MP4's `stsd` `sample_size` and
    /// Matroska's `BitDepth` were being filed as `bits_per_raw_sample`, so an
    /// AAC track reported 16 and an Opus track 32 where the reference reports
    /// `N/A`. The number was not wrong, it was in the wrong field.
    ///
    /// Measured, which is what separates them:
    ///
    /// ```text
    ///                 bits_per_sample  bits_per_raw_sample
    /// pcm_s16le wav        16                N/A
    /// pcm_s24le mov        24                24
    /// aac       mp4         0                N/A
    /// ```
    ///
    /// So `bits_per_sample` is the container's stored depth and is `0` — not
    /// absent — for a compressed codec, while `bits_per_raw_sample` is a codec
    /// fact the reference states only when it differs from the sample format's
    /// natural depth. `pcm_s16le` is 16-in-16 and says nothing; `pcm_s24le` is
    /// 24-in-32 and says 24.
    pub bits_per_coded_sample: Option<u8>,
    /// Bits of real precision in each decoded sample, when the *codec* states
    /// one. See [`AudioParameters::bits_per_coded_sample`] for the distinction.
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

// ---------------------------------------------------------------- behaviour

impl CodecParameters {
    /// An empty description of a stream of `media_type`.
    #[must_use]
    pub fn new(media_type: MediaType) -> Self {
        Self {
            media_type: Some(media_type),
            ..Self::default()
        }
    }

    /// An empty video description with its [`VideoParameters`] present.
    #[must_use]
    pub fn video() -> Self {
        Self {
            media_type: Some(MediaType::Video),
            video: Some(VideoParameters::default()),
            ..Self::default()
        }
    }

    /// An empty audio description with its [`AudioParameters`] present.
    #[must_use]
    pub fn audio() -> Self {
        Self {
            media_type: Some(MediaType::Audio),
            audio: Some(AudioParameters::default()),
            ..Self::default()
        }
    }

    /// Set the codec, and the media type implied by it if none is set yet.
    #[must_use]
    pub fn with_codec(mut self, id: CodecId) -> Self {
        self.codec_id = Some(id);
        self.media_type.get_or_insert(id.media_type());
        self
    }

    /// The media type, falling back to the one the codec implies and then to
    /// whichever parameter block is populated.
    #[must_use]
    pub fn effective_media_type(&self) -> Option<MediaType> {
        self.media_type
            .or_else(|| self.codec_id.map(CodecId::media_type))
            .or_else(|| match (self.video.is_some(), self.audio.is_some()) {
                (true, false) => Some(MediaType::Video),
                (false, true) => Some(MediaType::Audio),
                _ => None,
            })
    }

    /// Structural consistency, independent of any limit.
    ///
    /// Catches the two mistakes a demuxer actually makes: labelling a stream
    /// with one media type while filling in the other's parameter block, and
    /// filling in both.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] describing the inconsistency.
    pub fn check_consistent(&self) -> vaco_core::Result<()> {
        if self.video.is_some() && self.audio.is_some() {
            return Err(vaco_core::Error::InvalidData(
                "codec parameters carry both video and audio blocks",
            ));
        }
        match self.effective_media_type() {
            Some(MediaType::Video) if self.audio.is_some() => Err(vaco_core::Error::InvalidData(
                "video stream carries audio parameters",
            )),
            Some(MediaType::Audio) if self.video.is_some() => Err(vaco_core::Error::InvalidData(
                "audio stream carries video parameters",
            )),
            _ => Ok(()),
        }
    }

    /// Consistency plus every attacker-controlled magnitude checked against a
    /// budget: dimensions, implied frame size, sample rate, channel count and
    /// extradata length.
    ///
    /// This is the call a demuxer makes before it hands parameters onward. The
    /// numbers come from a file, so nothing derived from them may be trusted
    /// until it has been through here.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] for a structural problem, or
    /// [`vaco_core::Error::LimitExceeded`] naming the cap that was hit.
    pub fn validate(&self, budget: &Budget) -> vaco_core::Result<()> {
        self.check_consistent()?;
        if let Some(v) = &self.video {
            // Four bytes per pixel is the widest packed 8-bit layout; planar
            // high-bit-depth formats are checked again by the decoder once the
            // pixel format is actually known.
            budget.check_frame(v.coded_width.max(v.width), v.coded_height.max(v.height), 4)?;
        }
        if let Some(a) = &self.audio {
            budget.check_sample_rate(u64::from(a.sample_rate))?;
            if let Some(layout) = &a.layout {
                budget.check_channels(u64::from(layout.channels))?;
            }
        }
        if let Some(extra) = &self.extradata {
            budget.check_metadata_bytes(extra.len() as u64)?;
        }
        Ok(())
    }

    /// Fill in fields this description does not have from one that does.
    ///
    /// The direction is load-bearing: a container's own metadata wins, and a
    /// parser only supplies what the container left blank. Inverting it is how
    /// a stream whose header disagrees with its container ends up reported
    /// wrongly.
    pub fn fill_from(&mut self, other: &Self) {
        merge_option(&mut self.media_type, other.media_type);
        merge_option(&mut self.codec_id, other.codec_id);
        merge_option(&mut self.codec_tag, other.codec_tag);
        merge_option(&mut self.bit_rate, other.bit_rate);
        merge_option(&mut self.profile, other.profile);
        merge_option(&mut self.level, other.level);
        if self.extradata.is_none() {
            self.extradata.clone_from(&other.extradata);
        }
        match (&mut self.video, &other.video) {
            (Some(mine), Some(theirs)) => mine.fill_from(theirs),
            (slot @ None, Some(theirs)) => *slot = Some(theirs.clone()),
            _ => {}
        }
        match (&mut self.audio, &other.audio) {
            (Some(mine), Some(theirs)) => mine.fill_from(theirs),
            (slot @ None, Some(theirs)) => *slot = Some(theirs.clone()),
            _ => {}
        }
    }
}

fn merge_option<T: Copy>(slot: &mut Option<T>, from: Option<T>) {
    if slot.is_none() {
        *slot = from;
    }
}

/// Merge a colour description property by property, treating each
/// `Unspecified` as unset.
///
/// `vaco-color` has no `fill_from` of its own and this crate does not own it,
/// so the merge lives here. If `ColorInfo` ever grows one, this becomes a
/// forwarding call — the semantics are identical and deliberately so.
fn merge_colour(slot: &mut ColorInfo, from: ColorInfo) {
    if slot.primaries == ColorPrimaries::Unspecified {
        slot.primaries = from.primaries;
    }
    if slot.transfer == TransferCharacteristic::Unspecified {
        slot.transfer = from.transfer;
    }
    if slot.matrix == MatrixCoefficients::Unspecified {
        slot.matrix = from.matrix;
    }
    if slot.range == ColorRange::Unspecified {
        slot.range = from.range;
    }
    if slot.chroma_location == ChromaLocation::Unspecified {
        slot.chroma_location = from.chroma_location;
    }
}

impl VideoParameters {
    /// Fill in unset fields from `other`. Zero and
    /// [`Rational::ZERO`](vaco_core::Rational) count as unset, because that is
    /// what a container that did not say leaves behind.
    pub fn fill_from(&mut self, other: &Self) {
        if self.width == 0 {
            self.width = other.width;
        }
        if self.height == 0 {
            self.height = other.height;
        }
        if self.coded_width == 0 {
            self.coded_width = other.coded_width;
        }
        if self.coded_height == 0 {
            self.coded_height = other.coded_height;
        }
        if self.format.is_none() {
            self.format = other.format;
        }
        if self.sample_aspect_ratio.num == 0 {
            self.sample_aspect_ratio = other.sample_aspect_ratio;
        }
        if self.frame_rate.num == 0 {
            self.frame_rate = other.frame_rate;
        }
        if self.field_order == FieldOrder::Progressive {
            self.field_order = other.field_order;
        }
        if self.has_b_frames == 0 {
            self.has_b_frames = other.has_b_frames;
        }
        if self.bits_per_raw_sample.is_none() {
            self.bits_per_raw_sample = other.bits_per_raw_sample;
        }
        if self.nal_length_size.is_none() {
            self.nal_length_size = other.nal_length_size;
        }
        // Per-property, not whole-struct. A container often states *some* of
        // the colour description and leaves the rest — MP4's `colr` box carries
        // primaries, transfer and matrix but has no chroma siting at all — so
        // replacing the block only when it is entirely default would keep
        // `chroma_location=unspecified` on every H.264 file whose VUI states
        // it. Measured: 9 of the 180 divergences on the corpus were exactly
        // this, and all 9 are `chroma_location`.
        merge_colour(&mut self.color, other.color);
    }

    /// Coded dimensions, falling back to the display ones when the container
    /// did not distinguish them.
    #[must_use]
    pub const fn coded_dimensions(&self) -> (u32, u32) {
        (
            if self.coded_width == 0 {
                self.width
            } else {
                self.coded_width
            },
            if self.coded_height == 0 {
                self.height
            } else {
                self.coded_height
            },
        )
    }
}

impl AudioParameters {
    /// Fill in unset fields from `other`.
    pub fn fill_from(&mut self, other: &Self) {
        if self.sample_rate == 0 {
            self.sample_rate = other.sample_rate;
        }
        if self.format.is_none() {
            self.format = other.format;
        }
        if self.layout.is_none() {
            self.layout.clone_from(&other.layout);
        }
        if self.bits_per_raw_sample.is_none() {
            self.bits_per_raw_sample = other.bits_per_raw_sample;
        }
        if self.bits_per_coded_sample.is_none() {
            self.bits_per_coded_sample = other.bits_per_coded_sample;
        }
        if self.initial_padding == 0 {
            self.initial_padding = other.initial_padding;
        }
    }
}

impl Profile {
    /// The value every codec uses for "not stated".
    pub const UNKNOWN_VALUE: i32 = -99;

    /// A profile with a display name.
    #[must_use]
    pub const fn new(value: i32, name: &'static str) -> Self {
        Self { value, name }
    }

    /// Whether this is the "not stated" value.
    #[must_use]
    pub const fn is_unknown(self) -> bool {
        self.value == Self::UNKNOWN_VALUE
    }
}

impl Level {
    /// The raw, codec-specific encoding: H.264 level ×10, HEVC
    /// `general_level_idc` ×30, AV1 `seq_level_idx`, VP9 level ×10.
    ///
    /// Never normalised across codecs — round-tripping a container's value back
    /// out byte-identically is what `vaco-probe` needs.
    #[must_use]
    pub const fn raw(self) -> i32 {
        self.0
    }
}

/// What a level caps. The units are the specification's, not ours.
///
/// Levels are what `-level` validation, DPB sizing and hardware capability
/// matching all consult, which is why this is a table of constraints rather
/// than just a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LevelConstraints {
    /// Maximum luma samples in one picture.
    pub max_luma_picture_size: u64,
    /// Maximum luma samples per second.
    pub max_luma_sample_rate: u64,
    /// Maximum bit rate, in kbit/s. Zero means unconstrained.
    pub max_bitrate_kbps: u32,
    /// Maximum decoded picture buffer size, in frames.
    pub max_dpb_frames: u16,
    /// Maximum picture width, in luma samples.
    pub max_h_size: u32,
    /// Maximum picture height, in luma samples.
    pub max_v_size: u32,
    /// Maximum tiles per picture. Zero means the codec has no tiles.
    pub max_tiles: u16,
    /// Maximum tile columns. Zero means the codec has no tiles.
    pub max_tile_cols: u16,
}

/// A coded configuration to size a level against — what `-level auto` needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LevelQuery {
    /// Picture width in luma samples.
    pub width: u32,
    /// Picture height in luma samples.
    pub height: u32,
    /// Luma samples per second, i.e. `width * height * frame_rate`.
    pub luma_sample_rate: u64,
    /// Target bit rate in kbit/s, or zero if unconstrained.
    pub bitrate_kbps: u32,
    /// Reference frames the encoder wants to keep.
    pub dpb_frames: u16,
    /// Tiles per picture, or zero.
    pub tiles: u16,
    /// Tile columns, or zero.
    pub tile_cols: u16,
}

impl LevelConstraints {
    /// Whether a configuration fits inside this level.
    #[must_use]
    pub const fn admits(&self, q: &LevelQuery) -> bool {
        let luma = (q.width as u64) * (q.height as u64);
        luma <= self.max_luma_picture_size
            && q.luma_sample_rate <= self.max_luma_sample_rate
            && q.width <= self.max_h_size
            && q.height <= self.max_v_size
            && (self.max_bitrate_kbps == 0 || q.bitrate_kbps <= self.max_bitrate_kbps)
            && q.dpb_frames <= self.max_dpb_frames
            && (self.max_tiles == 0 || q.tiles <= self.max_tiles)
            && (self.max_tile_cols == 0 || q.tile_cols <= self.max_tile_cols)
    }
}

/// One row of a codec's level table.
#[derive(Debug, Clone, Copy)]
pub struct LevelEntry {
    /// The level itself, in the codec's own encoding.
    pub level: Level,
    /// Display name, e.g. `"5.1"`.
    pub name: &'static str,
    /// What it caps.
    pub constraints: LevelConstraints,
}

/// A codec's level table, supplied by the codec crate.
///
/// The table lives with the codec — `vaco-codec-av1` supplies AV1's, from the
/// AV1 specification Annex A — and never here, because a central table would
/// mean this crate had to know every codec.
///
/// Entries must be ordered by increasing capability, which is how
/// [`LevelTable::smallest_for`] can stop at the first match.
#[derive(Debug, Clone, Copy)]
pub struct LevelTable(pub &'static [LevelEntry]);

impl LevelTable {
    /// The entry for a raw level value.
    #[must_use]
    pub fn entry(&self, level: Level) -> Option<&'static LevelEntry> {
        self.0.iter().find(|e| e.level == level)
    }

    /// What a level caps, if this table knows it.
    #[must_use]
    pub fn constraints(&self, level: Level) -> Option<&'static LevelConstraints> {
        self.entry(level).map(|e| &e.constraints)
    }

    /// A level's display name.
    #[must_use]
    pub fn name(&self, level: Level) -> Option<&'static str> {
        self.entry(level).map(|e| e.name)
    }

    /// Look a level up by display name.
    #[must_use]
    pub fn from_name(&self, name: &str) -> Option<Level> {
        self.0
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))
            .map(|e| e.level)
    }

    /// The smallest level that admits `query` — what `-level auto` picks.
    #[must_use]
    pub fn smallest_for(&self, query: &LevelQuery) -> Option<Level> {
        self.0
            .iter()
            .find(|e| e.constraints.admits(query))
            .map(|e| e.level)
    }

    /// Whether a configuration fits a level the caller has already chosen.
    #[must_use]
    pub fn admits(&self, level: Level, query: &LevelQuery) -> bool {
        self.constraints(level).is_some_and(|c| c.admits(query))
    }
}

/// One row of a codec's profile table.
#[derive(Debug, Clone, Copy)]
pub struct ProfileEntry {
    /// The profile itself.
    pub profile: Profile,
    /// Profiles whose streams this one can also decode, by raw value.
    ///
    /// AV1 Professional subsumes High subsumes Main; listing the closure
    /// explicitly keeps the lookup a scan rather than a graph walk.
    pub subsumes: &'static [i32],
}

/// A codec's profile table, supplied by the codec crate.
#[derive(Debug, Clone, Copy)]
pub struct ProfileTable(pub &'static [ProfileEntry]);

impl ProfileTable {
    /// The entry for a raw profile value.
    #[must_use]
    pub fn entry(&self, profile: Profile) -> Option<&'static ProfileEntry> {
        self.0.iter().find(|e| e.profile.value == profile.value)
    }

    /// Look a profile up by raw value.
    #[must_use]
    pub fn from_value(&self, value: i32) -> Option<Profile> {
        self.0
            .iter()
            .find(|e| e.profile.value == value)
            .map(|e| e.profile)
    }

    /// Look a profile up by name, case-insensitively — this is what
    /// `-profile:v high` goes through.
    #[must_use]
    pub fn from_name(&self, name: &str) -> Option<Profile> {
        self.0
            .iter()
            .find(|e| e.profile.name.eq_ignore_ascii_case(name))
            .map(|e| e.profile)
    }

    /// Whether a decoder that supports `profile` can also decode `other`.
    ///
    /// Reflexive, and otherwise exactly what the table lists.
    #[must_use]
    pub fn subsumes(&self, profile: Profile, other: Profile) -> bool {
        profile.value == other.value
            || self
                .entry(profile)
                .is_some_and(|e| e.subsumes.contains(&other.value))
    }
}
