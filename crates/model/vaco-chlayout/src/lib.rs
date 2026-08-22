#![forbid(unsafe_code)]
//! Channel layouts.
//!
//! Models the modern three-way distinction rather than a bare bitmask: a mask
//! cannot express more than 63 channels, cannot express ambisonics, and cannot
//! express "8 channels of unknown position", all of which occur in real files.
//!
//! # What it is
//!
//! Three things, in increasing order of subtlety:
//!
//! 1. [`Channel`] — the vocabulary of individual speaker positions, plus the
//!    three things that are not positions: an unnamed maskable slot, an
//!    ambisonic component, and the "present but undefined" and "gap" markers.
//! 2. [`ChannelLayout`] — a channel *count* plus a [`ChannelOrder`] saying how
//!    the count is interpreted. Only the `Native` order has a bitmask.
//! 3. The layout description grammar — the text `-ch_layout` accepts and
//!    `ffprobe` prints. This is where nearly all the difficulty is; see
//!    [`ChannelLayout::from_name`].
//!
//! # The vocabulary is an interface, not a design
//!
//! Channel names, layout names and the bit assignment behind the mask are facts
//! about formats and about a command line we must remain compatible with, so
//! they were recorded by probing the reference binary rather than invented. The
//! provenance of every table, and the exact probe used, is in
//! [`table`](self) and in `docs/model/vaco-chlayout.md`.
//!
//! # What this crate deliberately does not contain
//!
//! No rematrixing coefficients, no downmix policy, no "closest layout to" search.
//! Those need psychoacoustic judgement and belong in `vaco-resample`. This crate
//! is naming and structure only.

use core::fmt;
use core::num::NonZeroU8;

mod parse;
mod table;

/// A single speaker position, or an ambisonic component.
///
/// # Identity is the numeric id, not the variant
///
/// Every channel has a numeric id — the same id the `USR<n>` parse form names,
/// and, for ids below 64, the bit it occupies in a native mask. [`Channel`]
/// compares, orders and hashes by that id rather than by variant, so
/// [`Channel::Unnamed`]`(2)` and [`Channel::FrontCenter`] are the same channel.
/// Prefer [`Channel::from_id`], which never produces a redundant `Unnamed`, over
/// constructing `Unnamed` yourself.
///
/// Ordering by id means sorting a slice of channels puts them in mask order,
/// which is the order a native layout's channels are laid out in.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Channel {
    FrontLeft,
    FrontRight,
    FrontCenter,
    LowFrequency,
    BackLeft,
    BackRight,
    FrontLeftOfCenter,
    FrontRightOfCenter,
    BackCenter,
    SideLeft,
    SideRight,
    TopCenter,
    TopFrontLeft,
    TopFrontCenter,
    TopFrontRight,
    TopBackLeft,
    TopBackCenter,
    TopBackRight,
    /// `DL`. The left channel of a matrix-encoded stereo downmix.
    DownmixLeft,
    /// `DR`.
    DownmixRight,
    WideLeft,
    WideRight,
    SurroundDirectLeft,
    SurroundDirectRight,
    LowFrequency2,
    TopSideLeft,
    TopSideRight,
    BottomFrontCenter,
    BottomFrontLeft,
    BottomFrontRight,
    SideSurroundLeft,
    SideSurroundRight,
    TopSurroundLeft,
    TopSurroundRight,
    BinauralLeft,
    BinauralRight,
    /// A slot with an id but no assigned meaning: `USR<n>`.
    ///
    /// Ids below 64 are maskable, so an unnamed one still occupies a bit and can
    /// appear in a `Native` layout. The gaps are 18..=28, 45..=60 and 63.
    Unnamed(u32),
    /// An ACN-ordered ambisonic component: `AMBI<n>`, `n` in `0..=1023`.
    ///
    /// Individually addressable only in a `Custom` order; a complete set is
    /// canonicalised to [`ChannelOrder::Ambisonic`].
    Ambisonic(u16),
    /// Present in the stream but carrying no defined position.
    Unknown,
    /// A gap: the slot exists but carries nothing.
    Unused,
}

