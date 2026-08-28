//! Setup header parsing (spec section 4.2.4): codebooks, floors, residues,
//! channel mappings and modes.
//!
//! `Vaco-Spec-Ref: vorbis-i section 4.2.4`

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::bitreader::{BitReaderLsb, ilog};
use crate::codebook::Codebook;
use crate::floor0::Floor0Config;
use crate::floor1::Floor1Config;
use crate::residue::ResidueConfig;

#[derive(Debug, Clone)]
pub(crate) enum FloorConfig {
    Type0(Floor0Config),
    Type1(Floor1Config),
}

#[derive(Debug, Clone)]
pub(crate) struct Mapping {
    pub(crate) coupling: Vec<(u16, u16)>,
    pub(crate) mux: Vec<u8>,
    pub(crate) submap_floor: Vec<u8>,
    pub(crate) submap_residue: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Mode {
    pub(crate) blockflag: bool,
    pub(crate) mapping: u8,
}

#[derive(Debug, Clone)]
pub(crate) struct Setup {
    pub(crate) codebooks: Vec<Codebook>,
    pub(crate) floors: Vec<FloorConfig>,
    pub(crate) residues: Vec<ResidueConfig>,
    pub(crate) mappings: Vec<Mapping>,
    pub(crate) modes: Vec<Mode>,
}

/// Parse the setup header body (the packet payload with its `[type][vorbis]`
/// common header already stripped).
pub(crate) fn parse(body: &[u8], budget: &mut Budget, audio_channels: u8) -> Result<Setup> {
    let mut r = BitReaderLsb::new(body);

    // Codebooks.
    let codebook_count = r.get(8).saturating_add(1);
    let mut codebooks: Vec<Codebook> = Vec::new();
    for _ in 0..codebook_count {
        codebooks.push(Codebook::parse(&mut r, budget)?);
    }
    let max_codebook = codebook_count.saturating_sub(1);

    // Time-domain transforms: placeholders, must all read zero.
    let time_count = r.get(6).saturating_add(1);
    for _ in 0..time_count {
        if r.get(16) != 0 {
            return Err(Error::InvalidData(
                "vorbis: nonzero time-domain transform placeholder",
            ));
        }
    }
    if r.overran() {
        return Err(Error::InvalidData(
            "vorbis: eop decoding time-domain placeholders",
        ));
    }

    // Floors.
    let floor_count = r.get(6).saturating_add(1);
    let mut floors: Vec<FloorConfig> = Vec::new();
    for _ in 0..floor_count {
        let floor_type = r.get(16);
        let cfg = match floor_type {
            0 => FloorConfig::Type0(Floor0Config::parse_header(&mut r, budget, max_codebook)?),
            1 => FloorConfig::Type1(Floor1Config::parse_header(&mut r, budget, max_codebook)?),
            _ => return Err(Error::InvalidData("vorbis: floor type greater than 1")),
        };
        floors.push(cfg);
    }

    // Residues.
    let residue_count = r.get(6).saturating_add(1);
    let mut residues: Vec<ResidueConfig> = Vec::new();
    for _ in 0..residue_count {
        let residue_type = r.get(16);
        if residue_type > 2 {
            return Err(Error::InvalidData("vorbis: residue type greater than 2"));
        }
        residues.push(ResidueConfig::parse_header(
            residue_type as u8,
            &mut r,
            budget,
            &codebooks,
        )?);
    }

    // Mappings (mapping type 0 only).
    let mapping_count = r.get(6).saturating_add(1);
    let mut mappings: Vec<Mapping> = Vec::new();
    let channel_bits = ilog(i64::from(audio_channels).saturating_sub(1));
    for _ in 0..mapping_count {
        let mapping_type = r.get(16);
        if mapping_type != 0 {
            return Err(Error::InvalidData("vorbis: nonzero mapping type"));
        }
        let submaps = if r.get_bool() {
            r.get(4).saturating_add(1)
        } else {
            1
        };
        let mut coupling: Vec<(u16, u16)> = Vec::new();
        if r.get_bool() {
            let steps = r.get(8).saturating_add(1);
            for _ in 0..steps {
                let magnitude = r.get(channel_bits.max(1));
                let angle = r.get(channel_bits.max(1));
                if magnitude == angle
                    || magnitude >= u32::from(audio_channels)
                    || angle >= u32::from(audio_channels)
                {
                    return Err(Error::InvalidData("vorbis: invalid channel coupling step"));
                }
                coupling.push((magnitude as u16, angle as u16));
            }
        }
        if r.get(2) != 0 {
            return Err(Error::InvalidData("vorbis: nonzero mapping reserved field"));
        }
        let mut mux: Vec<u8> = budget.alloc(audio_channels as usize)?;
        if submaps > 1 {
            for slot in &mut mux {
                let m = r.get(4);
                if m >= submaps {
                    return Err(Error::InvalidData("vorbis: mapping mux out of range"));
                }
                *slot = m as u8;
            }
        }
        let mut submap_floor: Vec<u8> = budget.alloc(submaps as usize)?;
        let mut submap_residue: Vec<u8> = budget.alloc(submaps as usize)?;
        for i in 0..submaps as usize {
            let _time_placeholder = r.get(8);
            let floor_number = r.get(8);
            if floor_number >= floor_count {
                return Err(Error::InvalidData(
                    "vorbis: mapping floor number out of range",
                ));
            }
            let residue_number = r.get(8);
            if residue_number >= residue_count {
                return Err(Error::InvalidData(
                    "vorbis: mapping residue number out of range",
                ));
            }
            if let Some(s) = submap_floor.get_mut(i) {
                *s = floor_number as u8;
            }
            if let Some(s) = submap_residue.get_mut(i) {
                *s = residue_number as u8;
            }
        }
        if r.overran() {
            return Err(Error::InvalidData("vorbis: eop decoding mapping"));
        }
        mappings.push(Mapping {
            coupling,
            mux,
            submap_floor,
            submap_residue,
        });
    }

    // Modes.
    let mode_count = r.get(6).saturating_add(1);
    let mut modes: Vec<Mode> = Vec::new();
    for _ in 0..mode_count {
        let blockflag = r.get_bool();
        let windowtype = r.get(16);
        let transformtype = r.get(16);
        let mapping = r.get(8);
        if windowtype != 0 || transformtype != 0 {
            return Err(Error::InvalidData(
                "vorbis: unsupported window or transform type",
            ));
        }
        if mapping >= mapping_count {
            return Err(Error::InvalidData(
                "vorbis: mode mapping number out of range",
            ));
        }
        modes.push(Mode {
            blockflag,
            mapping: mapping as u8,
        });
    }
    if r.overran() {
        return Err(Error::InvalidData("vorbis: eop decoding modes"));
    }
    if !r.get_bool() {
        return Err(Error::InvalidData(
            "vorbis: setup header framing bit is unset",
        ));
    }

    Ok(Setup {
        codebooks,
        floors,
        residues,
        mappings,
        modes,
    })
}
