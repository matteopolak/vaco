//! ASSETMAP parsing (SMPTE ST 429-9): the file that maps every asset's
//! `UUID` to one or more on-disk `Chunk`s (usually exactly one — IMF allows
//! splitting a single asset across several files, "chunking", which this
//! crate does not reassemble; see "How to change it").
//!
//! # What is spec-derived, not measured
//!
//! No reference implementation was available to check this crate's
//! understanding of ASSETMAP against (this machine's `ffmpeg 8.1` has no
//! `imf` demuxer compiled in at all — confirmed via `ffmpeg -demuxers`, not
//! assumed) — see this crate's own top-level docs, "Verification", for the
//! full account. Every element name and structure below is read directly
//! from the published ST 429-9 schema, the same clean-room posture (D7/D15)
//! this project already applies to specs it has *also* cross-checked against
//! a reference; here there is no second leg of that check available.

use std::collections::HashMap;

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::xml::{self, XmlNode};

/// One `<Asset>` entry: its `UUID` (without the `urn:uuid:` prefix — see
/// [`strip_urn_uuid`]) and the relative path of its first (and, in the
/// common single-chunk case, only) `Chunk`.
#[derive(Debug, Clone)]
pub struct AssetMapEntry {
    pub id: String,
    /// `<Path>`, relative to the ASSETMAP's own directory — resolved to an
    /// absolute path by [`crate::package::Package::open`], not here (this
    /// module has no filesystem access and is exercised standalone by the
    /// fuzz target for exactly that reason).
    pub path: String,
    /// `<PackingList>true</PackingList>` — this asset is itself the/a
    /// Packing List, not a track file. Not required to locate the PKL (a
    /// `Package` can also be told the PKL's `Id` directly by the CPL's own
    /// enclosing context or simply by trying every asset whose path ends in
    /// `.xml`), but stated when present since it is free.
    pub is_packing_list: bool,
}

/// The parsed ASSETMAP: every entry, keyed by `UUID` for
/// [`AssetMap::resolve`].
#[derive(Debug, Clone, Default)]
pub struct AssetMap {
    pub entries: Vec<AssetMapEntry>,
}

impl AssetMap {
    /// Look up `id` (a bare UUID, `urn:uuid:` prefix already stripped).
    #[must_use]
    pub fn resolve(&self, id: &str) -> Option<&AssetMapEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// A `HashMap` view for repeated lookups (virtual-track assembly
    /// resolves one `TrackFileId` per `Resource`, which can be in the
    /// thousands for a long-form composition with many edits).
    #[must_use]
    pub fn as_map(&self) -> HashMap<&str, &AssetMapEntry> {
        self.entries.iter().map(|e| (e.id.as_str(), e)).collect()
    }
}

/// Strip a leading `urn:uuid:` (case-insensitive per RFC 4122's own `URN`
/// registration) — every `<Id>`/`<TrackFileId>`/`<TrackId>` in IMF's XML
/// family states one, and comparing the bare UUID rather than the full URN
/// string is what lets a `TrackFileId` (from the CPL) match an `Id` (in the
/// ASSETMAP) that some encoders state with a different case.
#[must_use]
pub fn strip_urn_uuid(raw: &str) -> String {
    let t = raw.trim();
    t.strip_prefix("urn:uuid:")
        .or_else(|| t.strip_prefix("URN:UUID:"))
        .unwrap_or(t)
        .to_ascii_lowercase()
}

/// Parse an ASSETMAP document's bytes.
///
/// # Errors
/// [`Error::InvalidData`] for malformed XML or a document missing a
/// required element (`AssetList`, an `Asset`'s own `Id`).
pub fn parse(xml_bytes: &str, budget: &mut Budget) -> Result<AssetMap> {
    let root = xml::parse(xml_bytes, budget)?;
    if root.name != "AssetMap" {
        return Err(Error::InvalidData(
            "not an ASSETMAP document (root element is not AssetMap)",
        ));
    }
    let asset_list = root
        .child("AssetList")
        .ok_or(Error::InvalidData("ASSETMAP has no AssetList"))?;

    let mut entries = Vec::new();
    for asset in asset_list.children_named("Asset") {
        entries.push(parse_asset(asset)?);
    }
    Ok(AssetMap { entries })
}

fn parse_asset(asset: &XmlNode) -> Result<AssetMapEntry> {
    let id_raw = asset
        .child_text("Id")
        .ok_or(Error::InvalidData("ASSETMAP Asset has no Id"))?;
    let is_packing_list = asset
        .child_text("PackingList")
        .is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1");
    let chunk_list = asset
        .child("ChunkList")
        .ok_or(Error::InvalidData("ASSETMAP Asset has no ChunkList"))?;
    // Only the first Chunk: multi-chunk assets (a single asset split across
    // several files) are spec-legal but not reassembled here — see the
    // module docs.
    let chunk = chunk_list
        .child("Chunk")
        .ok_or(Error::InvalidData("ASSETMAP ChunkList has no Chunk"))?;
    let path = chunk
        .child_text("Path")
        .ok_or(Error::InvalidData("ASSETMAP Chunk has no Path"))?
        .to_owned();
    if chunk_list.children_named("Chunk").count() > 1 {
        return Err(Error::Unsupported(
            "imf: multi-chunk assets (one asset split across several files) are not reassembled",
        ));
    }

    Ok(AssetMapEntry {
        id: strip_urn_uuid(id_raw),
        path,
        is_packing_list,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<AssetMap xmlns="http://www.smpte-ra.org/schemas/429-9/2007/AM">
  <Id>urn:uuid:aaaaaaaa-0000-0000-0000-000000000000</Id>
  <AssetList>
    <Asset>
      <Id>urn:uuid:BBBBBBBB-1111-1111-1111-111111111111</Id>
      <PackingList>true</PackingList>
      <ChunkList>
        <Chunk><Path>PKL_x.xml</Path></Chunk>
      </ChunkList>
    </Asset>
    <Asset>
      <Id>urn:uuid:cccccccc-2222-2222-2222-222222222222</Id>
      <ChunkList>
        <Chunk><Path>video_track.mxf</Path></Chunk>
      </ChunkList>
    </Asset>
  </AssetList>
</AssetMap>"#;

    #[test]
    fn resolves_by_bare_lowercase_uuid() {
        let mut b = Budget::new(Limits::permissive());
        let map = parse(SAMPLE, &mut b).unwrap();
        assert_eq!(map.entries.len(), 2);
        let pkl = map.resolve("bbbbbbbb-1111-1111-1111-111111111111").unwrap();
        assert!(pkl.is_packing_list);
        assert_eq!(pkl.path, "PKL_x.xml");
        let track = map.resolve("cccccccc-2222-2222-2222-222222222222").unwrap();
        assert!(!track.is_packing_list);
        assert_eq!(track.path, "video_track.mxf");
    }

    #[test]
    fn rejects_the_wrong_root_element() {
        let mut b = Budget::new(Limits::permissive());
        assert!(parse("<NotAnAssetMap/>", &mut b).is_err());
    }
}