impl Channel {
    /// Numeric id of [`Channel::Unused`]. Ids are dense apart from this and the
    /// two below, which sit above every maskable position.
    const ID_UNUSED: u32 = 512;
    /// Numeric id of [`Channel::Unknown`].
    const ID_UNKNOWN: u32 = 768;
    /// Numeric id of `AMBI0`; components run to `AMBI1023` at 2047.
    const ID_AMBISONIC: u32 = 1024;
    /// Ids are parsed out of a C `int`, so this is the largest one that exists.
    const ID_MAX: u32 = i32::MAX as u32;

    /// Every channel that has a name, in bit order — the order `-layouts`
    /// prints them in.
    #[must_use]
    pub fn named() -> impl ExactSizeIterator<Item = Self> {
        table::CHANNELS.into_iter().map(|(c, ..)| c)
    }

    /// The numeric id: the value `USR<n>` names, and the mask bit when below 64.
    #[must_use]
    pub const fn id(self) -> u32 {
        match self {
            Self::FrontLeft => 0,
            Self::FrontRight => 1,
            Self::FrontCenter => 2,
            Self::LowFrequency => 3,
            Self::BackLeft => 4,
            Self::BackRight => 5,
            Self::FrontLeftOfCenter => 6,
            Self::FrontRightOfCenter => 7,
            Self::BackCenter => 8,
            Self::SideLeft => 9,
            Self::SideRight => 10,
            Self::TopCenter => 11,
            Self::TopFrontLeft => 12,
            Self::TopFrontCenter => 13,
            Self::TopFrontRight => 14,
            Self::TopBackLeft => 15,
            Self::TopBackCenter => 16,
            Self::TopBackRight => 17,
            Self::DownmixLeft => 29,
            Self::DownmixRight => 30,
            Self::WideLeft => 31,
            Self::WideRight => 32,
            Self::SurroundDirectLeft => 33,
            Self::SurroundDirectRight => 34,
            Self::LowFrequency2 => 35,
            Self::TopSideLeft => 36,
            Self::TopSideRight => 37,
            Self::BottomFrontCenter => 38,
            Self::BottomFrontLeft => 39,
            Self::BottomFrontRight => 40,
            Self::SideSurroundLeft => 41,
            Self::SideSurroundRight => 42,
            Self::TopSurroundLeft => 43,
            Self::TopSurroundRight => 44,
            Self::BinauralLeft => 61,
            Self::BinauralRight => 62,
            Self::Unnamed(n) => n,
            Self::Ambisonic(n) => Self::ID_AMBISONIC + n as u32,
            Self::Unknown => Self::ID_UNKNOWN,
            Self::Unused => Self::ID_UNUSED,
        }
    }

    /// The channel with this id, in canonical form.
    ///
    /// Returns `None` above [`i32::MAX`], which is where the reference's own
    /// parser stops. Every id below that names some channel, so this is
    /// otherwise total.
    #[must_use]
    pub fn from_id(id: u32) -> Option<Self> {
        if id > Self::ID_MAX {
            return None;
        }
        if let Some(&(c, ..)) = table::CHANNELS.iter().find(|e| u32::from(e.1) == id) {
            return Some(c);
        }
        Some(match id {
            Self::ID_UNUSED => Self::Unused,
            Self::ID_UNKNOWN => Self::Unknown,
            n if (Self::ID_AMBISONIC..Self::ID_AMBISONIC + 1024).contains(&n) => {
                Self::Ambisonic((n - Self::ID_AMBISONIC) as u16)
            }
            n => Self::Unnamed(n),
        })
    }

    /// The bit this channel occupies in a native mask, if it has one.
    ///
    /// Only ids below 64 are maskable — which excludes [`Channel::Unknown`],
    /// [`Channel::Unused`] and every ambisonic component, and is why a layout
    /// containing any of them cannot use the `Native` order.
    #[must_use]
    pub const fn bit(self) -> Option<u8> {
        let id = self.id();
        if id < 64 { Some(id as u8) } else { None }
    }

