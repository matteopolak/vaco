//! H.263+'s extended picture header: `PLUSPTYPE` and the optional fields
//! that follow it (§5.1.4–§5.1.10).
//!
//! `Vaco-Spec-Ref: itu-t-h263` (01/2005 edition — the free base text this
//! crate's baseline decoder cites is the 03/96 edition, which predates
//! every annex this module reads; the 01/2005 edition is the same freely
//! published ITU-T recommendation, its current in-force revision,
//! cumulative over the 1998/2000/2001 amendments that added Annexes
//! I through X).
//!
//! # Scope
//!
//! Enough of the extended header to support Annexes D (Unrestricted
//! Motion Vector), K (Slice Structured) and T (Modified Quantization) —
//! see this crate's own top-level docs for which annexes landed and
//! which did not. A picture whose header sets any mode bit this crate
//! does not implement (SAC, Advanced Prediction, Advanced INTRA,
//! Deblocking Filter, Reference Picture Selection, Independent Segment
//! Decoding, Alternative INTER VLC, Reference Picture Resampling,
//! Reduced-Resolution Update) or whose `MPPTYPE` picture-type code names
//! Improved PB/B/EI/EP (Annexes M/O) is reported as unsupported by
//! [`parse`] rather than guessed at — this module does not know how to
//! read what follows those bits (`ELNUM`/`RLNUM`/`RPSMF`/`TRPI`/`TRP`/
//! `BCI`/`BCM`/`RPRP` and the rest are all scoped to modes this crate
//! does not support). Advanced Prediction is parsed as a mode bit (it
//! shares `OPPTYPE` with every other mode) but never *acted* on — this
//! crate stops one bit short of using it, at the same bail as SAC/AIC/DF.

use vaco_bitstream::BitReader;

/// Which optional H.263+ modes are active for the current and following
/// pictures. §5.1.4.5's own "mode inference rules": once a mode bit is
/// set to `1` in a picture whose `OPPTYPE` is present (`UFEP == 1`), it
/// stays "on" for every following picture until a picture with `OPPTYPE`
/// present explicitly overrides it, or a picture without `PLUSPTYPE` at
/// all resets every mode to "off". This struct is that persisted state,
/// owned by the decoder across pictures — see `h263::H263Decoder`'s own
/// field of this type.
#[derive(Debug, Clone, Copy, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per independent OPPTYPE/SSS mode bit (`Vaco-Spec-Ref: itu-t-h263` §5.1.4.2/§5.1.10) — the spec itself defines these as independent yes/no switches, so collapsing them into an enum would just re-encode the same bits behind a match"
)]
pub(crate) struct PlusModes {
    pub umv: bool,
    pub sac: bool,
    pub advanced_prediction: bool,
    pub advanced_intra: bool,
    pub deblocking_filter: bool,
    pub slice_structured: bool,
    pub reference_picture_selection: bool,
    pub independent_segment_decoding: bool,
    pub alternative_inter_vlc: bool,
    pub modified_quantization: bool,
    /// §5.1.10: rectangular-slice / arbitrary-slice-ordering submode bits,
    /// meaningful only when `slice_structured` is set. Persists the same
    /// way the mode bits above do (§5.1.10's own text: "If the Slice
    /// Structured mode is in use but UFEP is not 001, the last values
    /// sent for SSS shall remain in effect").
    pub rectangular_slices: bool,
    pub arbitrary_slice_order: bool,
}

/// One picture's fully-parsed extended header, or the reason this crate
/// can't decode it.
pub(crate) struct PlusHeader {
    pub width: u32,
    pub height: u32,
    pub intra: bool,
    pub cpm: bool,
    /// Rounding Type (`RTYPE`), MPPTYPE bit 6 (`Vaco-Spec-Ref: itu-t-h263`
    /// 6.1.2): selects which of the two half-pel interpolation rounding
    /// conventions (`RCONTROL`) this picture's own motion compensation
    /// uses. Always `false` outside `PLUSPTYPE` (§6.1.2's own text: "RCONTROL
    /// has an implied value of 0" when the extended header is absent).
    pub rtype: bool,
}

