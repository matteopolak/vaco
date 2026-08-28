//! Packing List parsing (SMPTE ST 2067-2 / ST 429-8): the manifest that
//! lists every asset belonging to one IMF package, each with a declared
//! `Size`/`Hash`/`Type` — the integrity-checking layer above the ASSETMAP's
//! pure path lookup.
//!
//! # Scope
//!
//! This crate reads the PKL for **its own `AssetList`** (which assets exist
//! and what type each is) and does not verify `Hash` against the actual
//! file bytes — that is a real, separable feature (SHA-1, already available
//! workspace-wide via `vaco-hash`, D11's single owner of that algorithm)
//! left for whoever needs package integrity checking rather than just
//! playback; see "How to change it" in this crate's top-level docs. Every
//! element name below is spec-derived — see `assetmap.rs`'s module docs for
//! why there is no reference to cross-check it against on this machine.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::assetmap::strip_urn_uuid;
use crate::xml::{self, XmlNode};

/// One `<Asset>` entry in a Packing List.
#[derive(Debug, Clone)]
pub struct PklAsset {
    pub id: String,
    /// `<Type>`, e.g. `application/mxf`, `text/xml;asdcpKind=CPL`. Free text
    /// per the schema; this crate does not attempt to enumerate every legal
    /// value, only reads it through for a caller that wants it.
    pub kind: Option<String>,
    /// `<Size>`, in bytes, when stated (optional per the schema).
    pub size: Option<u64>,
    /// `<Hash>`, base64, when stated. Not verified against file bytes —
    /// see the module docs.
    pub hash: Option<String>,
}

/// The parsed Packing List.
#[derive(Debug, Clone, Default)]
pub struct Pkl {
    pub id: String,
    pub assets: Vec<PklAsset>,
}

impl Pkl {
    /// Look up an asset by its bare, lowercased UUID.
    #[must_use]
    pub fn asset(&self, id: &str) -> Option<&PklAsset> {
        self.assets.iter().find(|a| a.id == id)
    }
}

/// Parse a Packing List document's bytes.
///
/// # Errors
/// [`Error::InvalidData`] for malformed XML, the wrong root element, or a
/// document missing a required element (`Id`, `AssetList`, an `Asset`'s own
/// `Id`).
pub fn parse(xml_bytes: &str, budget: &mut Budget) -> Result<Pkl> {
    let root = xml::parse(xml_bytes, budget)?;
    if root.name != "PackingList" {
        return Err(Error::InvalidData(
            "not a Packing List document (root element is not PackingList)",
        ));
    }
    let id = strip_urn_uuid(
        root.child_text("Id")
            .ok_or(Error::InvalidData("PackingList has no Id"))?,
    );
    let asset_list = root
        .child("AssetList")
        .ok_or(Error::InvalidData("PackingList has no AssetList"))?;
    let mut assets = Vec::new();
    for asset in asset_list.children_named("Asset") {
        assets.push(parse_asset(asset)?);
    }
    Ok(Pkl { id, assets })
}

fn parse_asset(asset: &XmlNode) -> Result<PklAsset> {
    let id_raw = asset
        .child_text("Id")
        .ok_or(Error::InvalidData("PackingList Asset has no Id"))?;
    let size = asset
        .child_text("Size")
        .and_then(|v| v.trim().parse::<u64>().ok());
    Ok(PklAsset {
        id: strip_urn_uuid(id_raw),
        kind: asset.child_text("Type").map(str::to_owned),
        size,
        hash: asset.child_text("Hash").map(str::to_owned),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<PackingList xmlns="http://www.smpte-ra.org/schemas/2067-2/2016/PKL">
  <Id>urn:uuid:11111111-1111-1111-1111-111111111111</Id>
  <AssetList>
    <Asset>
      <Id>urn:uuid:cccccccc-2222-2222-2222-222222222222</Id>
      <Hash>ZmFrZQ==</Hash>
      <Size>1048576</Size>
      <Type>application/mxf</Type>
    </Asset>
  </AssetList>
</PackingList>"#;

    #[test]
    fn parses_the_asset_list() {
        let mut b = Budget::new(Limits::permissive());
        let pkl = parse(SAMPLE, &mut b).unwrap();
        assert_eq!(pkl.id, "11111111-1111-1111-1111-111111111111");
        let a = pkl.asset("cccccccc-2222-2222-2222-222222222222").unwrap();
        assert_eq!(a.size, Some(1_048_576));
        assert_eq!(a.kind.as_deref(), Some("application/mxf"));
    }

    #[test]
    fn rejects_the_wrong_root_element() {
        let mut b = Budget::new(Limits::permissive());
        assert!(parse("<NotAPackingList/>", &mut b).is_err());
    }
}