    /// The short name, for the channels that have a fixed one.
    ///
    /// `None` for [`Channel::Unnamed`] and [`Channel::Ambisonic`], whose names
    /// carry a number — use the [`fmt::Display`] impl, which spells every
    /// channel the way the reference does.
    #[must_use]
    pub fn short_name(self) -> Option<&'static str> {
        match self {
            Self::Unnamed(_) | Self::Ambisonic(_) => None,
            Self::Unknown => Some("UNK"),
            Self::Unused => Some("UNSD"),
            other => table::CHANNELS
                .iter()
                .find(|e| u32::from(e.1) == other.id())
                .map(|e| e.2),
        }
    }

    /// The human-readable description `-layouts` prints beside the name.
    ///
    /// `None` for everything the reference does not list there, which is
    /// [`Channel::Unknown`], [`Channel::Unused`], and the numbered forms.
    #[must_use]
    pub fn description(self) -> Option<&'static str> {
        table::CHANNELS
            .iter()
            .find(|e| u32::from(e.1) == self.id())
            .map(|e| e.3)
    }

    /// Parse a channel name: `FL`, `LFE2`, `UNK`, `UNSD`, `USR<n>`, `AMBI<n>`.
    ///
    /// See [`ChannelLayout::from_name`] for the surrounding grammar and the
    /// D17 notes on the numeric forms.
    #[must_use]
    pub fn from_name(s: &str) -> Option<Self> {
        parse::channel(s)
    }
}

/// The reference's spelling: a short name, or `USR<n>` / `AMBI<n>`.
impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Ambisonic(n) => write!(f, "AMBI{n}"),
            other => match other.short_name() {
                Some(n) => f.write_str(n),
                None => write!(f, "USR{}", other.id()),
            },
        }
    }
}

impl PartialEq for Channel {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for Channel {}

impl PartialOrd for Channel {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Mask order: sorting a slice of channels puts them in the order a native
/// layout lays them out.
impl Ord for Channel {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.id().cmp(&other.id())
    }
}

impl core::hash::Hash for Channel {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.id().hash(state);
    }
}

/// A per-channel display name, as in `FL@Left`.
///
/// # The 15-byte cap is part of the type
///
/// The reference stores a label in a fixed 16-byte, NUL-terminated buffer, so a
/// longer one is **silently truncated** rather than rejected:
/// `FL@0123456789abcdef` describes back as `FL@0123456789abcde`. That is
/// observable output, so it is reproduced — and it is enforced here, in the
/// constructor, rather than at the parser boundary, so there is no way to build
/// a `Label` the reference could not have produced.
///
/// # D17: the reference truncates by byte, mid-character
///
/// `strncpy` into a `char[16]` does not know about UTF-8. Feeding nine `é`
/// (18 bytes) makes the reference emit fourteen bytes of `é` plus the *lead
/// byte* of the fifteenth — a broken sequence, which is what it then prints.
///
/// We cut at the last character boundary at or below 15 bytes instead, which
/// for that input keeps seven `é` and drops the dangling byte. This is the one
/// place we knowingly do not reproduce the reference byte for byte, and the
/// reason is that its output is **not a fixed point of its own grammar**: our
/// `describe` returns a `String`, a broken sequence cannot survive in one, and
/// any lossy rendering of it re-parses to a third value. A label that is pure
/// ASCII — which is every label in the recorded corpus and every label a command
/// line realistically carries — is byte-identical either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label {
    /// Valid UTF-8, `len` bytes of it. Never empty: an empty label is `None`.
    bytes: [u8; Self::CAP],
    /// `NonZero` because a `Label` is never empty — and because that gives
    /// `Option<Label>` a niche to put its `None` in, so the option is free.
    /// A `ChannelEntry` is 24 bytes with this and 28 without, and that
    /// difference is multiplied by however many entries a layout holds.
    len: NonZeroU8,
}

impl Label {
    /// The longest label the reference keeps, in bytes.
    pub const CAP: usize = 15;

