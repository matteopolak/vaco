//! The Matroska and EBML element schema, as a flat table.
//!
//! Every row is transcribed from the element definitions in RFC 9559 section 5
//! (Matroska) and RFC 8794 sections 11.2 and 11.1.6 (the EBML header elements
//! and the two global ones). The parent column is the element path each
//! definition states, expressed as the parent's ID so that the whole schema is
//! one flat array with no strings to walk.
//!
//! The table is what makes RFC 8794 section 6.2 implementable: terminating an
//! unknown-size element requires knowing, for an arbitrary ID, whether it is a
//! legal child of the element currently open. See [`super::MatroskaStack`].
//!
//! # What is deliberately absent
//!
//! RFC 9559's IANA registry (section 27.1) lists 254 IDs; section 5 defines 207.
//! The difference is the deprecated and reserved set — `Slices`, `TrickTrack*`,
//! `SilentTracks`, `AspectRatioType` and the rest. They are left out on purpose:
//! an ID the schema does not define is skipped by its size and, per section 6.2,
//! cannot terminate an unknown-size element, which is exactly the treatment a
//! Matroska v4 reader owes them.
//!
//! # Most of these named constants have no caller, on purpose
//!
//! [`ELEMENTS`] below states every entry's `id` as a raw hex literal, not by
//! name — the schema table has to be one flat array [`super::MatroskaStack`]
//! can walk by numeric ID, and repeating each constant's own definition as
//! its initializer would only add an indirection nothing reads back. A named
//! constant here exists for two independent reasons that both stop at
//! "declared," not "consumed elsewhere": documenting which RFC element a
//! given ID is, and giving `demux.rs` a name to match on for the specific
//! elements this crate actually parses today (`el::TAGEDITIONUID` and
//! similar). An element this crate does not yet act on by name — block
//! addition IDs, chapter-processing scripts, codec private state, track
//! translation, stereo mode, and the rest of `cargo xtask dead-code`'s
//! report for this crate — still needs its ID recognised by [`ELEMENTS`]
//! for RFC 8794 section 6.2's unknown-size termination rule to hold, even
//! though nothing downstream reads its *value* yet. That is a real,
//! declared gap in what this demuxer parses, not a leftover: the schema is
//! complete on purpose (see above), the parser is not, and conflating "the
//! ID is known" with "the element is handled" would be the wrong fix for
//! either.

#![allow(
    clippy::unreadable_literal,
    reason = "an element ID is written here exactly as RFC 9559 and RFC 8794 write               it; inserting separators would make the table no longer greppable               against the specification, which is the only thing that makes it               checkable"
)]

use super::{ElementDef, ElementKind};

/// Parent ID used for the two root elements, `EBML` and `Segment`.
pub const ROOT: u32 = 0;

/// Parent ID used for global elements, which are legal inside any master.
pub const GLOBAL: u32 = u32::MAX;

