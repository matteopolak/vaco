//! Hand-written tests.
//!
//! The generated table carries its own `generated_invariants` module (see
//! `table.rs`), which asserts the structural properties over every entry. What
//! is here is the part a generator cannot check itself: that names round-trip,
//! that the plane arithmetic agrees with values computed by hand, and that no
//! reachable `(format, width, height, align)` produces a nonsensical layout.
#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a test that indexes out of range or unwraps a None is a failing \
              test, which is the correct outcome; the lints exist to stop \
              library code panicking on hostile input"
)]

use super::{PixFmt, PixFmtFlags as F};
use proptest::prelude::*;

#[test]
fn every_name_round_trips() {
    for &fmt in PixFmt::all() {
        let name = fmt.name();
        assert_eq!(
            PixFmt::from_name(name).ok(),
            Some(fmt),
            "`{name}` did not round-trip"
        );
    }
}

#[test]
fn aliases_resolve() {
    for (alias, expected) in [
        ("gray8", "gray"),
        ("y400a", "ya8"),
        ("gray8a", "ya8"),
        ("gbr24p", "gbrp"),
    ] {
        let fmt = PixFmt::from_name(alias).expect("alias should resolve");
        assert_eq!(fmt.name(), expected);
    }
}

#[test]
fn unsuffixed_names_take_the_host_endianness() {
    let native = if cfg!(target_endian = "big") {
        "gray16be"
    } else {
        "gray16le"
    };
    assert_eq!(
        PixFmt::from_name("gray16").expect("should resolve").name(),
        native
    );
    assert_eq!(
        PixFmt::from_name("rgb48").expect("should resolve").name(),
        if cfg!(target_endian = "big") {
            "rgb48be"
        } else {
            "rgb48le"
        }
    );
}

#[test]
fn unknown_names_are_rejected_not_guessed() {
    for bad in ["", "yuv420", "not_a_format", "le", "yuv420pxx", "GRAY"] {
        assert!(
            PixFmt::from_name(bad).is_err(),
            "`{bad}` should not have parsed"
        );
    }
}

#[test]
fn waves_one_to_three_formats_are_present() {
    // The formats the first decoders, the scaler and ffprobe actually need. If
    // a family declaration is broken this is the test that says so first.
    for name in [
        "yuv420p",
        "yuv422p",
        "yuv444p",
        "yuvj420p",
        "yuv420p10le",
        "yuv422p10le",
        "yuv420p10be",
        "yuv444p12le",
        "nv12",
        "nv21",
        "p010le",
        "gray",
        "gray10le",
        "rgb24",
        "bgr24",
        "rgba",
        "bgra",
        "argb",
        "abgr",
        "rgb0",
        "0rgb",
        "pal8",
        "gbrp",
        "gbrap",
        "yuva420p",
        "monow",
        "monob",
        "yuyv422",
        "uyvy422",
    ] {
        assert!(
            PixFmt::from_name(name).is_ok(),
            "wave 1-3 needs `{name}` and it is missing"
        );
    }
}

// ---------------------------------------------------------------- descriptors

#[test]
fn planar_yuv_shape_is_what_it_claims() {
    let d = PixFmt::from_name("yuv420p").expect("present").descriptor();
    assert_eq!(d.planes, 3);
    assert_eq!((d.log2_chroma_w, d.log2_chroma_h), (1, 1));
    assert_eq!(d.bits_per_pixel, 12);
    assert!(d.flags.contains(F::PLANAR));
    assert!(!d.flags.contains(F::RGB));
    for (i, c) in d.components.iter().enumerate() {
        assert_eq!(c.plane, i as u8);
        assert_eq!((c.step, c.offset, c.shift, c.depth), (1, 0, 0, 8));
    }
}

#[test]
fn nv12_interleaves_chroma_and_nv21_swaps_it() {
    let nv12 = PixFmt::from_name("nv12").expect("present").descriptor();
    assert_eq!(nv12.planes, 2);
    assert_eq!(nv12.bits_per_pixel, 12);
    assert_eq!(nv12.components[1].plane, 1);
    assert_eq!(nv12.components[1].offset, 0); // Cb first
    assert_eq!(nv12.components[2].offset, 1); // then Cr
    assert_eq!(nv12.components[1].step, 2);

    let nv21 = PixFmt::from_name("nv21").expect("present").descriptor();
    assert_eq!(nv21.components[1].offset, 1);
    assert_eq!(nv21.components[2].offset, 0);
}

#[test]
fn p010_is_left_aligned_in_sixteen_bits() {
    let d = PixFmt::from_name("p010le").expect("present").descriptor();
    assert_eq!(d.planes, 2);
    assert_eq!(d.bits_per_pixel, 15);
    for c in d.components {
        assert_eq!(c.depth, 10);
        assert_eq!(
            c.shift, 6,
            "10 significant bits at the top of a 16-bit word"
        );
    }
}