    /// Truncate `text` to a label, or `None` if it is empty.
    ///
    /// An empty label is the same as no label at all — `FL@+FR` is plain
    /// `stereo` — which is why this returns an `Option` rather than an empty
    /// `Label`.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        // The greatest character boundary at or below the cap. `floor_char_boundary`
        // is still unstable, so walk down from the cap; at most three steps.
        let mut end = text.len().min(Self::CAP);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let kept = text.get(..end)?;
        if kept.is_empty() {
            return None;
        }
        let mut bytes = [0u8; Self::CAP];
        bytes
            .get_mut(..kept.len())?
            .copy_from_slice(kept.as_bytes());
        Some(Self {
            bytes,
            len: NonZeroU8::new(u8::try_from(kept.len()).ok()?)?,
        })
    }

    /// The text. Never empty.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.bytes
            .get(..self.len.get() as usize)
            .and_then(|b| core::str::from_utf8(b).ok())
            .unwrap_or("")
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry of a [`ChannelOrder::Custom`] map: a channel and its optional
/// display label.
///
/// A tuple rather than a struct because it is what a caller building a map
/// naturally writes, and because the label is genuinely secondary — everything
/// that reasons about a layout reasons about the [`Channel`].
pub type ChannelEntry = (Channel, Option<Label>);

/// The per-channel map of a [`ChannelOrder::Custom`] layout.
///
/// # Why a boxed slice and not a `SmallVec`
///
/// A [`ChannelLayout`] is embedded **by value** in every audio frame, so its
/// size is paid on the hot path whether or not a layout is custom — and the
/// overwhelmingly common layout is [`ChannelOrder::Native`], which is a bare
/// mask and needs no map at all. Inlining eight entries to spare the rare case
/// an allocation cost the common case 184 bytes per frame:
///
/// | map type | `ChannelOrder` | `ChannelLayout` |
/// |---|---|---|
/// | `SmallVec<[ChannelEntry; 8]>` | 208 | 224 |
/// | `SmallVec<[ChannelEntry; 2]>` | 72 | 88 |
/// | `Box<[ChannelEntry]>` | **24** | **40** |
///
/// A boxed slice allocates only when a layout actually is custom, which is the
/// case that was already off the hot path, and an empty one does not allocate at
/// all — so an ambisonic layout with no extras stays free. `layout_stays_small`
/// pins the result.
pub type ChannelMap = Box<[ChannelEntry]>;

/// How a layout's channel positions are described.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOrder {
    /// Count is known, positions are not.
    Unspecified,
    /// Positions given by a bitmask in the conventional order.
    Native,
    /// An explicit per-index map, permitting gaps, arbitrary order, repeats,
    /// channels that no mask can hold, and a per-channel display label.
    ///
    /// A label is not decoration: it is what keeps a layout custom instead of
    /// canonicalising, so `FL@Left+FR@Right` stays a two-entry map where
    /// `FL+FR` collapses to `stereo`.
    Custom(ChannelMap),
    /// ACN-ordered ambisonic components, optionally with non-diegetic extras.
    ///
    /// `order` is a `u16` because the reference's parser accepts every order
    /// whose `(order + 1)^2` component count fits an `i32` — up to 46 339 — and
    /// rejects the rest itself. A narrower field here would reject a band of
    /// orders the reference accepts, at a different stage and with a different
    /// message.
    ///
    /// The ACN components themselves are implied by `order` and carry no
    /// labels; the reference discards a label written on one, so
    /// `AMBI0@z+AMBI1+AMBI2+AMBI3` is just `ambisonic 1`. The extras keep
    /// theirs.
    Ambisonic { order: u16, extra: ChannelMap },
}

/// A channel count plus the interpretation of that count.
///
/// The mask is private because it is meaningful only under
/// [`ChannelOrder::Native`]; read it with [`ChannelLayout::mask`], which returns
/// `0` for every other order rather than a stale value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelLayout {
    pub channels: u32,
    pub order: ChannelOrder,
    mask: u64,
}

impl ChannelLayout {
    pub const MONO: Self = Self {
        channels: 1,
        order: ChannelOrder::Native,
        mask: 0x4,
    };
    pub const STEREO: Self = Self {
        channels: 2,
        order: ChannelOrder::Native,
        mask: 0x3,
    };

