//! Family expansion: `source::FAMILIES` -> a flat, ordered list of formats.
//!
//! Every rule here is mechanical. If a format needs a rule that is not, it
//! belongs in `Family::Explicit` instead — a family that grows a special case
//! has stopped paying for itself.

use super::model::{Comp, Flag, Format};
use super::source::{
    self, Alpha, BiplanarDef, End, ExplicitDef, Family, Pack, PackedDef, Store, Sub,
};

/// Everything a format is, before endianness is decided.
struct Core {
    comps: Vec<Comp>,
    planes: u8,
    log2_w: u8,
    log2_h: u8,
    flags: Vec<Flag>,
    bpp: Option<u8>,
    aliases: Vec<String>,
}

impl Core {
    fn new(comps: Vec<Comp>, planes: u8, log2: (u8, u8), mut flags: Vec<Flag>) -> Self {
        if planes > 1 {
            flags.push(Flag::Planar);
        }
        if comps.len() == 4 {
            flags.push(Flag::Alpha);
        }
        Self {
            comps,
            planes,
            log2_w: log2.0,
            log2_h: log2.1,
            flags,
            bpp: None,
            aliases: Vec::new(),
        }
    }
}

/// Emit one format, or a big/little-endian pair, from a base name.
fn emit(out: &mut Vec<Format>, base: &str, end: End, core: Core) {
    let bpp = core
        .bpp
        .unwrap_or_else(|| Format::derive_bpp(&core.comps, core.log2_w, core.log2_h));

    let build = |name: String, be: bool, sibling: Option<String>| {
        let mut flags = core.flags.clone();
        if be {
            flags.push(Flag::Be);
        }
        flags.sort_unstable();
        flags.dedup();
        Format {
            variant: variant_of(&name),
            name,
            aliases: core.aliases.clone(),
            comps: core.comps.clone(),
            planes: core.planes,
            log2_chroma_w: core.log2_w,
            log2_chroma_h: core.log2_h,
            bits_per_pixel: bpp,
            flags,
            endian_sibling: sibling,
        }
    };

    match end {
        End::Never => out.push(build(base.to_string(), false, None)),
        End::Pair => {
            let (le, be) = (format!("{base}le"), format!("{base}be"));
            out.push(build(le.clone(), false, Some(be.clone())));
            out.push(build(be, true, Some(le)));
        }
    }
}

/// Derive the Rust enum variant identifier from a format name.
///
/// Split on `_`, capitalise each segment, prefix `X` if the result would start
/// with a digit. Overridden by `source::VARIANT_OVERRIDES` where the mechanical
/// answer reads badly.
fn variant_of(name: &str) -> String {
    for (n, v) in source::VARIANT_OVERRIDES {
        if *n == name {
            return (*v).to_string();
        }
    }
    let mut s = String::new();
    for seg in name.split('_') {
        let mut chars = seg.chars();
        if let Some(c) = chars.next() {
            s.extend(c.to_uppercase());
            s.push_str(chars.as_str());
        }
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, 'X');
    }
    s
}

/// The depth/store suffix a name carries: `""`, `"10"`, `"10msb"`, `"f32"`.
fn depth_tag(depth: u8, store: Store) -> String {
    match store {
        Store::Int if depth <= 8 => String::new(),
        Store::Int => depth.to_string(),
        Store::Msb => format!("{depth}msb"),
        Store::F16 => "f16".to_string(),
        Store::F32 => "f32".to_string(),
    }
}

fn planar_yuv(
    out: &mut Vec<Format>,
    stem: &str,
    subs: &[Sub],
    depths: &[u8],
    alpha: Alpha,
    store: Store,
    end: End,
) {
    for &has_alpha in alpha.variants() {
        for &sub in subs {
            for &depth in depths {
                let (bytes, shift, bits) = store.sample(depth);
                let planes = if has_alpha { 4 } else { 3 };
                let comps: Vec<Comp> = (0..planes)
                    .map(|p| Comp::new(p, bytes, 0, shift, bits))
                    .collect();
                let mut flags = Vec::new();
                if store.is_float() {
                    flags.push(Flag::Float);
                }
                let a = if has_alpha { "a" } else { "" };
                let base = format!("{stem}{a}{}p{}", sub.tag(), depth_tag(depth, store));
                emit(out, &base, end, Core::new(comps, planes, sub.log2(), flags));
            }
        }
    }
}

fn planar_gbr(out: &mut Vec<Format>, depths: &[u8], alpha: Alpha, store: Store, end: End) {
    for &has_alpha in alpha.variants() {
        for &depth in depths {
            let (bytes, shift, bits) = store.sample(depth);
            let planes = if has_alpha { 4 } else { 3 };
            // Planes are stored G, B, R; components are indexed R, G, B, A.
            let plane_of = [2u8, 0, 1, 3];
            let comps: Vec<Comp> = (0..planes as usize)
                .map(|i| Comp::new(plane_of[i], bytes, 0, shift, bits))
                .collect();
            let mut flags = vec![Flag::Rgb];
            if store.is_float() {
                flags.push(Flag::Float);
            }
            let a = if has_alpha { "a" } else { "" };
            let base = format!("gbr{a}p{}", depth_tag(depth, store));
            emit(out, &base, end, Core::new(comps, planes, (0, 0), flags));
        }
    }
}