#[test]
fn gbr_planes_are_stored_g_b_r() {
    let d = PixFmt::from_name("gbrp").expect("present").descriptor();
    assert_eq!(d.components[0].plane, 2, "R is the third plane");
    assert_eq!(d.components[1].plane, 0, "G is the first plane");
    assert_eq!(d.components[2].plane, 1, "B is the second plane");
    assert!(d.flags.contains(F::RGB));
    assert!(d.flags.contains(F::PLANAR));
}

#[test]
fn byte_order_only_changes_the_flag() {
    for &fmt in PixFmt::all() {
        let Some(other) = fmt.swap_endianness() else {
            continue;
        };
        assert_eq!(other.swap_endianness(), Some(fmt));
        assert_ne!(fmt.is_big_endian(), other.is_big_endian());
        assert_eq!(fmt.descriptor().components, other.descriptor().components);
        assert_eq!(fmt.bits_per_pixel(), other.bits_per_pixel());
    }
    let le = PixFmt::from_name("yuv420p10le").expect("present");
    assert_eq!(le.swap_endianness().map(PixFmt::name), Some("yuv420p10be"));
}

#[test]
fn packed_rgb_bitfields_sit_where_the_name_says() {
    let d = PixFmt::from_name("rgb565le").expect("present").descriptor();
    assert_eq!(
        d.components[0],
        super::Component {
            plane: 0,
            step: 2,
            offset: 0,
            shift: 11,
            depth: 5
        }
    );
    assert_eq!(
        d.components[1],
        super::Component {
            plane: 0,
            step: 2,
            offset: 0,
            shift: 5,
            depth: 6
        }
    );
    assert_eq!(
        d.components[2],
        super::Component {
            plane: 0,
            step: 2,
            offset: 0,
            shift: 0,
            depth: 5
        }
    );
    assert_eq!(d.bits_per_pixel, 16);

    // bgr565 is the same container with the channels exchanged.
    let b = PixFmt::from_name("bgr565le").expect("present").descriptor();
    assert_eq!(b.components[0].shift, 0);
    assert_eq!(b.components[2].shift, 11);
}

#[test]
fn padding_channels_are_not_components() {
    for name in ["0rgb", "rgb0", "vuyx", "x2rgb10le"] {
        let fmt = PixFmt::from_name(name).expect("present");
        assert_eq!(fmt.component_count(), 3, "{name}");
        assert!(!fmt.has_alpha(), "{name}");
    }
    assert_eq!(
        PixFmt::from_name("0rgb").expect("present").bits_per_pixel(),
        24,
        "bits_per_pixel excludes padding"
    );
}

#[test]
fn hardware_surfaces_carry_no_layout() {
    for name in ["videotoolbox", "cuda", "vulkan", "vaapi"] {
        let fmt = PixFmt::from_name(name).expect("present");
        assert!(fmt.is_hw());
        assert_eq!(fmt.plane_count(), 0);
        assert_eq!(fmt.component_count(), 0);
        assert_eq!(fmt.bits_per_pixel(), 0);
        assert_eq!(fmt.plane_size(0, 1080, 1920), 0);
    }
}

// ------------------------------------------------------------------- geometry

#[test]
fn plane_size_applies_vertical_decimation() {
    let yuv420p = PixFmt::from_name("yuv420p").expect("present");
    // 1920x1080, stride 1920: luma is the full height, chroma is half.
    assert_eq!(yuv420p.plane_size(0, 1080, 1920), 1920 * 1080);
    assert_eq!(yuv420p.plane_size(1, 1080, 960), 960 * 540);
    assert_eq!(yuv420p.plane_size(2, 1080, 960), 960 * 540);
    assert_eq!(yuv420p.plane_size(3, 1080, 960), 0, "no fourth plane");

    // 4:2:2 decimates horizontally only, so chroma keeps the full height.
    let yuv422p = PixFmt::from_name("yuv422p").expect("present");
    assert_eq!(yuv422p.plane_size(1, 1080, 960), 960 * 1080);

    // 4:4:0 decimates vertically only.
    let yuv440p = PixFmt::from_name("yuv440p").expect("present");
    assert_eq!(yuv440p.plane_size(1, 1080, 1920), 1920 * 540);

    // 4:1:0 is quartered in both directions.
    let yuv410p = PixFmt::from_name("yuv410p").expect("present");
    assert_eq!(yuv410p.plane_size(1, 1080, 480), 480 * 270);

    // The alpha plane is never decimated.
    let yuva420p = PixFmt::from_name("yuva420p").expect("present");
    assert_eq!(yuva420p.plane_size(3, 1080, 1920), 1920 * 1080);

    // Odd heights round up, so the last row of pixels still has chroma.
    assert_eq!(yuv420p.plane_size(0, 1081, 1920), 1920 * 1081);
    assert_eq!(yuv420p.plane_size(1, 1081, 960), 960 * 541);

    // NV12's single chroma plane is half height and full width.
    let nv12 = PixFmt::from_name("nv12").expect("present");
    assert_eq!(nv12.plane_size(1, 1080, 1920), 1920 * 540);

    // GBR is not subsampled at all.
    let gbrp = PixFmt::from_name("gbrp").expect("present");
    for p in 0..3 {
        assert_eq!(gbrp.plane_size(p, 1080, 1920), 1920 * 1080);
    }
}