    // -------------------------------------------------------- constructors

    /// A layout of `channels` channels whose positions are not known.
    ///
    /// This is a real, useful state — a raw PCM stream with no header says how
    /// many channels it has and nothing more — and it is why the model is not
    /// just a bitmask.
    #[must_use]
    pub const fn unspecified(channels: u32) -> Self {
        Self {
            channels,
            order: ChannelOrder::Unspecified,
            mask: 0,
        }
    }

    /// A native layout from a bitmask. `None` for an empty mask.
    #[must_use]
    pub const fn from_mask(mask: u64) -> Option<Self> {
        if mask == 0 {
            return None;
        }
        Some(Self {
            channels: mask.count_ones(),
            order: ChannelOrder::Native,
            mask,
        })
    }

    /// A layout from an explicit channel list, canonicalised.
    ///
    /// Canonicalisation is not cosmetic — it is what the reference does, and it
    /// decides what [`describe`](Self::describe) prints:
    ///
    /// * all channels [`Channel::Unknown`] becomes [`ChannelOrder::Unspecified`];
    /// * a strictly ascending list of maskable channels becomes
    ///   [`ChannelOrder::Native`], so `FL+FR` is `stereo` but `FR+FL` is not;
    /// * a leading run of `AMBI0..AMBI(k-1)` whose length is a perfect square,
    ///   followed by no further ambisonic components, becomes
    ///   [`ChannelOrder::Ambisonic`];
    /// * anything else stays [`ChannelOrder::Custom`].
    ///
    /// `None` for an empty list.
    #[must_use]
    pub fn custom<I: IntoIterator<Item = Channel>>(channels: I) -> Option<Self> {
        Self::custom_labelled(channels.into_iter().map(|c| (c, None)))
    }

    /// As [`custom`](Self::custom), with a display label per channel.
    ///
    /// A labelled channel blocks the collapse to `Native` and to `Unspecified`
    /// — the label has nowhere to live in either — but not the collapse to
    /// `Ambisonic`, which discards labels on its ACN components exactly as the
    /// reference does.
    #[must_use]
    pub fn custom_labelled<I: IntoIterator<Item = ChannelEntry>>(entries: I) -> Option<Self> {
        Self::from_channel_list(entries.into_iter().collect())
    }

    /// An ambisonic layout of the given order, with optional non-diegetic
    /// extras appended after the `(order + 1)^2` ACN components.
    ///
    /// `None` if the extras contain an ambisonic component, or if the total
    /// channel count would not fit a `u32`.
    #[must_use]
    pub fn ambisonic<I: IntoIterator<Item = Channel>>(order: u16, extra: I) -> Option<Self> {
        Self::ambisonic_labelled(order, extra.into_iter().map(|c| (c, None)))
    }

    /// As [`ambisonic`](Self::ambisonic), with a display label per extra
    /// channel.
    ///
    /// `None` if the extras contain an ambisonic component, or if the component
    /// count would not fit an `i32` — which is where the reference's own parser
    /// stops, at order 46 340.
    #[must_use]
    pub fn ambisonic_labelled<I: IntoIterator<Item = ChannelEntry>>(
        order: u16,
        extra: I,
    ) -> Option<Self> {
        let extra: Vec<ChannelEntry> = extra.into_iter().collect();
        if extra
            .iter()
            .any(|(c, _)| matches!(c, Channel::Ambisonic(_)))
        {
            return None;
        }
        let base = (u32::from(order) + 1).checked_mul(u32::from(order) + 1)?;
        // The reference counts components in an `int` and rejects the layout
        // outright once the square overflows one. Reproduced here rather than in
        // the parser so that every path agrees.
        if base > i32::MAX as u32 {
            return None;
        }
        let channels = base.checked_add(u32::try_from(extra.len()).ok()?)?;
        Some(Self {
            channels,
            order: ChannelOrder::Ambisonic {
                order,
                extra: extra.into_boxed_slice(),
            },
            mask: 0,
        })
    }