fn biplanar(out: &mut Vec<Format>, def: &BiplanarDef) {
    let (bytes, shift, bits) = def.store.sample(def.depth);
    let (first, second) = if def.swapped { (bytes, 0) } else { (0, bytes) };
    let comps = vec![
        Comp::new(0, bytes, 0, shift, bits),
        Comp::new(1, bytes * 2, first, shift, bits),
        Comp::new(1, bytes * 2, second, shift, bits),
    ];
    emit(
        out,
        def.name,
        def.end,
        Core::new(comps, 2, def.sub.log2(), Vec::new()),
    );
}

fn gray(out: &mut Vec<Format>, depths: &[u8], alpha: Alpha, store: Store, end: End) {
    for &has_alpha in alpha.variants() {
        for &depth in depths {
            let (bytes, shift, bits) = store.sample(depth);
            let mut flags = Vec::new();
            if store.is_float() {
                flags.push(Flag::Float);
            }
            let (base, comps) = if has_alpha {
                let step = bytes * 2;
                let stem = match store {
                    Store::F16 => "yaf16".to_string(),
                    Store::F32 => "yaf32".to_string(),
                    _ => format!("ya{}", bits),
                };
                (
                    stem,
                    vec![
                        Comp::new(0, step, 0, shift, bits),
                        Comp::new(0, step, bytes, shift, bits),
                    ],
                )
            } else {
                let stem = match store {
                    Store::F16 => "grayf16".to_string(),
                    Store::F32 => "grayf32".to_string(),
                    _ if bits == 8 => "gray".to_string(),
                    _ => format!("gray{bits}"),
                };
                (stem, vec![Comp::new(0, bytes, 0, shift, bits)])
            };
            // A two-component gray format is Y + A, and `Core::new` only infers
            // ALPHA from a four-component layout.
            if has_alpha {
                flags.push(Flag::Alpha);
            }
            emit(out, &base, end, Core::new(comps, 1, (0, 0), flags));
        }
    }
}

fn packed(out: &mut Vec<Format>, def: &PackedDef) {
    assert_eq!(
        def.order.len(),
        def.bits.len(),
        "{}: order and bits disagree",
        def.name
    );
    let mut comps = vec![Comp::new(0, 0, 0, 0, 0); 4];
    let mut used = [false; 4];
    let mut flags: Vec<Flag> = def.extra.to_vec();

    match def.pack {
        Pack::Bytes(n) => {
            let step = n * def.order.len() as u8;
            for (i, (&chan, &bits)) in def.order.iter().zip(def.bits).enumerate() {
                let Some(slot) = chan.slot() else { continue };
                let (_, shift, depth) = def.store.sample(bits);
                comps[slot] = Comp::new(0, step, n * i as u8, shift, depth);
                used[slot] = true;
            }
        }
        // Bitfields inside one container, listed most-significant first.
        // `step` measures the container: bytes for `Field`, bits for
        // `Bitstream`, because a bitstream format has no byte-aligned unit.
        Pack::Field(bytes) => {
            let container = bytes * 8;
            let mut acc = 0u8;
            for (&chan, &bits) in def.order.iter().zip(def.bits) {
                let shift = container - acc - bits;
                acc += bits;
                if let Some(slot) = chan.slot() {
                    comps[slot] = Comp::new(0, bytes, 0, shift, bits);
                    used[slot] = true;
                }
            }
            assert_eq!(acc, container, "{}: fields do not fill container", def.name);
        }
        Pack::Bitstream(container) => {
            flags.push(Flag::Bitstream);
            let mut acc = 0u8;
            for (&chan, &bits) in def.order.iter().zip(def.bits) {
                let shift = container - acc - bits;
                acc += bits;
                if let Some(slot) = chan.slot() {
                    comps[slot] = Comp::new(0, container, 0, shift, bits);
                    used[slot] = true;
                }
            }
            assert_eq!(acc, container, "{}: fields do not fill container", def.name);
        }
    }

    let n_comp = used.iter().filter(|u| **u).count();
    assert!(
        used[..n_comp].iter().all(|u| *u),
        "{}: component slots are not contiguous",
        def.name
    );
    comps.truncate(n_comp);
    if def.store.is_float() {
        flags.push(Flag::Float);
    }
    emit(out, def.name, def.end, Core::new(comps, 1, (0, 0), flags));
}