pub const CHAPTERDISPLAY: u32 = 0x80;
pub const TRACKTYPE: u32 = 0x83;
pub const CHAPSTRING: u32 = 0x85;
pub const CODECID: u32 = 0x86;
pub const FLAGDEFAULT: u32 = 0x88;
pub const CHAPTERTIMESTART: u32 = 0x91;
pub const CHAPTERTIMEEND: u32 = 0x92;
pub const CUEREFTIME: u32 = 0x96;
pub const CHAPTERFLAGHIDDEN: u32 = 0x98;
pub const FLAGINTERLACED: u32 = 0x9A;
pub const BLOCKDURATION: u32 = 0x9B;
pub const FLAGLACING: u32 = 0x9C;
pub const FIELDORDER: u32 = 0x9D;
pub const CHANNELS: u32 = 0x9F;
pub const BLOCKGROUP: u32 = 0xA0;
pub const BLOCK: u32 = 0xA1;
pub const SIMPLEBLOCK: u32 = 0xA3;
pub const CODECSTATE: u32 = 0xA4;
pub const BLOCKADDITIONAL: u32 = 0xA5;
pub const BLOCKMORE: u32 = 0xA6;
pub const POSITION: u32 = 0xA7;
pub const PREVSIZE: u32 = 0xAB;
pub const TRACKENTRY: u32 = 0xAE;
pub const PIXELWIDTH: u32 = 0xB0;
pub const CUEDURATION: u32 = 0xB2;
pub const CUETIME: u32 = 0xB3;
pub const SAMPLINGFREQUENCY: u32 = 0xB5;
pub const CHAPTERATOM: u32 = 0xB6;
pub const CUETRACKPOSITIONS: u32 = 0xB7;
pub const FLAGENABLED: u32 = 0xB9;
pub const PIXELHEIGHT: u32 = 0xBA;
pub const CUEPOINT: u32 = 0xBB;
pub const CRC32: u32 = 0xBF;
pub const TRACKNUMBER: u32 = 0xD7;
pub const CUEREFERENCE: u32 = 0xDB;
pub const VIDEO: u32 = 0xE0;
pub const AUDIO: u32 = 0xE1;
pub const TRACKOPERATION: u32 = 0xE2;
pub const TRACKCOMBINEPLANES: u32 = 0xE3;
pub const TRACKPLANE: u32 = 0xE4;
pub const TRACKPLANEUID: u32 = 0xE5;
pub const TRACKPLANETYPE: u32 = 0xE6;
pub const TIMESTAMP: u32 = 0xE7;
pub const TRACKJOINBLOCKS: u32 = 0xE9;
pub const CUECODECSTATE: u32 = 0xEA;
pub const VOID: u32 = 0xEC;
pub const TRACKJOINUID: u32 = 0xED;
pub const BLOCKADDID: u32 = 0xEE;
pub const CUERELATIVEPOSITION: u32 = 0xF0;
pub const CUECLUSTERPOSITION: u32 = 0xF1;
pub const CUETRACK: u32 = 0xF7;
pub const REFERENCEPRIORITY: u32 = 0xFA;
pub const REFERENCEBLOCK: u32 = 0xFB;
pub const BLOCKADDIDNAME: u32 = 0x41A4;
pub const BLOCKADDITIONMAPPING: u32 = 0x41E4;
pub const BLOCKADDIDTYPE: u32 = 0x41E7;
pub const BLOCKADDIDEXTRADATA: u32 = 0x41ED;
pub const BLOCKADDIDVALUE: u32 = 0x41F0;
pub const CONTENTCOMPALGO: u32 = 0x4254;
pub const CONTENTCOMPSETTINGS: u32 = 0x4255;
pub const DOCTYPEEXTENSION: u32 = 0x4281;
pub const DOCTYPE: u32 = 0x4282;
pub const DOCTYPEEXTENSIONNAME: u32 = 0x4283;
pub const DOCTYPEEXTENSIONVERSION: u32 = 0x4284;
pub const DOCTYPEREADVERSION: u32 = 0x4285;
pub const EBMLVERSION: u32 = 0x4286;
pub const DOCTYPEVERSION: u32 = 0x4287;
pub const EBMLMAXIDLENGTH: u32 = 0x42F2;
pub const EBMLMAXSIZELENGTH: u32 = 0x42F3;
pub const EBMLREADVERSION: u32 = 0x42F7;
pub const CHAPLANGUAGE: u32 = 0x437C;
pub const CHAPLANGUAGEBCP47: u32 = 0x437D;
pub const CHAPCOUNTRY: u32 = 0x437E;
pub const SEGMENTFAMILY: u32 = 0x4444;
pub const DATEUTC: u32 = 0x4461;
pub const TAGLANGUAGE: u32 = 0x447A;
pub const TAGLANGUAGEBCP47: u32 = 0x447B;
pub const TAGDEFAULT: u32 = 0x4484;
pub const TAGBINARY: u32 = 0x4485;
pub const TAGSTRING: u32 = 0x4487;
pub const DURATION: u32 = 0x4489;
pub const CHAPPROCESSPRIVATE: u32 = 0x450D;
pub const TAGNAME: u32 = 0x45A3;
pub const EDITIONENTRY: u32 = 0x45B9;
pub const EDITIONUID: u32 = 0x45BC;
pub const EDITIONFLAGDEFAULT: u32 = 0x45DB;
pub const EDITIONFLAGORDERED: u32 = 0x45DD;
pub const FILEDATA: u32 = 0x465C;
pub const FILEMEDIATYPE: u32 = 0x4660;
pub const FILENAME: u32 = 0x466E;
pub const FILEDESCRIPTION: u32 = 0x467E;
pub const FILEUID: u32 = 0x46AE;
pub const CONTENTENCALGO: u32 = 0x47E1;
pub const CONTENTENCKEYID: u32 = 0x47E2;
pub const CONTENTENCAESSETTINGS: u32 = 0x47E7;
pub const AESSETTINGSCIPHERMODE: u32 = 0x47E8;
pub const MUXINGAPP: u32 = 0x4D80;
pub const SEEK: u32 = 0x4DBB;
pub const CONTENTENCODINGORDER: u32 = 0x5031;
pub const CONTENTENCODINGSCOPE: u32 = 0x5032;
pub const CONTENTENCODINGTYPE: u32 = 0x5033;
pub const CONTENTCOMPRESSION: u32 = 0x5034;
pub const CONTENTENCRYPTION: u32 = 0x5035;
pub const NAME: u32 = 0x536E;
pub const CUEBLOCKNUMBER: u32 = 0x5378;
pub const SEEKID: u32 = 0x53AB;
pub const SEEKPOSITION: u32 = 0x53AC;
pub const STEREOMODE: u32 = 0x53B8;
pub const OLDSTEREOMODE: u32 = 0x53B9;
pub const ALPHAMODE: u32 = 0x53C0;
pub const PIXELCROPBOTTOM: u32 = 0x54AA;
pub const DISPLAYWIDTH: u32 = 0x54B0;
pub const DISPLAYUNIT: u32 = 0x54B2;
pub const DISPLAYHEIGHT: u32 = 0x54BA;
pub const PIXELCROPTOP: u32 = 0x54BB;
pub const PIXELCROPLEFT: u32 = 0x54CC;
pub const PIXELCROPRIGHT: u32 = 0x54DD;
pub const FLAGFORCED: u32 = 0x55AA;
pub const FLAGHEARINGIMPAIRED: u32 = 0x55AB;
pub const FLAGVISUALIMPAIRED: u32 = 0x55AC;
pub const FLAGTEXTDESCRIPTIONS: u32 = 0x55AD;
pub const FLAGORIGINAL: u32 = 0x55AE;
pub const FLAGCOMMENTARY: u32 = 0x55AF;
pub const COLOUR: u32 = 0x55B0;
pub const MATRIXCOEFFICIENTS: u32 = 0x55B1;
pub const BITSPERCHANNEL: u32 = 0x55B2;
pub const CHROMASUBSAMPLINGHORZ: u32 = 0x55B3;
pub const CHROMASUBSAMPLINGVERT: u32 = 0x55B4;
pub const CBSUBSAMPLINGHORZ: u32 = 0x55B5;
pub const CBSUBSAMPLINGVERT: u32 = 0x55B6;
pub const CHROMASITINGHORZ: u32 = 0x55B7;
pub const CHROMASITINGVERT: u32 = 0x55B8;
pub const RANGE: u32 = 0x55B9;
pub const TRANSFERCHARACTERISTICS: u32 = 0x55BA;
pub const PRIMARIES: u32 = 0x55BB;
pub const MAXCLL: u32 = 0x55BC;
pub const MAXFALL: u32 = 0x55BD;
pub const MASTERINGMETADATA: u32 = 0x55D0;
pub const PRIMARYRCHROMATICITYX: u32 = 0x55D1;
pub const PRIMARYRCHROMATICITYY: u32 = 0x55D2;
pub const PRIMARYGCHROMATICITYX: u32 = 0x55D3;
pub const PRIMARYGCHROMATICITYY: u32 = 0x55D4;
pub const PRIMARYBCHROMATICITYX: u32 = 0x55D5;
pub const PRIMARYBCHROMATICITYY: u32 = 0x55D6;
pub const WHITEPOINTCHROMATICITYX: u32 = 0x55D7;
pub const WHITEPOINTCHROMATICITYY: u32 = 0x55D8;
pub const LUMINANCEMAX: u32 = 0x55D9;
pub const LUMINANCEMIN: u32 = 0x55DA;
pub const MAXBLOCKADDITIONID: u32 = 0x55EE;
pub const CHAPTERSTRINGUID: u32 = 0x5654;
pub const CODECDELAY: u32 = 0x56AA;
pub const SEEKPREROLL: u32 = 0x56BB;
pub const WRITINGAPP: u32 = 0x5741;
pub const ATTACHEDFILE: u32 = 0x61A7;
pub const CONTENTENCODING: u32 = 0x6240;
pub const BITDEPTH: u32 = 0x6264;
pub const CODECPRIVATE: u32 = 0x63A2;
pub const TARGETS: u32 = 0x63C0;
pub const CHAPTERPHYSICALEQUIV: u32 = 0x63C3;
pub const TAGCHAPTERUID: u32 = 0x63C4;
pub const TAGTRACKUID: u32 = 0x63C5;
pub const TAGATTACHMENTUID: u32 = 0x63C6;
pub const TAGEDITIONUID: u32 = 0x63C9;
pub const TARGETTYPE: u32 = 0x63CA;
pub const TRACKTRANSLATE: u32 = 0x6624;
pub const TRACKTRANSLATETRACKID: u32 = 0x66A5;
pub const TRACKTRANSLATECODEC: u32 = 0x66BF;
pub const TRACKTRANSLATEEDITIONUID: u32 = 0x66FC;
pub const SIMPLETAG: u32 = 0x67C8;
pub const TARGETTYPEVALUE: u32 = 0x68CA;
pub const CHAPPROCESSCOMMAND: u32 = 0x6911;
pub const CHAPPROCESSTIME: u32 = 0x6922;
pub const CHAPTERTRANSLATE: u32 = 0x6924;
pub const CHAPPROCESSDATA: u32 = 0x6933;
pub const CHAPPROCESS: u32 = 0x6944;
pub const CHAPPROCESSCODECID: u32 = 0x6955;
pub const CHAPTERTRANSLATEID: u32 = 0x69A5;
pub const CHAPTERTRANSLATECODEC: u32 = 0x69BF;
pub const CHAPTERTRANSLATEEDITIONUID: u32 = 0x69FC;
pub const CONTENTENCODINGS: u32 = 0x6D80;
pub const CHAPTERSEGMENTUUID: u32 = 0x6E67;
pub const CHAPTERSEGMENTEDITIONUID: u32 = 0x6EBC;
pub const TAG: u32 = 0x7373;
pub const SEGMENTFILENAME: u32 = 0x7384;
pub const SEGMENTUUID: u32 = 0x73A4;
pub const CHAPTERUID: u32 = 0x73C4;
pub const TRACKUID: u32 = 0x73C5;
pub const ATTACHMENTLINK: u32 = 0x7446;
pub const BLOCKADDITIONS: u32 = 0x75A1;
pub const DISCARDPADDING: u32 = 0x75A2;
pub const PROJECTION: u32 = 0x7670;
pub const PROJECTIONTYPE: u32 = 0x7671;
pub const PROJECTIONPRIVATE: u32 = 0x7672;
pub const PROJECTIONPOSEYAW: u32 = 0x7673;
pub const PROJECTIONPOSEPITCH: u32 = 0x7674;
pub const PROJECTIONPOSEROLL: u32 = 0x7675;
pub const OUTPUTSAMPLINGFREQUENCY: u32 = 0x78B5;
pub const TITLE: u32 = 0x7BA9;
pub const LANGUAGE: u32 = 0x22B59C;
pub const LANGUAGEBCP47: u32 = 0x22B59D;
pub const TRACKTIMESTAMPSCALE: u32 = 0x23314F;
pub const DEFAULTDECODEDFIELDDURATION: u32 = 0x234E7A;
pub const DEFAULTDURATION: u32 = 0x23E383;
pub const CODECNAME: u32 = 0x258688;
pub const TIMESTAMPSCALE: u32 = 0x2AD7B1;
pub const UNCOMPRESSEDFOURCC: u32 = 0x2EB524;
pub const PREVFILENAME: u32 = 0x3C83AB;
pub const PREVUUID: u32 = 0x3CB923;
pub const NEXTFILENAME: u32 = 0x3E83BB;
pub const NEXTUUID: u32 = 0x3EB923;
pub const CHAPTERS: u32 = 0x1043A770;
pub const SEEKHEAD: u32 = 0x114D9B74;
pub const TAGS: u32 = 0x1254C367;
pub const INFO: u32 = 0x1549A966;
pub const TRACKS: u32 = 0x1654AE6B;
pub const SEGMENT: u32 = 0x18538067;
pub const ATTACHMENTS: u32 = 0x1941A469;
pub const EBML: u32 = 0x1A45DFA3;
pub const CUES: u32 = 0x1C53BB6B;
pub const CLUSTER: u32 = 0x1F43B675;