    /// The layout the reference picks for a bare channel count — the `<n>c`
    /// parse form.
    ///
    /// Defined as *the first standard layout with exactly `n` channels*, in
    /// [`table::LAYOUTS`] order. There is no default for a count no standard
    /// layout has, which is why `9c`, `11c` and `18c` are errors rather than
    /// falling back to an unspecified layout.
    #[must_use]
    pub fn default_for(channels: u32) -> Option<Self> {
        table::LAYOUTS
            .iter()
            .find(|&&(_, mask)| mask.count_ones() == channels)
            .and_then(|&(_, mask)| Self::from_mask(mask))
    }

    /// Every standard layout, as `(name, layout)`, in `-layouts` order.
    #[must_use]
    pub fn standard() -> impl ExactSizeIterator<Item = (&'static str, Self)> {
        table::LAYOUTS.into_iter().map(|(name, mask)| {
            (
                name,
                Self {
                    channels: mask.count_ones(),
                    order: ChannelOrder::Native,
                    mask,
                },
            )
        })
    }

    // ------------------------------------------------------------ accessors

    /// The bitmask, or `0` when the order is not [`ChannelOrder::Native`].
    #[must_use]
    pub const fn mask(&self) -> u64 {
        match self.order {
            ChannelOrder::Native => self.mask,
            _ => 0,
        }
    }