/// A Bayer mosaic is **three** components, not one.
///
/// Measured: `bayer_bggr8` reports `3 components, 8 bpp, 2-4-2` and
/// `bayer_bggr16le` reports `3, 16, 4-8-4`. The ratio is the 2x2 block itself —
/// one red pixel, *two* green, one blue — so green carries half the bits and
/// red and blue a quarter each, listed in R-G-B component order.
///
/// This modelled the mosaic as a single component of the full depth, which is
/// how the bytes are *stored* but not what the format means: a consumer
/// computing per-component precision from it would conclude every channel has
/// 8 bits of red information, which is exactly backwards for the two channels
/// that are subsampled.
fn bayer(out: &mut Vec<Format>, patterns: &[&str], depths: &[u8], end: End) {
    for &depth in depths {
        for pattern in patterns {
            let bytes = super::model::sample_bytes(depth);
            let (quarter, half) = (depth / 4, depth / 2);
            let comps = vec![
                Comp::new(0, bytes, 0, 0, quarter),
                Comp::new(0, bytes, 0, 0, half),
                Comp::new(0, bytes, 0, 0, quarter),
            ];
            emit(
                out,
                &format!("bayer_{pattern}{depth}"),
                end,
                Core::new(comps, 1, (0, 0), vec![Flag::Rgb, Flag::Bayer]),
            );
        }
    }
}

fn explicit(out: &mut Vec<Format>, def: &ExplicitDef) {
    let comps: Vec<Comp> = def
        .comps
        .iter()
        .map(|&(p, st, o, sh, d)| Comp::new(p, st, o, sh, d))
        .collect();
    let mut core = Core::new(comps, def.planes, def.log2_chroma, def.flags.to_vec());
    core.bpp = def.bpp;
    core.aliases = def.aliases.iter().map(|a| (*a).to_string()).collect();
    emit(out, def.name, def.end, core);
}

fn hw_surface(out: &mut Vec<Format>, name: &str) {
    emit(
        out,
        name,
        End::Never,
        Core {
            comps: Vec::new(),
            planes: 0,
            log2_w: 0,
            log2_h: 0,
            flags: vec![Flag::HwAccel],
            bpp: Some(0),
            aliases: Vec::new(),
        },
    );
}

/// Expand every family, in declaration order.
pub fn expand() -> Vec<Format> {
    let mut out = Vec::new();
    for family in source::FAMILIES {
        match *family {
            Family::PlanarYuv {
                stem,
                subs,
                depths,
                alpha,
                store,
                end,
            } => planar_yuv(&mut out, stem, subs, depths, alpha, store, end),
            Family::PlanarGbr {
                depths,
                alpha,
                store,
                end,
            } => planar_gbr(&mut out, depths, alpha, store, end),
            Family::Biplanar(defs) => {
                for d in defs {
                    biplanar(&mut out, d);
                }
            }
            Family::Gray {
                depths,
                alpha,
                store,
                end,
            } => gray(&mut out, depths, alpha, store, end),
            Family::Packed(defs) => {
                for d in defs {
                    packed(&mut out, d);
                }
            }
            Family::Bayer {
                patterns,
                depths,
                end,
            } => bayer(&mut out, patterns, depths, end),
            Family::HwSurface(names) => {
                for n in names {
                    hw_surface(&mut out, n);
                }
            }
            Family::Explicit(defs) => {
                for d in defs {
                    explicit(&mut out, d);
                }
            }
        }
    }

    for (alias, canonical) in source::ALIASES {
        let target = out
            .iter_mut()
            .find(|f| f.name == *canonical)
            .unwrap_or_else(|| panic!("alias `{alias}` targets unknown format `{canonical}`"));
        target.aliases.push((*alias).to_string());
    }

    out
}

/// Panic with a readable message on any structural mistake in the source.
///
/// These are generator-time assertions about the *declarations*, distinct from
/// the invariants the generated test module asserts about the *table*.
pub fn validate(formats: &[Format]) {
    let mut names = std::collections::BTreeSet::new();
    let mut variants = std::collections::BTreeSet::new();
    for f in formats {
        assert!(names.insert(f.name.clone()), "duplicate name `{}`", f.name);
        assert!(
            variants.insert(f.variant.clone()),
            "duplicate variant `{}` (from `{}`)",
            f.variant,
            f.name
        );
        for a in &f.aliases {
            assert!(names.insert(a.clone()), "alias `{a}` collides with a name");
        }
        assert!(f.comps.len() <= 4, "{}: more than four components", f.name);
        if !f.has(Flag::HwAccel) {
            assert!(!f.comps.is_empty(), "{}: no components", f.name);
            let max_plane = f.comps.iter().map(|c| c.plane).max().unwrap_or(0);
            assert_eq!(
                f.planes,
                max_plane + 1,
                "{}: plane count disagrees with components",
                f.name
            );
            for p in 0..f.planes {
                assert!(
                    f.comps.iter().any(|c| c.plane == p),
                    "{}: plane {p} is unused",
                    f.name
                );
            }
            for c in &f.comps {
                assert!(c.depth > 0 && c.depth <= 32, "{}: bad depth", f.name);
                assert!(c.shift + c.depth <= 32, "{}: field overruns", f.name);
            }
        }
    }
    for f in formats {
        let Some(sibling) = &f.endian_sibling else {
            continue;
        };
        let other = formats
            .iter()
            .find(|o| &o.name == sibling)
            .unwrap_or_else(|| panic!("{}: missing sibling `{sibling}`", f.name));
        assert_eq!(other.comps, f.comps, "{}: sibling layout differs", f.name);
        assert_ne!(
            other.has(Flag::Be),
            f.has(Flag::Be),
            "{}: sibling has the same endianness",
            f.name
        );
    }
}
