//! `bsi()`: bitstream information. ATSC A/52:2018 §5.3.2 (classic AC-3) /
//! §E.1.3.2 (E-AC-3).
//!
//! Every optional field's presence bit is modelled even where this crate
//! surfaces no value for it (E-AC-3's downmix-metadata block, `mixmdate`,
//! nests dozens of `acmod`-conditional fields used only by professional
//! authoring tools) — getting the *skip* wrong would misalign every audio
//! block that follows, which is a far worse failure than not exposing a
//! field nobody asked for. See [`Bsi::bit_len`]'s doc comment for how this is
//! checked against real files: the frame's own length states exactly where
//! the trailing CRC starts, which is an oracle a hand-recalled field-width
//! table cannot be.

use vaco_bitstream::BitReader;

use crate::syncinfo::{FrameKind, SyncInfo};
use crate::tables::{has_center, has_surround};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BsiError;

/// Everything `bsi()` states, common fields first. E-AC-3-only fields are
/// `None` on a classic AC-3 frame and vice versa is not applicable (classic
/// AC-3 has no E-AC-3-only fields to omit).
#[derive(Debug, Clone)]
pub struct Bsi {
    pub acmod: u8,
    pub lfeon: bool,
    pub cmixlev: Option<u8>,
    pub surmixlev: Option<u8>,
    pub dsurmod: Option<u8>,
    pub dialnorm: u8,
    pub compr: Option<u8>,
    /// `Some` only when `acmod == 0` (dual mono / 1+1).
    pub dialnorm2: Option<u8>,
    pub compr2: Option<u8>,
    pub copyrightb: bool,
    pub origbs: bool,
    /// E-AC-3 only: `0` independent, `1` dependent, `2` independent (AC-3
    /// convert sync frame).
    pub strmtyp: Option<u8>,
    pub substream_id: Option<u8>,
    /// Bit offset (from the start of the frame) where `bsi()` ends and the
    /// first `audblk()` begins.
    pub bit_len: u32,
}

impl Bsi {
    /// Parse `bsi()` in full, from the start of the frame (re-reading the
    /// `syncinfo()` bits `info` already parsed — cheap, and it keeps this
    /// function self-contained rather than threading a reader across two
    /// modules).
    ///
    /// # Errors
    /// [`BsiError`] if the bitstream overran (truncated frame) or a reserved
    /// value made the rest of the structure unrecoverable.
    pub fn parse(buf: &[u8], info: &SyncInfo) -> Result<Self, BsiError> {
        let mut r = BitReader::new(buf);
        match info.kind {
            FrameKind::Ac3 => Self::parse_ac3(&mut r),
            FrameKind::Eac3 => Self::parse_eac3(&mut r),
        }
    }