/// §5.1.4–§5.1.10: parse `PLUSPTYPE` and its optional trailing fields,
/// starting right after the 8-bit `PTYPE` that signalled its presence
/// (bits 6-8 of `PTYPE` equal to `"111"`). `modes` is updated in place
/// per the mode-inference rules; `fallback_dims` supplies width/height
/// for a `UFEP == 0` picture, which does not resend the source format.
///
/// Returns `None` if this picture uses a mode this crate does not
/// implement, or an unrecognised/custom-without-`CPFMT` source format —
/// the caller marks the whole picture unsupported rather than guessing
/// at unread syntax.
#[allow(
    clippy::too_many_lines,
    reason = "one linear read of one bitstream structure (PLUSPTYPE plus every field Figure 8 lists after it); splitting it up would just thread the same BitReader and PlusModes through several functions that are each called exactly once, from here"
)]
pub(crate) fn parse(
    r: &mut BitReader<'_>,
    modes: &mut PlusModes,
    fallback_dims: Option<(u32, u32)>,
) -> Option<PlusHeader> {
    let ufep = r.get(3);
    if ufep != 0 && ufep != 1 {
        return None; // reserved UFEP value.
    }
    let full_update = ufep == 1;

    let mut width = fallback_dims.map_or(0, |(w, _)| w);
    let mut height = fallback_dims.map_or(0, |(_, h)| h);
    let mut has_custom_format = false;
    let mut has_custom_pcf = false;

    // §5.1.4.2: OPPTYPE, only when UFEP == "001".
    if full_update {
        let opptype = r.get(18);
        let bit = |n: u32| (opptype >> (18 - n)) & 1 == 1;
        let source_format = (opptype >> 15) & 0b111;
        has_custom_format = source_format == 0b110;
        has_custom_pcf = bit(4);
        modes.umv = bit(5);
        modes.sac = bit(6);
        modes.advanced_prediction = bit(7);
        modes.advanced_intra = bit(8);
        modes.deblocking_filter = bit(9);
        modes.slice_structured = bit(10);
        modes.reference_picture_selection = bit(11);
        modes.independent_segment_decoding = bit(12);
        modes.alternative_inter_vlc = bit(13);
        modes.modified_quantization = bit(14);

        if !has_custom_format {
            let (w, h) = crate::h263::source_format_dims(source_format)?;
            width = w;
            height = h;
        }
        // A custom source format's own dimensions come from CPFMT below;
        // `width`/`height` are left at 0 until then.
    }
    // else: UFEP == 0 — every mode bit keeps its persisted value from the
    // last picture whose OPPTYPE was present, per §5.1.4.4; nothing to
    // update in `modes` itself.

    // §5.1.4.3: MPPTYPE, always present once PLUSPTYPE is — this
    // completes "PLUSPTYPE" proper; every field below is one of Figure
    // 8's *own* fields, which Figure 8's caption places after it (and,
    // per §5.1.4.7, after CPM/PSBI too).
    let mpptype = r.get(9);
    let picture_type_code = (mpptype >> 6) & 0b111;
    let rpr = (mpptype >> 5) & 1 == 1;
    let rru = (mpptype >> 4) & 1 == 1;
    // §6.1.2/§5.1.4.3: bit 6 of MPPTYPE, only meaningful for P/Improved-PB/
    // EP pictures (always 0 otherwise, per the encoder-side restriction
    // this decoder does not need to itself enforce).
    let rtype = (mpptype >> 3) & 1 == 1;

    // §5.1.4.7: CPM follows PLUSPTYPE directly here (not after PQUANT,
    // as in the non-extended header) — the caller reads PQUANT itself,
    // once parsing this whole structure succeeds.
    let cpm = r.get_bit() == 1;
    if cpm {
        r.skip(2); // PSBI, not used by this crate.
    }

    if has_custom_format {
        // §5.1.5: CPFMT (23 bits), only when UFEP == 1 and the source
        // format indicated a custom picture.
        let cpfmt = r.get(23);
        let par_code = (cpfmt >> 19) & 0b1111;
        let pwi = (cpfmt >> 10) & 0x1FF; // bits 5-13, 9 bits
        let phi = cpfmt & 0x1FF; // bits 15-23, 9 bits
        width = (pwi + 1) * 4;
        height = phi * 4;
        if par_code == 0b1111 {
            r.skip(16); // §5.1.6: EPAR, not used by this crate beyond staying in sync.
        }
    }

    if full_update && has_custom_pcf {
        r.skip(8); // §5.1.7: CPCFC, not used by this crate beyond staying in sync.
    }
    if has_custom_pcf {
        r.skip(2); // §5.1.8: ETR, present regardless of UFEP once a custom PCF is in use.
    }

    if full_update && modes.umv {
        // §5.1.9: UUI, "1" (limited, Tables D.1/D.2) or "01" (unlimited
        // except by picture-border distance). Both decode identically
        // (see `motion::h263_umv_vector_plus`'s own docs: Table D.3 is
        // unambiguous, so the range only bounds what a conforming
        // encoder sends, never how the decoder reconstructs it) — this
        // read only needs to consume the right number of bits to stay
        // aligned with whichever codeword follows.
        if r.get_bit() == 0 && r.get_bit() == 0 {
            return None; // "00" is not a valid UUI codeword.
        }
    }

    if full_update && modes.slice_structured {
        // §5.1.10: SSS (2 bits).
        let sss = r.get(2);
        modes.rectangular_slices = (sss >> 1) & 1 == 1;
        modes.arbitrary_slice_order = sss & 1 == 1;
    }

    // Every other optional field in Figure 8 (ELNUM, RLNUM, RPSMF, TRPI,
    // TRP, BCI, BCM, RPRP) is scoped to Reference Picture Selection,
    // Temporal/SNR/Spatial Scalability, or Reference Picture Resampling
    // — all out of this crate's scope. None of them can be reached
    // without one of the mode bits this function already bails on below,
    // so checking here (after the fields above, all of which any
    // supported picture actually needs) is enough to avoid misreading
    // them.
    if modes.sac
        || modes.advanced_intra
        || modes.reference_picture_selection
        || modes.independent_segment_decoding
        || modes.alternative_inter_vlc
        || rpr
        || rru
        // Annex F §F.3's remote-vector rules have their own, different
        // cross-segment substitution when Slice Structured or
        // Independent Segment Decoding is also active ("remote motion
        // vectors... are set to the motion vector of the current block,
        // regardless of the other conditions") — not implemented, and
        // ISD is already out of scope above, so only the Annex F +
        // Annex K combination needs an explicit bail here rather than
        // silently mispredicting at slice boundaries. This is also what
        // keeps the one-macroblock reconstruction lookahead confined to
        // decode_gob's plain raster order: decode_slice_rect/
        // decode_first_slice/decode_slice never run with
        // advanced_prediction set.
        || (modes.advanced_prediction && modes.slice_structured)
    {
        return None;
    }
    let intra = match picture_type_code {
        0 => true,        // I-picture.
        1 => false,       // P-picture.
        _ => return None, // Improved PB / B / EI / EP: Annexes M/O, out of scope.
    };

    if width == 0 || height == 0 {
        return None;
    }
    Some(PlusHeader {
        width,
        height,
        intra,
        cpm,
        rtype,
    })
}