    /// Whether the layout is internally consistent.
    ///
    /// The struct's `channels` and `order` fields are public, so a caller can
    /// put them out of step; and the reference's parser accepts some strings
    /// that produce a structurally invalid layout — `ambisonic 1+4 channels`
    /// parses and is then rejected downstream. This is that check.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.channels == 0 {
            return false;
        }
        match &self.order {
            ChannelOrder::Unspecified => true,
            ChannelOrder::Native => self.mask.count_ones() == self.channels,
            ChannelOrder::Custom(map) => u32::try_from(map.len()).is_ok_and(|n| n == self.channels),
            ChannelOrder::Ambisonic { order, extra } => {
                let base = (u32::from(*order) + 1).pow(2);
                u32::try_from(extra.len())
                    .ok()
                    .and_then(|n| base.checked_add(n))
                    .is_some_and(|n| n == self.channels)
                    && !extra
                        .iter()
                        .any(|(c, _)| matches!(c, Channel::Ambisonic(_) | Channel::Unknown))
            }
        }
    }

    /// The canonical name, if this layout has one.
    ///
    /// Only a [`ChannelOrder::Native`] layout can have one: a name is a name for
    /// a *set* of positions in the conventional order, and that is exactly what
    /// a mask is. `FL+FC` has no name; `FL+FR` is `stereo`; `FR+FL` is neither,
    /// because reordering it is a different layout.
    #[must_use]
    pub fn name(&self) -> Option<&'static str> {
        if !matches!(self.order, ChannelOrder::Native) {
            return None;
        }
        table::LAYOUTS
            .iter()
            .find(|&&(_, mask)| mask == self.mask)
            .map(|&(name, _)| name)
    }

    /// The channel at a given index, in stream order.
    #[must_use]
    pub fn channel_at(&self, index: u32) -> Option<Channel> {
        if index >= self.channels {
            return None;
        }
        match &self.order {
            // An unspecified layout has channels; it just does not know where
            // they are. That is `Unknown`, not "no channel".
            //
            // The reference returns its `AV_CHAN_NONE` sentinel here instead —
            // a third non-channel value distinct from `UNK` and `UNSD`, which
            // it prints as `NONE`. We have no such value because `Option` is
            // the sentinel, and the difference is visible in exactly one place:
            // `ambisonic 3+3 channels`, where the reference materialises the
            // extras as `NONE+NONE+NONE` and we leave them `UNK`. That layout
            // is structurally invalid in both implementations — see
            // `is_valid` — so nothing downstream ever sees either answer.
            ChannelOrder::Unspecified => Some(Channel::Unknown),
            ChannelOrder::Native => nth_set_bit(self.mask, index).and_then(Channel::from_id),
            ChannelOrder::Custom(map) => map.get(index as usize).map(|&(c, _)| c),
            ChannelOrder::Ambisonic { order, extra } => {
                let base = (u32::from(*order) + 1).pow(2);
                if index < base {
                    u16::try_from(index).ok().map(Channel::Ambisonic)
                } else {
                    extra.get((index - base) as usize).map(|&(c, _)| c)
                }
            }
        }
    }

    /// The index of a channel, or `None` if the layout does not carry it.
    ///
    /// Always `None` for an unspecified layout — it has channels but no answer
    /// to "where is the centre".
    #[must_use]
    pub fn index_of(&self, channel: Channel) -> Option<u32> {
        match &self.order {
            ChannelOrder::Unspecified => None,
            ChannelOrder::Native => {
                let bit = channel.bit()?;
                if self.mask >> bit & 1 == 0 {
                    return None;
                }
                // Channels below this bit come before it.
                Some((self.mask & ((1u64 << bit) - 1)).count_ones())
            }
            _ => (0..self.channels).find(|&i| self.channel_at(i) == Some(channel)),
        }
    }

    /// The display label at a given index, if the channel carries one.
    ///
    /// Only a `Custom` map and the extras of an `Ambisonic` layout can carry
    /// labels; every other order returns `None`, as does an index past the end.
    #[must_use]
    pub fn label_at(&self, index: u32) -> Option<&Label> {
        if index >= self.channels {
            return None;
        }
        match &self.order {
            ChannelOrder::Unspecified | ChannelOrder::Native => None,
            ChannelOrder::Custom(map) => map.get(index as usize)?.1.as_ref(),
            ChannelOrder::Ambisonic { order, extra } => {
                let base = (u32::from(*order) + 1).pow(2);
                extra.get(index.checked_sub(base)? as usize)?.1.as_ref()
            }
        }
    }

    /// Whether the layout carries this channel.
    #[must_use]
    pub fn contains(&self, channel: Channel) -> bool {
        self.index_of(channel).is_some()
    }

    /// Every channel, in stream order.
    pub fn iter(&self) -> impl Iterator<Item = Channel> + '_ {
        (0..self.channels).filter_map(|i| self.channel_at(i))
    }

    // ----------------------------------------------------- text conversions

    /// Parse a CLI-facing layout string such as `5.1` or `FL+FR`.
    ///
    /// See [`parse`](self::parse) for the grammar and for the D17 notes on the
    /// edge cases, of which there are many.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        parse::layout(name)
    }

    /// The full description — what `ffprobe` prints and what
    /// [`from_name`](Self::from_name) accepts back.
    ///
    /// Four shapes:
    ///
    /// | order | text |
    /// |---|---|
    /// | `Unspecified` | `6 channels` |
    /// | `Native`, named | `5.1` |
    /// | `Native`, unnamed | `2 channels (FL+FC)` |
    /// | `Custom` | `2 channels (FC+FL)` |
    /// | `Ambisonic` | `ambisonic 1`, `ambisonic 1+stereo` |
    ///
    /// # D17: `1 channels`
    ///
    /// The count is never pluralised, so a one-channel unspecified layout
    /// describes as `1 channels`. That text is what `ffmpeg`'s stream banner
    /// prints, so it is reproduced rather than corrected.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut out = String::new();
        self.write_description(&mut out);
        out
    }

    fn write_description(&self, out: &mut String) {
        use fmt::Write as _;

        match &self.order {
            ChannelOrder::Unspecified => {
                let _ = write!(out, "{} channels", self.channels);
            }
            ChannelOrder::Native | ChannelOrder::Custom(_) => {
                if let Some(name) = self.name() {
                    out.push_str(name);
                    return;
                }
                let _ = write!(out, "{} channels (", self.channels);
                for i in 0..self.channels {
                    if i > 0 {
                        out.push('+');
                    }
                    if let Some(ch) = self.channel_at(i) {
                        let _ = write!(out, "{ch}");
                    }
                    if let Some(label) = self.label_at(i) {
                        let _ = write!(out, "@{label}");
                    }
                }
                out.push(')');
            }
            ChannelOrder::Ambisonic { order, extra } => {
                let _ = write!(out, "ambisonic {order}");
                if !extra.is_empty() {
                    out.push('+');
                    // The extras are themselves a layout, and are described as
                    // one — which is why `ambisonic 1+stereo` says `stereo`
                    // rather than `FL+FR`.
                    match Self::from_channel_list(extra.to_vec()) {
                        Some(sub) => sub.write_description(out),
                        None => out.push_str("0 channels"),
                    }
                }
            }
        }
    }

    // -------------------------------------------------------------- internal

    /// Build a layout from a channel list, canonicalising it.
    ///
    /// Canonicalisation is unconditional, and that is what makes
    /// [`describe`](Self::describe) a fixed point: a `ChannelOrder::Custom`
    /// value is by construction one that no other order could express, so
    /// describing it and parsing the result lands back on the same value.
    fn from_channel_list(list: Vec<ChannelEntry>) -> Option<Self> {
        let channels = u32::try_from(list.len()).ok()?;
        if channels == 0 {
            return None;
        }
        let custom = |list: Vec<ChannelEntry>| Self {
            channels,
            order: ChannelOrder::Custom(list.into_boxed_slice()),
            mask: 0,
        };

        // A complete leading ACN set, and nothing ambisonic after it.
        //
        // Checked before the two collapses below because it is the one the
        // reference lets a label through: `AMBI0@z+AMBI1+AMBI2+AMBI3` is plain
        // `ambisonic 1`, the label on the component discarded. Labels on the
        // *extras* survive, which is why the tail keeps its entries whole.
        let acn = list
            .iter()
            .enumerate()
            .take_while(|&(i, (c, _))| u16::try_from(i).is_ok_and(|i| *c == Channel::Ambisonic(i)))
            .count();
        if acn > 0 {
            let root = acn.isqrt();
            if root * root == acn
                && root <= usize::from(u16::MAX) + 1
                && !list
                    .iter()
                    .skip(acn)
                    .any(|(c, _)| matches!(c, Channel::Ambisonic(_)))
            {
                let order = u16::try_from(root - 1).ok()?;
                return Self::ambisonic_labelled(order, list.iter().skip(acn).copied());
            }
        }

        // The remaining two collapses both throw a label away, so a label
        // blocks them: `UNK@x+UNK` and `FL@x+FC` stay custom where `UNK+UNK` is
        // unspecified and `FL+FC` is a mask. This is not a nicety — it is what
        // makes `describe` a fixed point, since a `Custom` value is then always
        // one that no other order could have expressed.
        if list.iter().any(|(_, label)| label.is_some()) {
            return Some(custom(list));
        }

        // All-unknown is exactly what "we have N channels and no idea" means.
        if list.iter().all(|&(c, _)| c == Channel::Unknown) {
            return Some(Self::unspecified(channels));
        }

        // Strictly ascending and entirely maskable is a native layout. Strictly:
        // a repeated channel is not a mask, and a descending pair is a genuinely
        // different layout from its sorted form.
        let mut mask = 0u64;
        let mut prev: Option<u8> = None;
        for &(ch, _) in &list {
            let Some(bit) = ch.bit() else {
                return Some(custom(list));
            };
            if prev.is_some_and(|p| bit <= p) {
                return Some(custom(list));
            }
            prev = Some(bit);
            mask |= 1u64 << bit;
        }
        Some(Self {
            channels,
            order: ChannelOrder::Native,
            mask,
        })
    }
}

/// The description — the same text [`ChannelLayout::describe`] returns.
impl fmt::Display for ChannelLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

/// The id of the `index`-th set bit of `mask`, counting from the least
/// significant.
fn nth_set_bit(mask: u64, index: u32) -> Option<u32> {
    let mut rest = mask;
    for _ in 0..index {
        if rest == 0 {
            return None;
        }
        rest &= rest - 1;
    }
    if rest == 0 {
        None
    } else {
        Some(rest.trailing_zeros())
    }
}

#[cfg(test)]
mod tests;