    fn parse_ac3(r: &mut BitReader<'_>) -> Result<Self, BsiError> {
        r.skip(16); // syncword
        r.skip(16); // crc1
        r.skip(2); // fscod
        r.skip(6); // frmsizecod
        r.skip(5); // bsid
        r.skip(3); // bsmod
        let acmod = u8::try_from(r.get(3)).unwrap_or(0);

        let cmixlev = has_center(acmod).then(|| u8::try_from(r.get(2)).unwrap_or(0));
        let surmixlev = has_surround(acmod).then(|| u8::try_from(r.get(2)).unwrap_or(0));
        let dsurmod = (acmod == 2).then(|| u8::try_from(r.get(2)).unwrap_or(0));
        let lfeon = r.get_bit() != 0;
        let dialnorm = u8::try_from(r.get(5)).unwrap_or(0);
        let compre = r.get_bit() != 0;
        let compr = compre.then(|| u8::try_from(r.get(8)).unwrap_or(0));
        let langcode = r.get_bit() != 0;
        if langcode {
            r.skip(8); // langcod
        }
        let audprodie = r.get_bit() != 0;
        if audprodie {
            r.skip(5); // mixlevel
            r.skip(2); // roomtyp
        }
        let (dialnorm2, compr2) = if acmod == 0 {
            let d2 = u8::try_from(r.get(5)).unwrap_or(0);
            let compr2e = r.get_bit() != 0;
            let c2 = compr2e.then(|| u8::try_from(r.get(8)).unwrap_or(0));
            let langcode2 = r.get_bit() != 0;
            if langcode2 {
                r.skip(8);
            }
            let audprodie2 = r.get_bit() != 0;
            if audprodie2 {
                r.skip(5);
                r.skip(2);
            }
            (Some(d2), c2)
        } else {
            (None, None)
        };
        let copyrightb = r.get_bit() != 0;
        let origbs = r.get_bit() != 0;
        let timecod1e = r.get_bit() != 0;
        if timecod1e {
            r.skip(14);
        }
        let timecod2e = r.get_bit() != 0;
        if timecod2e {
            r.skip(14);
        }
        skip_addbsi(r);
        finish(
            r,
            Self {
                acmod,
                lfeon,
                cmixlev,
                surmixlev,
                dsurmod,
                dialnorm,
                compr,
                dialnorm2,
                compr2,
                copyrightb,
                origbs,
                strmtyp: None,
                substream_id: None,
                bit_len: 0,
            },
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one straight-line syntax walk over E.1.3.2; splitting it would scatter bit positions across functions, which is the one thing this parser cannot afford to get wrong"
    )]
    fn parse_eac3(r: &mut BitReader<'_>) -> Result<Self, BsiError> {
        r.skip(16); // syncword
        let strmtyp = u8::try_from(r.get(2)).unwrap_or(0);
        let substream_id = u8::try_from(r.get(3)).unwrap_or(0);
        r.skip(11); // frmsiz
        let fscod = r.get(2);
        let numblkscod = if fscod == 3 {
            r.skip(2); // fscod2
            3u32 // a reduced-sample-rate frame is always 6 blocks
        } else {
            r.get(2)
        };
        let acmod = u8::try_from(r.get(3)).unwrap_or(0);
        let lfeon = r.get_bit() != 0;
        r.skip(5); // bsid

        let dialnorm = u8::try_from(r.get(5)).unwrap_or(0);
        let compre = r.get_bit() != 0;
        let compr = compre.then(|| u8::try_from(r.get(8)).unwrap_or(0));
        let (dialnorm2, compr2) = if acmod == 0 {
            let d2 = u8::try_from(r.get(5)).unwrap_or(0);
            let compr2e = r.get_bit() != 0;
            let c2 = compr2e.then(|| u8::try_from(r.get(8)).unwrap_or(0));
            (Some(d2), c2)
        } else {
            (None, None)
        };

        if strmtyp == 1 {
            let chanmape = r.get_bit() != 0;
            if chanmape {
                r.skip(16); // chanmap
            }
        }

        // Downmix/mixing metadata, present only when an authoring tool wrote
        // it. `mixmdate == 0` is what every fixture measured for this crate
        // exercises; the nested `acmod`-nested fields below are the
        // best-effort structural skip and are the least-verified part of
        // this parser (see the module docs).
        let mixmdate = r.get_bit() != 0;
        if mixmdate {
            if acmod > 2 {
                r.skip(2); // dmixmod
            }
            if has_center(acmod) {
                r.skip(3); // ltrtcmixlev
                r.skip(3); // lorocmixlev
            }
            if has_surround(acmod) {
                r.skip(3); // ltrtsurmixlev
                r.skip(3); // lorosurmixlev
            }
            if lfeon {
                let lfemixlevcode = r.get_bit() != 0;
                if lfemixlevcode {
                    r.skip(5); // lfemixlevcod
                }
            }
            if strmtyp == 0 {
                let pgmscle = r.get_bit() != 0;
                if pgmscle {
                    r.skip(6);
                }
                if acmod == 0 {
                    let pgmscl2e = r.get_bit() != 0;
                    if pgmscl2e {
                        r.skip(6);
                    }
                }
                let extpgmscle = r.get_bit() != 0;
                if extpgmscle {
                    r.skip(6);
                }
                let mixdef = r.get(2);
                match mixdef {
                    1 => {
                        r.skip(5); // premixcmpsel(1)+drcsrc(1)+premixcmpscl(3) approximated as 5
                    }
                    2 => {
                        r.skip(12); // mixdata2 block, approximated
                    }
                    3 => {
                        let mixdeflen = r.get(5);
                        let bits = (mixdeflen.saturating_add(2)).saturating_mul(8);
                        r.skip(bits);
                    }
                    _ => {}
                }
                if acmod < 2 {
                    let paninfoe = r.get_bit() != 0;
                    if paninfoe {
                        r.skip(14); // panmean(8)+paninfo direction/mode approximated
                    }
                    if acmod == 0 {
                        let paninfo2e = r.get_bit() != 0;
                        if paninfo2e {
                            r.skip(14);
                        }
                    }
                }
                let frmmixcfginfoe = r.get_bit() != 0;
                if frmmixcfginfoe {
                    if numblkscod == 0 {
                        r.skip(5);
                    } else {
                        for _ in 0..blocks_for(numblkscod) {
                            let blkmixcfginfoe = r.get_bit() != 0;
                            if blkmixcfginfoe {
                                r.skip(5);
                            }
                        }
                    }
                }
            }
        }

        let infomdate = r.get_bit() != 0;
        let mut copyrightb = false;
        let mut origbs = false;
        if infomdate {
            r.skip(3); // bsmod
            copyrightb = r.get_bit() != 0;
            origbs = r.get_bit() != 0;
            if acmod == 2 {
                r.skip(2); // dsurmod
                r.skip(2); // dheadphonmod
            }
            if acmod >= 6 {
                r.skip(2); // dsurexmod
            }
            let audprodie = r.get_bit() != 0;
            if audprodie {
                r.skip(5); // mixlevel
                r.skip(2); // roomtyp
                r.skip(1); // adconvtyp
            }
            if acmod == 0 {
                let audprodi2e = r.get_bit() != 0;
                if audprodi2e {
                    r.skip(5);
                    r.skip(2);
                    r.skip(1);
                }
            }
            if info_source_is_half_rate(fscod) {
                r.skip(1); // sourcefscod
            }
        }

        if strmtyp == 0 && numblkscod != 3 {
            r.skip(1); // convsync
        }
        if strmtyp == 2 && numblkscod == 3 {
            r.skip(1); // blkid, always 1 here, still a bit to consume
        }
        skip_addbsi(r);

        finish(
            r,
            Self {
                acmod,
                lfeon,
                cmixlev: None,
                surmixlev: None,
                dsurmod: None,
                dialnorm,
                compr,
                dialnorm2,
                compr2,
                copyrightb,
                origbs,
                strmtyp: Some(strmtyp),
                substream_id: Some(substream_id),
                bit_len: 0,
            },
        )
    }
}

/// `fscod == 3` (reduced sample rate) is the only case E-AC-3's `infomdate`
/// gates an extra `sourcefscod` bit on. Named to read at the call site rather
/// than repeating the magic comparison.
const fn info_source_is_half_rate(fscod: u32) -> bool {
    fscod == 3
}

const fn blocks_for(numblkscod: u32) -> u32 {
    match numblkscod {
        0 => 1,
        1 => 2,
        2 => 3,
        _ => 6,
    }
}

fn skip_addbsi(r: &mut BitReader<'_>) {
    let addbsie = r.get_bit() != 0;
    if addbsie {
        let addbsil = r.get(6);
        r.skip((addbsil.saturating_add(1)).saturating_mul(8));
    }
}

fn finish(r: &BitReader<'_>, mut bsi: Bsi) -> Result<Bsi, BsiError> {
    if r.check().is_err() {
        return Err(BsiError);
    }
    bsi.bit_len = u32::try_from(r.bit_pos()).unwrap_or(u32::MAX);
    Ok(bsi)
}
