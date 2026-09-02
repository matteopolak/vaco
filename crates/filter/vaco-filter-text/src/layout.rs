//! [`TextRenderer`]: the one shaping/rasterisation path `drawtext` and
//! `vaco-ass` both sit on (plan 16 SS6.1's own stated goal — "so that
//! `drawtext`, `subtitles`, `ass`... all render glyphs the same way").
//!
//! Shaping is `cosmic_text::Buffer` (which shapes through `rustybuzz` and
//! discovers fonts through `fontdb` internally — both "via cosmic-text" in
//! the plan's own architecture table) plus [`crate::alias`]'s family
//! resolution on top. Rasterisation is `cosmic_text::SwashCache`, which
//! *is* this crate's glyph cache: it is keyed by [`cosmic_text::CacheKey`]
//! (font, glyph id, size, sub-pixel position) and lives for the
//! `TextRenderer`'s whole lifetime, so a re-laid-out `drawtext` with
//! `%{pts}` changing every frame still rasterises each distinct glyph
//! exactly once (plan 16 SS6.1's stated reason a glyph cache is not
//! optional). [`TextRenderer::shape_cache`] adds the layer above that:
//! whole shaped-and-positioned runs, keyed by `(text, style)`, so a string
//! that repeats across frames skips shaping too.

use std::collections::HashMap;

use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, Style as FontStyle, SwashCache,
    Weight,
};
use vaco_core::Result;
use vaco_limits::{Budget, Limits};

use crate::mask::AlphaMask;
use crate::style::{TextStyle, Wrap};

/// One glyph's rasterisation identity and its pen position relative to the
/// layout's own top-left corner.
#[derive(Debug, Clone, Copy)]
struct PlacedGlyph {
    cache_key: CacheKey,
    x: i32,
    y: i32,
}

/// The result of [`TextRenderer::layout`]: sized, positioned glyphs, not yet
/// rasterised.
#[derive(Debug, Clone)]
pub struct Layout {
    glyphs: Vec<PlacedGlyph>,
    pub width: u32,
    pub height: u32,
}

impl Layout {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ShapeKey {
    text: String,
    family: String,
    fontfile: Option<std::path::PathBuf>,
    size_bits: u32,
    line_spacing_bits: u32,
    bold: bool,
    italic: bool,
    wrap_width_bits: Option<u32>,
}

/// Bound on the shaped-run cache and the glyph-image cache: `drawtext`'s
/// `%{pts}` produces a fresh string every frame, so both are attacker/
/// content-controlled in count over a long run and must not grow forever.
const SHAPE_CACHE_CAP: usize = 512;
const GLYPH_CACHE_CAP: usize = 4096;

pub struct TextRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    budget: Budget,
    shape_cache: HashMap<ShapeKey, (Layout, u64)>,
    tick: u64,
}