#[test]
fn min_stride_matches_hand_computed_values() {
    let cases: &[(&str, u32, u8, usize)] = &[
        ("yuv420p", 1920, 0, 1920),
        ("yuv420p", 1920, 1, 960),
        ("yuv420p", 1921, 1, 961), // odd width rounds up
        ("yuv410p", 1920, 1, 480),
        ("yuv411p", 1920, 1, 480),
        ("yuv422p", 1920, 1, 960),
        ("yuv440p", 1920, 1, 1920), // no horizontal decimation
        ("yuv420p10le", 1920, 0, 3840),
        ("yuv420p10le", 1920, 1, 1920),
        ("nv12", 1920, 1, 1920), // interleaved: half the samples, twice the step
        ("p010le", 1920, 0, 3840),
        ("p010le", 1920, 1, 3840),
        ("rgb24", 1920, 0, 5760),
        ("rgba", 1920, 0, 7680),
        ("0rgb", 1920, 0, 7680), // padding still occupies the row
        ("rgb565le", 1920, 0, 3840),
        ("rgb48le", 1920, 0, 11520),
        ("rgbaf32le", 1920, 0, 30720),
        ("gray", 1920, 0, 1920),
        ("gray16le", 1920, 0, 3840),
        ("ya8", 1920, 0, 3840),
        ("pal8", 1920, 0, 1920),
        ("yuyv422", 1920, 0, 3840),
        ("uyvy422", 1920, 0, 3840),
        ("uyyvyy411", 1920, 0, 2880), // 6 bytes per 4 pixels
        ("y210le", 1920, 0, 7680),
        ("monow", 1920, 0, 240), // one bit per pixel
        ("monow", 1921, 0, 241), // rounded up to a whole byte
        ("rgb4", 1920, 0, 960),  // one nibble per pixel
        ("rgb4", 1921, 0, 961),
        ("rgb4_byte", 1920, 0, 1920), // same packing, one byte per pixel
        ("bayer_bggr8", 1920, 0, 1920),
        ("bayer_rggb16le", 1920, 0, 3840),
        ("xyz12le", 1920, 0, 11520),
    ];
    for &(name, width, plane, expected) in cases {
        let fmt = PixFmt::from_name(name).expect("present");
        assert_eq!(
            fmt.min_stride(width, plane),
            expected,
            "{name} plane {plane} at width {width}"
        );
    }
}

#[test]
fn plane_layout_aligns_and_totals() {
    let yuv420p = PixFmt::from_name("yuv420p").expect("present");
    let l = yuv420p.plane_layout(1920, 1080, 32).expect("valid");
    assert_eq!(l.planes, 3);
    assert_eq!(l.strides, [1920, 960, 960, 0]);
    assert_eq!(l.sizes[0], 1920 * 1080);
    assert_eq!(l.sizes[1], 960 * 540);
    assert_eq!(l.sizes[2], 960 * 540);
    assert_eq!(l.total, 1920 * 1080 + 2 * 960 * 540);

    // An unaligned width is padded per plane, not globally.
    let l = yuv420p.plane_layout(1921, 1080, 64).expect("valid");
    assert_eq!(l.strides[0], 1984, "1921 rounded up to a multiple of 64");
    assert_eq!(l.strides[1], 1024, "961 rounded up to a multiple of 64");
    assert_eq!(l.total, 1984 * 1080 + 2 * 1024 * 540);

    assert!(yuv420p.plane_layout(16, 16, 0).is_err());
    assert!(
        yuv420p.plane_layout(16, 16, 48).is_err(),
        "48 is not a power of two"
    );
}

#[test]
fn plane_layout_refuses_to_overflow() {
    let fmt = PixFmt::from_name("rgba128le").expect("present");
    assert!(
        fmt.plane_layout(u32::MAX, u32::MAX, 64).is_err(),
        "a frame this size cannot be described, and must not wrap"
    );
}