/// Every element the schema knows, sorted by ID so lookup is a binary search.
pub(super) static ELEMENTS: &[ElementDef] = &[
    ElementDef {
        id: 0x80,
        name: "ChapterDisplay",
        kind: ElementKind::Master,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x83,
        name: "TrackType",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x85,
        name: "ChapString",
        kind: ElementKind::Utf8,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x86,
        name: "CodecID",
        kind: ElementKind::Str,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x88,
        name: "FlagDefault",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x91,
        name: "ChapterTimeStart",
        kind: ElementKind::UInt,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x92,
        name: "ChapterTimeEnd",
        kind: ElementKind::UInt,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x96,
        name: "CueRefTime",
        kind: ElementKind::UInt,
        parent: 0xB7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x98,
        name: "ChapterFlagHidden",
        kind: ElementKind::UInt,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x9A,
        name: "FlagInterlaced",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x9B,
        name: "BlockDuration",
        kind: ElementKind::UInt,
        parent: 0xA0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x9C,
        name: "FlagLacing",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x9D,
        name: "FieldOrder",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x9F,
        name: "Channels",
        kind: ElementKind::UInt,
        parent: 0xE1,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xA0,
        name: "BlockGroup",
        kind: ElementKind::Master,
        parent: 0x1F43B675,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xA1,
        name: "Block",
        kind: ElementKind::Binary,
        parent: 0xA0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xA3,
        name: "SimpleBlock",
        kind: ElementKind::Binary,
        parent: 0x1F43B675,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xA4,
        name: "CodecState",
        kind: ElementKind::Binary,
        parent: 0xA0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xA5,
        name: "BlockAdditional",
        kind: ElementKind::Binary,
        parent: 0x75A1,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xA6,
        name: "BlockMore",
        kind: ElementKind::Master,
        parent: 0x75A1,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xA7,
        name: "Position",
        kind: ElementKind::UInt,
        parent: 0x1F43B675,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xAB,
        name: "PrevSize",
        kind: ElementKind::UInt,
        parent: 0x1F43B675,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xAE,
        name: "TrackEntry",
        kind: ElementKind::Master,
        parent: 0x1654AE6B,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xB0,
        name: "PixelWidth",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xB2,
        name: "CueDuration",
        kind: ElementKind::UInt,
        parent: 0xB7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xB3,
        name: "CueTime",
        kind: ElementKind::UInt,
        parent: 0xBB,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xB5,
        name: "SamplingFrequency",
        kind: ElementKind::Float,
        parent: 0xE1,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xB6,
        name: "ChapterAtom",
        kind: ElementKind::Master,
        parent: 0x45B9,
        recursive: true,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xB7,
        name: "CueTrackPositions",
        kind: ElementKind::Master,
        parent: 0xBB,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xB9,
        name: "FlagEnabled",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xBA,
        name: "PixelHeight",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xBB,
        name: "CuePoint",
        kind: ElementKind::Master,
        parent: 0x1C53BB6B,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xBF,
        name: "Crc32",
        kind: ElementKind::Binary,
        parent: GLOBAL,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xD7,
        name: "TrackNumber",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xDB,
        name: "CueReference",
        kind: ElementKind::Master,
        parent: 0xB7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xE0,
        name: "Video",
        kind: ElementKind::Master,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xE1,
        name: "Audio",
        kind: ElementKind::Master,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xE2,
        name: "TrackOperation",
        kind: ElementKind::Master,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xE3,
        name: "TrackCombinePlanes",
        kind: ElementKind::Master,
        parent: 0xE2,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xE4,
        name: "TrackPlane",
        kind: ElementKind::Master,
        parent: 0xE2,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xE5,
        name: "TrackPlaneUID",
        kind: ElementKind::UInt,
        parent: 0xE2,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xE6,
        name: "TrackPlaneType",
        kind: ElementKind::UInt,
        parent: 0xE2,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xE7,
        name: "Timestamp",
        kind: ElementKind::UInt,
        parent: 0x1F43B675,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xE9,
        name: "TrackJoinBlocks",
        kind: ElementKind::Master,
        parent: 0xE2,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xEA,
        name: "CueCodecState",
        kind: ElementKind::UInt,
        parent: 0xB7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xEC,
        name: "Void",
        kind: ElementKind::Binary,
        parent: GLOBAL,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xED,
        name: "TrackJoinUID",
        kind: ElementKind::UInt,
        parent: 0xE2,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xEE,
        name: "BlockAddID",
        kind: ElementKind::UInt,
        parent: 0x75A1,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xF0,
        name: "CueRelativePosition",
        kind: ElementKind::UInt,
        parent: 0xB7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xF1,
        name: "CueClusterPosition",
        kind: ElementKind::UInt,
        parent: 0xB7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xF7,
        name: "CueTrack",
        kind: ElementKind::UInt,
        parent: 0xB7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xFA,
        name: "ReferencePriority",
        kind: ElementKind::UInt,
        parent: 0xA0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0xFB,
        name: "ReferenceBlock",
        kind: ElementKind::Int,
        parent: 0xA0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x41A4,
        name: "BlockAddIDName",
        kind: ElementKind::Str,
        parent: 0x41E4,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x41E4,
        name: "BlockAdditionMapping",
        kind: ElementKind::Master,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x41E7,
        name: "BlockAddIDType",
        kind: ElementKind::UInt,
        parent: 0x41E4,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x41ED,
        name: "BlockAddIDExtraData",
        kind: ElementKind::Binary,
        parent: 0x41E4,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x41F0,
        name: "BlockAddIDValue",
        kind: ElementKind::UInt,
        parent: 0x41E4,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4254,
        name: "ContentCompAlgo",
        kind: ElementKind::UInt,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4255,
        name: "ContentCompSettings",
        kind: ElementKind::Binary,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4281,
        name: "DocTypeExtension",
        kind: ElementKind::Master,
        parent: 0x1A45DFA3,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4282,
        name: "DocType",
        kind: ElementKind::Str,
        parent: 0x1A45DFA3,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4283,
        name: "DocTypeExtensionName",
        kind: ElementKind::Str,
        parent: 0x4281,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4284,
        name: "DocTypeExtensionVersion",
        kind: ElementKind::UInt,
        parent: 0x4281,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4285,
        name: "DocTypeReadVersion",
        kind: ElementKind::UInt,
        parent: 0x1A45DFA3,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4286,
        name: "EbmlVersion",
        kind: ElementKind::UInt,
        parent: 0x1A45DFA3,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4287,
        name: "DocTypeVersion",
        kind: ElementKind::UInt,
        parent: 0x1A45DFA3,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x42F2,
        name: "EbmlMaxIdLength",
        kind: ElementKind::UInt,
        parent: 0x1A45DFA3,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x42F3,
        name: "EbmlMaxSizeLength",
        kind: ElementKind::UInt,
        parent: 0x1A45DFA3,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x42F7,
        name: "EbmlReadVersion",
        kind: ElementKind::UInt,
        parent: 0x1A45DFA3,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x437C,
        name: "ChapLanguage",
        kind: ElementKind::Str,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x437D,
        name: "ChapLanguageBCP47",
        kind: ElementKind::Str,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x437E,
        name: "ChapCountry",
        kind: ElementKind::Str,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4444,
        name: "SegmentFamily",
        kind: ElementKind::Binary,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4461,
        name: "DateUTC",
        kind: ElementKind::Date,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x447A,
        name: "TagLanguage",
        kind: ElementKind::Str,
        parent: 0x67C8,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x447B,
        name: "TagLanguageBCP47",
        kind: ElementKind::Str,
        parent: 0x67C8,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4484,
        name: "TagDefault",
        kind: ElementKind::UInt,
        parent: 0x67C8,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4485,
        name: "TagBinary",
        kind: ElementKind::Binary,
        parent: 0x67C8,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4487,
        name: "TagString",
        kind: ElementKind::Utf8,
        parent: 0x67C8,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4489,
        name: "Duration",
        kind: ElementKind::Float,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x450D,
        name: "ChapProcessPrivate",
        kind: ElementKind::Binary,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x45A3,
        name: "TagName",
        kind: ElementKind::Utf8,
        parent: 0x67C8,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x45B9,
        name: "EditionEntry",
        kind: ElementKind::Master,
        parent: 0x1043A770,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x45BC,
        name: "EditionUID",
        kind: ElementKind::UInt,
        parent: 0x45B9,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x45DB,
        name: "EditionFlagDefault",
        kind: ElementKind::UInt,
        parent: 0x45B9,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x45DD,
        name: "EditionFlagOrdered",
        kind: ElementKind::UInt,
        parent: 0x45B9,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x465C,
        name: "FileData",
        kind: ElementKind::Binary,
        parent: 0x61A7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4660,
        name: "FileMediaType",
        kind: ElementKind::Str,
        parent: 0x61A7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x466E,
        name: "FileName",
        kind: ElementKind::Utf8,
        parent: 0x61A7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x467E,
        name: "FileDescription",
        kind: ElementKind::Utf8,
        parent: 0x61A7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x46AE,
        name: "FileUID",
        kind: ElementKind::UInt,
        parent: 0x61A7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x47E1,
        name: "ContentEncAlgo",
        kind: ElementKind::UInt,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x47E2,
        name: "ContentEncKeyID",
        kind: ElementKind::Binary,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x47E7,
        name: "ContentEncAESSettings",
        kind: ElementKind::Master,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x47E8,
        name: "AESSettingsCipherMode",
        kind: ElementKind::UInt,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4D80,
        name: "MuxingApp",
        kind: ElementKind::Utf8,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x4DBB,
        name: "Seek",
        kind: ElementKind::Master,
        parent: 0x114D9B74,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x5031,
        name: "ContentEncodingOrder",
        kind: ElementKind::UInt,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x5032,
        name: "ContentEncodingScope",
        kind: ElementKind::UInt,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x5033,
        name: "ContentEncodingType",
        kind: ElementKind::UInt,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x5034,
        name: "ContentCompression",
        kind: ElementKind::Master,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x5035,
        name: "ContentEncryption",
        kind: ElementKind::Master,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x536E,
        name: "Name",
        kind: ElementKind::Utf8,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x5378,
        name: "CueBlockNumber",
        kind: ElementKind::UInt,
        parent: 0xB7,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x53AB,
        name: "SeekID",
        kind: ElementKind::Binary,
        parent: 0x4DBB,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x53AC,
        name: "SeekPosition",
        kind: ElementKind::UInt,
        parent: 0x4DBB,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x53B8,
        name: "StereoMode",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x53B9,
        name: "OldStereoMode",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x53C0,
        name: "AlphaMode",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x54AA,
        name: "PixelCropBottom",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x54B0,
        name: "DisplayWidth",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x54B2,
        name: "DisplayUnit",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x54BA,
        name: "DisplayHeight",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x54BB,
        name: "PixelCropTop",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x54CC,
        name: "PixelCropLeft",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x54DD,
        name: "PixelCropRight",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55AA,
        name: "FlagForced",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55AB,
        name: "FlagHearingImpaired",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55AC,
        name: "FlagVisualImpaired",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55AD,
        name: "FlagTextDescriptions",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55AE,
        name: "FlagOriginal",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55AF,
        name: "FlagCommentary",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B0,
        name: "Colour",
        kind: ElementKind::Master,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B1,
        name: "MatrixCoefficients",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B2,
        name: "BitsPerChannel",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B3,
        name: "ChromaSubsamplingHorz",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B4,
        name: "ChromaSubsamplingVert",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B5,
        name: "CbSubsamplingHorz",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B6,
        name: "CbSubsamplingVert",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B7,
        name: "ChromaSitingHorz",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B8,
        name: "ChromaSitingVert",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55B9,
        name: "Range",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55BA,
        name: "TransferCharacteristics",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55BB,
        name: "Primaries",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55BC,
        name: "MaxCLL",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55BD,
        name: "MaxFALL",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D0,
        name: "MasteringMetadata",
        kind: ElementKind::Master,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D1,
        name: "PrimaryRChromaticityX",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D2,
        name: "PrimaryRChromaticityY",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D3,
        name: "PrimaryGChromaticityX",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D4,
        name: "PrimaryGChromaticityY",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D5,
        name: "PrimaryBChromaticityX",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D6,
        name: "PrimaryBChromaticityY",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D7,
        name: "WhitePointChromaticityX",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D8,
        name: "WhitePointChromaticityY",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55D9,
        name: "LuminanceMax",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55DA,
        name: "LuminanceMin",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x55EE,
        name: "MaxBlockAdditionID",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x5654,
        name: "ChapterStringUID",
        kind: ElementKind::Utf8,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x56AA,
        name: "CodecDelay",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x56BB,
        name: "SeekPreRoll",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x5741,
        name: "WritingApp",
        kind: ElementKind::Utf8,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x61A7,
        name: "AttachedFile",
        kind: ElementKind::Master,
        parent: 0x1941A469,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6240,
        name: "ContentEncoding",
        kind: ElementKind::Master,
        parent: 0x6D80,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6264,
        name: "BitDepth",
        kind: ElementKind::UInt,
        parent: 0xE1,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x63A2,
        name: "CodecPrivate",
        kind: ElementKind::Binary,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x63C0,
        name: "Targets",
        kind: ElementKind::Master,
        parent: 0x7373,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x63C3,
        name: "ChapterPhysicalEquiv",
        kind: ElementKind::UInt,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x63C4,
        name: "TagChapterUID",
        kind: ElementKind::UInt,
        parent: 0x63C0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x63C5,
        name: "TagTrackUID",
        kind: ElementKind::UInt,
        parent: 0x63C0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x63C6,
        name: "TagAttachmentUID",
        kind: ElementKind::UInt,
        parent: 0x63C0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x63C9,
        name: "TagEditionUID",
        kind: ElementKind::UInt,
        parent: 0x63C0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x63CA,
        name: "TargetType",
        kind: ElementKind::Str,
        parent: 0x63C0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6624,
        name: "TrackTranslate",
        kind: ElementKind::Master,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x66A5,
        name: "TrackTranslateTrackID",
        kind: ElementKind::Binary,
        parent: 0x6624,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x66BF,
        name: "TrackTranslateCodec",
        kind: ElementKind::UInt,
        parent: 0x6624,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x66FC,
        name: "TrackTranslateEditionUID",
        kind: ElementKind::UInt,
        parent: 0x6624,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x67C8,
        name: "SimpleTag",
        kind: ElementKind::Master,
        parent: 0x7373,
        recursive: true,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x68CA,
        name: "TargetTypeValue",
        kind: ElementKind::UInt,
        parent: 0x63C0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6911,
        name: "ChapProcessCommand",
        kind: ElementKind::Master,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6922,
        name: "ChapProcessTime",
        kind: ElementKind::UInt,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6924,
        name: "ChapterTranslate",
        kind: ElementKind::Master,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6933,
        name: "ChapProcessData",
        kind: ElementKind::Binary,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6944,
        name: "ChapProcess",
        kind: ElementKind::Master,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6955,
        name: "ChapProcessCodecID",
        kind: ElementKind::UInt,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x69A5,
        name: "ChapterTranslateID",
        kind: ElementKind::Binary,
        parent: 0x6924,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x69BF,
        name: "ChapterTranslateCodec",
        kind: ElementKind::UInt,
        parent: 0x6924,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x69FC,
        name: "ChapterTranslateEditionUID",
        kind: ElementKind::UInt,
        parent: 0x6924,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6D80,
        name: "ContentEncodings",
        kind: ElementKind::Master,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6E67,
        name: "ChapterSegmentUUID",
        kind: ElementKind::Binary,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x6EBC,
        name: "ChapterSegmentEditionUID",
        kind: ElementKind::UInt,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7373,
        name: "Tag",
        kind: ElementKind::Master,
        parent: 0x1254C367,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7384,
        name: "SegmentFilename",
        kind: ElementKind::Utf8,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x73A4,
        name: "SegmentUUID",
        kind: ElementKind::Binary,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x73C4,
        name: "ChapterUID",
        kind: ElementKind::UInt,
        parent: 0xB6,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x73C5,
        name: "TrackUID",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7446,
        name: "AttachmentLink",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x75A1,
        name: "BlockAdditions",
        kind: ElementKind::Master,
        parent: 0xA0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x75A2,
        name: "DiscardPadding",
        kind: ElementKind::Int,
        parent: 0xA0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7670,
        name: "Projection",
        kind: ElementKind::Master,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7671,
        name: "ProjectionType",
        kind: ElementKind::UInt,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7672,
        name: "ProjectionPrivate",
        kind: ElementKind::Binary,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7673,
        name: "ProjectionPoseYaw",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7674,
        name: "ProjectionPosePitch",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7675,
        name: "ProjectionPoseRoll",
        kind: ElementKind::Float,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x78B5,
        name: "OutputSamplingFrequency",
        kind: ElementKind::Float,
        parent: 0xE1,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x7BA9,
        name: "Title",
        kind: ElementKind::Utf8,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x22B59C,
        name: "Language",
        kind: ElementKind::Str,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x22B59D,
        name: "LanguageBCP47",
        kind: ElementKind::Str,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x23314F,
        name: "TrackTimestampScale",
        kind: ElementKind::Float,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x234E7A,
        name: "DefaultDecodedFieldDuration",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x23E383,
        name: "DefaultDuration",
        kind: ElementKind::UInt,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x258688,
        name: "CodecName",
        kind: ElementKind::Utf8,
        parent: 0xAE,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x2AD7B1,
        name: "TimestampScale",
        kind: ElementKind::UInt,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x2EB524,
        name: "UncompressedFourCC",
        kind: ElementKind::Binary,
        parent: 0xE0,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x3C83AB,
        name: "PrevFilename",
        kind: ElementKind::Utf8,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x3CB923,
        name: "PrevUUID",
        kind: ElementKind::Binary,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x3E83BB,
        name: "NextFilename",
        kind: ElementKind::Utf8,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x3EB923,
        name: "NextUUID",
        kind: ElementKind::Binary,
        parent: 0x1549A966,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x1043A770,
        name: "Chapters",
        kind: ElementKind::Master,
        parent: 0x18538067,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x114D9B74,
        name: "SeekHead",
        kind: ElementKind::Master,
        parent: 0x18538067,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x1254C367,
        name: "Tags",
        kind: ElementKind::Master,
        parent: 0x18538067,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x1549A966,
        name: "Info",
        kind: ElementKind::Master,
        parent: 0x18538067,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x1654AE6B,
        name: "Tracks",
        kind: ElementKind::Master,
        parent: 0x18538067,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x18538067,
        name: "Segment",
        kind: ElementKind::Master,
        parent: ROOT,
        recursive: false,
        unknown_size_ok: true,
    },
    ElementDef {
        id: 0x1941A469,
        name: "Attachments",
        kind: ElementKind::Master,
        parent: 0x18538067,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x1A45DFA3,
        name: "Ebml",
        kind: ElementKind::Master,
        parent: ROOT,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x1C53BB6B,
        name: "Cues",
        kind: ElementKind::Master,
        parent: 0x18538067,
        recursive: false,
        unknown_size_ok: false,
    },
    ElementDef {
        id: 0x1F43B675,
        name: "Cluster",
        kind: ElementKind::Master,
        parent: 0x18538067,
        recursive: false,
        unknown_size_ok: true,
    },
];