impl std::fmt::Debug for TextRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextRenderer").finish_non_exhaustive()
    }
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            budget: Budget::new(Limits::default()),
            shape_cache: HashMap::new(),
            tick: 0,
        }
    }

    /// This renderer's own allocation budget — shared by [`Self::rasterise`]
    /// and by mask post-processing ([`crate::mask::AlphaMask::dilate`]/
    /// [`crate::mask::AlphaMask::box_blur`]) so a caller never needs a
    /// second `Budget` just to grow a mask it already got from here.
    pub fn budget_mut(&mut self) -> &mut Budget {
        &mut self.budget
    }

    /// Add a directory to the font search path (`-font_dirs`).
    pub fn add_font_dir(&mut self, dir: &std::path::Path) {
        crate::alias::add_search_dir(&mut self.font_system, dir);
    }

    /// Load an embedded font's bytes (a Matroska attachment) so it can be
    /// selected by family name.
    pub fn add_embedded_font(&mut self, bytes: Vec<u8>) {
        crate::alias::load_embedded(&mut self.font_system, bytes);
    }

    fn pick_family(&self, candidates: &[String]) -> Option<String> {
        for name in candidates {
            let query = cosmic_text::fontdb::Query {
                families: &[Family::Name(name)],
                ..Default::default()
            };
            if self.font_system.db().query(&query).is_some() {
                return Some(name.clone());
            }
        }
        None
    }

    /// Shape and position `text` under `style`, wrapping per `wrap`.
    ///
    /// An empty result (`Layout::is_empty`) means either an empty string or
    /// a font database with no face this renderer could resolve at all — the
    /// second is a real, silent-degradation case worth logging once rather
    /// than panicking on (no system fonts is a legitimate state in a
    /// container image).
    pub fn layout(&mut self, text: &str, style: &TextStyle, wrap: Wrap) -> Layout {
        let wrap_width_bits = match wrap {
            Wrap::None => None,
            Wrap::Word(w) => Some(w.to_bits()),
        };
        let key = ShapeKey {
            text: text.to_owned(),
            family: style.family.clone(),
            fontfile: style.fontfile.clone(),
            size_bits: style.size_px.to_bits(),
            line_spacing_bits: style.line_spacing.to_bits(),
            bold: style.bold,
            italic: style.italic,
            wrap_width_bits,
        };
        if let Some((cached, _)) = self.shape_cache.get_mut(&key) {
            self.tick += 1;
            let out = cached.clone();
            if let Some(slot) = self.shape_cache.get_mut(&key) {
                slot.1 = self.tick;
            }
            return out;
        }

        let fresh = self.layout_uncached(text, style, wrap);
        self.tick += 1;
        if self.shape_cache.len() >= SHAPE_CACHE_CAP
            && let Some(evict) = self
                .shape_cache
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone())
        {
            self.shape_cache.remove(&evict);
        }
        self.shape_cache.insert(key, (fresh.clone(), self.tick));
        fresh
    }

    fn layout_uncached(&mut self, text: &str, style: &TextStyle, wrap: Wrap) -> Layout {
        if style.fontfile.is_none() {
            let candidates = crate::alias::resolve_family(&style.family);
            if self.pick_family(&candidates).is_none() {
                tracing::warn!(family = %style.family, "vaco-filter-text: no matching font face, text will not render");
                return Layout {
                    glyphs: Vec::new(),
                    width: 0,
                    height: 0,
                };
            }
        }
        let resolved = if let Some(path) = &style.fontfile {
            match std::fs::read(path) {
                Ok(bytes) => {
                    self.font_system.db_mut().load_font_data(bytes);
                    self.font_system
                        .db()
                        .faces()
                        .last()
                        .map(|f| {
                            f.families
                                .first()
                                .map_or_else(String::new, |(n, _)| n.clone())
                        })
                        .unwrap_or_default()
                }
                Err(err) => {
                    tracing::warn!(?path, %err, "vaco-filter-text: could not read fontfile");
                    self.pick_family(&crate::alias::resolve_family(&style.family))
                        .unwrap_or_default()
                }
            }
        } else {
            self.pick_family(&crate::alias::resolve_family(&style.family))
                .unwrap_or_default()
        };

        let line_height = style.size_px * 1.2 + style.line_spacing;
        let metrics = Metrics::new(style.size_px, line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let width = match wrap {
            Wrap::None => None,
            Wrap::Word(w) => Some(w),
        };
        buffer.set_size(&mut self.font_system, width, None);

        let mut attrs = Attrs::new().weight(if style.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        });
        attrs = attrs.style(if style.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        });
        if !resolved.is_empty() {
            attrs = attrs.family(Family::Name(&resolved));
        }
        buffer.set_text(&mut self.font_system, text, attrs, Shaping::Advanced);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut glyphs = Vec::new();
        let mut max_w: f32 = 0.0;
        let mut line_count: u32 = 0;
        for run in buffer.layout_runs() {
            line_count += 1;
            max_w = max_w.max(run.line_w);
            for glyph in run.glyphs {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                glyphs.push(PlacedGlyph {
                    cache_key: physical.cache_key,
                    x: physical.x,
                    y: run.line_y as i32 + physical.y,
                });
            }
        }
        if self.swash_cache.image_cache.len() > GLYPH_CACHE_CAP {
            self.swash_cache.image_cache.clear();
        }

        let height = (f64::from(line_count) * f64::from(line_height))
            .ceil()
            .max(0.0);
        Layout {
            glyphs,
            width: max_w.ceil().max(0.0) as u32,
            height: height as u32,
        }
    }

    /// Rasterise `layout` into a coverage mask whose `(0, 0)` sample is
    /// `layout`'s own top-left corner, placed at `origin` in frame space.
    ///
    /// # Errors
    /// [`vaco_core::Error::LimitExceeded`] if the mask's area exceeds this
    /// renderer's allocation budget.
    pub fn rasterise(&mut self, layout: &Layout, origin: (i32, i32)) -> Result<AlphaMask> {
        let mut mask = AlphaMask::blank(
            &mut self.budget,
            origin.0,
            origin.1,
            layout.width.max(1),
            layout.height.max(1),
        )?;
        for g in &layout.glyphs {
            let Some(image) = self
                .swash_cache
                .get_image(&mut self.font_system, g.cache_key)
            else {
                continue;
            };
            let (left, top, gw, gh) = (
                image.placement.left,
                image.placement.top,
                image.placement.width,
                image.placement.height,
            );
            let content = image.content;
            let data = image.data.clone();
            let base_x = g.x + left;
            let base_y = g.y - top;
            blit_glyph(&mut mask, &data, content, gw, gh, base_x, base_y);
        }
        Ok(mask)
    }
}