#[test]
fn descriptor_folds_at_compile_time() {
    // If `descriptor` ever stops being usable in a const context this fails to
    // compile, which is the property the crate exists to provide.
    const D: &super::PixFmtDescriptor = PixFmt::Yuv420p.descriptor();
    const PLANES: usize = PixFmt::Yuv420p.plane_count();
    const SIZE: usize = PixFmt::Nv12.plane_size(1, 1080, 1920);
    assert_eq!(D.name, "yuv420p");
    assert_eq!(PLANES, 3);
    assert_eq!(SIZE, 1920 * 540);
}

// ------------------------------------------------------------------ properties

fn any_format() -> impl Strategy<Value = PixFmt> {
    proptest::sample::select(PixFmt::all())
}

proptest! {
    /// No descriptor may be internally inconsistent, whichever one we land on.
    #[test]
    fn descriptors_are_self_consistent(fmt in any_format()) {
        let d = fmt.descriptor();
        prop_assert!(!d.name.is_empty());
        prop_assert_eq!(PixFmt::from_name(d.name).ok(), Some(fmt));
        prop_assert!(d.log2_chroma_w <= 2);
        prop_assert!(d.log2_chroma_h <= 2);
        prop_assert!(d.components.len() <= 4);
        prop_assert_eq!(d.planes > 1, d.flags.contains(F::PLANAR));

        if d.flags.contains(F::HW_ACCEL) {
            prop_assert_eq!(d.planes, 0);
            prop_assert!(d.components.is_empty());
            return Ok(());
        }

        prop_assert!(!d.components.is_empty());
        let mut seen = [false; 4];
        for c in d.components {
            prop_assert!(c.depth > 0 && c.depth <= 32);
            prop_assert!(c.plane < d.planes, "component outside the plane count");
            prop_assert!(u16::from(c.shift) + u16::from(c.depth) <= 32);
            if let Some(s) = seen.get_mut(c.plane as usize) {
                *s = true;
            }
        }
        for (p, used) in seen.iter().enumerate().take(d.planes as usize) {
            prop_assert!(used, "plane {} is declared but unused", p);
        }
        prop_assert!(d.bits_per_pixel > 0);
        prop_assert!(u32::from(d.bits_per_pixel) <= 128);
    }

    /// Geometry must be monotone, decimation-respecting and overflow-free for
    /// any dimensions and alignment a caller can reach.
    #[test]
    fn geometry_is_sane(
        fmt in any_format(),
        width in 1u32..8192,
        height in 1u32..8192,
        align_log2 in 0u32..7,
    ) {
        let align = 1usize << align_log2;
        let layout = fmt.plane_layout(width, height, align);
        if fmt.is_hw() {
            let l = layout.expect("a hw surface has an empty but valid layout");
            prop_assert_eq!(l.planes, 0);
            prop_assert_eq!(l.total, 0);
            return Ok(());
        }
        let l = layout.expect("these dimensions fit comfortably");
        prop_assert_eq!(l.planes, fmt.plane_count());

        let mut total = 0usize;
        for p in 0..l.planes {
            let plane = p as u8;
            let stride = l.strides[p];
            prop_assert!(stride >= fmt.min_stride(width, plane), "stride below the minimum");
            prop_assert_eq!(stride % align, 0, "stride not aligned");
            prop_assert!(stride < fmt.min_stride(width, plane) + align, "over-padded");

            let rows = fmt.plane_height(height, plane);
            prop_assert_eq!(l.sizes[p], stride * rows as usize);
            prop_assert_eq!(fmt.plane_size(plane, height, stride), l.sizes[p]);

            // A chroma plane is decimated; luma and alpha never are.
            prop_assert!(rows <= height);
            prop_assert!(rows >= height >> 2);
            prop_assert!(fmt.plane_width(width, plane) <= width);
            total += l.sizes[p];
        }
        prop_assert_eq!(l.total, total);
        for p in l.planes..4 {
            prop_assert_eq!(l.strides[p], 0);
            prop_assert_eq!(l.sizes[p], 0);
        }
    }

    /// `from_name` must reject anything it does not know, never panic, and never
    /// invent a format. Anything it does accept must be the canonical name, a
    /// declared alias, or the host-endian widening of one.
    #[test]
    fn from_name_accepts_only_real_spellings(s in ".{0,32}") {
        if let Ok(fmt) = PixFmt::from_name(&s) {
            let native = if cfg!(target_endian = "big") { "be" } else { "le" };
            prop_assert!(
                fmt.name() == s
                    || PixFmt::from_name(fmt.name()).ok() == Some(fmt),
                "accepted `{}` but it does not resolve back", s
            );
            prop_assert!(
                s == fmt.name() || format!("{s}{native}") == fmt.name() || !s.is_empty(),
                "the empty string must never parse"
            );
        }
    }
}