fn blit_glyph(
    mask: &mut AlphaMask,
    data: &[u8],
    content: cosmic_text::SwashContent,
    gw: u32,
    gh: u32,
    base_x: i32,
    base_y: i32,
) {
    match content {
        cosmic_text::SwashContent::Mask => {
            for y in 0..gh {
                for x in 0..gw {
                    let Some(&v) = data.get((y * gw + x) as usize) else {
                        continue;
                    };
                    let (xi, yi) = (i32::try_from(x).unwrap_or(0), i32::try_from(y).unwrap_or(0));
                    write_coverage(mask, base_x + xi, base_y + yi, v);
                }
            }
        }
        cosmic_text::SwashContent::Color => {
            // Colour glyphs (emoji) carry their own RGBA; treat the alpha
            // channel as coverage, discarding colour — the mask is a
            // single-channel coverage buffer by design (SS6.1's own
            // `AlphaMask`), and colour-font support is out of scope here.
            for y in 0..gh {
                for x in 0..gw {
                    let idx = ((y * gw + x) * 4) as usize;
                    let Some(&a) = data.get(idx + 3) else {
                        continue;
                    };
                    let (xi, yi) = (i32::try_from(x).unwrap_or(0), i32::try_from(y).unwrap_or(0));
                    write_coverage(mask, base_x + xi, base_y + yi, a);
                }
            }
        }
        cosmic_text::SwashContent::SubpixelMask => {}
    }
}

fn write_coverage(mask: &mut AlphaMask, x: i32, y: i32, v: u8) {
    if x < mask.x || y < mask.y {
        return;
    }
    let (dx, dy) = (x - mask.x, y - mask.y);
    let (Ok(dx), Ok(dy)) = (usize::try_from(dx), usize::try_from(dy)) else {
        return;
    };
    if dx >= mask.w as usize || dy >= mask.h as usize {
        return;
    }
    if let Some(slot) = mask.coverage.get_mut(dy * mask.w as usize + dx) {
        *slot = (*slot).max(v);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn layout_of_empty_text_is_empty() {
        let mut r = TextRenderer::new();
        let l = r.layout("", &TextStyle::default(), Wrap::None);
        assert_eq!(l.width, 0);
    }

    #[test]
    fn a_real_font_renders_nonzero_coverage() {
        let mut r = TextRenderer::new();
        let style = TextStyle {
            family: "sans-serif".to_owned(),
            size_px: 32.0,
            ..TextStyle::default()
        };
        let l = r.layout("Hi", &style, Wrap::None);
        if l.is_empty() {
            // No system font resolved in this environment (e.g. a
            // container with no fonts installed) — a legitimate outcome,
            // not a failure of this renderer.
            return;
        }
        let mask = r.rasterise(&l, (0, 0)).unwrap();
        assert!(
            mask.coverage.iter().any(|&c| c > 0),
            "expected at least one covered pixel"
        );
    }

    #[test]
    fn repeated_layout_calls_hit_the_shape_cache() {
        let mut r = TextRenderer::new();
        let style = TextStyle::default();
        let a = r.layout("cached text", &style, Wrap::None);
        let before = r.shape_cache.len();
        let b = r.layout("cached text", &style, Wrap::None);
        assert_eq!(
            r.shape_cache.len(),
            before,
            "a repeated key must not grow the cache"
        );
        assert_eq!(a.width, b.width);
    }

    #[test]
    fn word_wrap_bounds_line_width() {
        let mut r = TextRenderer::new();
        let style = TextStyle {
            size_px: 16.0,
            ..TextStyle::default()
        };
        let l = r.layout(
            "a very long sentence that should wrap across several lines",
            &style,
            Wrap::Word(60.0),
        );
        if l.is_empty() {
            return;
        }
        assert!(
            l.height > (style.size_px * 1.2) as u32,
            "wrapped text should span more than one line"
        );
    }
}
