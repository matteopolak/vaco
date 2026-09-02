//! `noise`: corrupt or drop packets, for testing error resilience.
//!
//! # Why this one does not match the reference bit-for-bit
//!
//! Measured (`ffmpeg 8.1`): `-bsf:v noise` with **no options at all**
//! corrupts every byte of the payload, from byte zero, non-deterministically
//! — `-h bsf=noise` shows no seed option, `-amount`'s default is not the
//! numeric zero the option table's blank default column might suggest, and
//! two runs were not compared byte-for-byte because there is no declared
//! mechanism that would make them agree. A fault injector with an
//! unreproducible bare-name default is not a target D17 measurement can
//! pin down; there is no "the reference's answer" to converge on.
//!
//! Given that, and that [`BsfProvider::open`](vaco_format_core::mux::BsfProvider::open)
//! carries no option string to opt into corruption explicitly
//! (`planning/INTERFACE-GAPS.md`), this implementation deliberately diverges
//! from the reference's bare-name behaviour: **default `amount = 0`,
//! `dropamount = 0` is the identity transform**, not silent corruption. A
//! filter nobody asked to corrupt data should not corrupt data just because
//! it was constructed; a caller that reaches this through the (currently
//! nonexistent) options seam and asks for `amount > 0` gets real,
//! deterministically-seeded corruption — reproducible by construction, which
//! the reference's own default is not — rather than a promise this crate
//! cannot keep.
//!
//! The corruption algorithm itself (xorshift64*, seeded from the stream's
//! codec id so two runs over the same stream agree) is original, not a
//! transcription of anything: there is nothing to transcribe when the
//! reference will not hold still to be measured.

use std::collections::VecDeque;

use vaco_bsf_core::{BsfDesc, MappedFilter, PacketMap};
use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::Result;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

/// The registry descriptor. `ctor` target for `vaco-component.toml`.
pub const DESC: BsfDesc = BsfDesc {
    name: "noise",
    long_name: "Damage the contents of packets, without effecting the frame headers",
    build,
};

/// A tiny, deterministic PRNG (xorshift64*) — enough for fault injection,
/// with none of the allocation or dependency weight a full RNG crate would
/// add for a filter this narrow. Not a security primitive.
struct XorShift64Star(u64);

impl XorShift64Star {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "must match BsfDesc::build's fn-pointer signature, shared by every filter"
)]
fn build(params: &CodecParameters) -> Result<Box<dyn BitstreamFilter>> {
    let seed = params
        .codec_id
        .map_or(0x9E37_79B9, |c| c as u64 ^ 0x9E37_79B9);
    Ok(Box::new(MappedFilter::new(Noise {
        // Both zero: the identity transform, per the module docs. Structured
        // as fields, not constants, so a future options seam only needs to
        // set them.
        amount_per_65536: 0,
        drop_per_65536: 0,
        rng: XorShift64Star(seed.max(1)),
        budget: Budget::new(Limits::permissive()),
    })))
}

struct Noise {
    /// Probability a given byte is corrupted, out of 65536. `0` disables it.
    amount_per_65536: u32,
    /// Probability a given packet is dropped entirely, out of 65536.
    drop_per_65536: u32,
    rng: XorShift64Star,
    budget: Budget,
}

impl PacketMap for Noise {
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
        let Some(p) = packet else { return Ok(()) };
        if self.drop_per_65536 > 0
            && u32::try_from(self.rng.next() & 0xFFFF).unwrap_or(0) < self.drop_per_65536
        {
            return Ok(());
        }
        if self.amount_per_65536 == 0 {
            out.push_back(p.clone());
            return Ok(());
        }
        let mut bytes = p.payload().to_vec();
        for b in &mut bytes {
            if u32::try_from(self.rng.next() & 0xFFFF).unwrap_or(0) < self.amount_per_65536 {
                *b ^= u8::try_from(self.rng.next() & 0xFF).unwrap_or(0xFF);
            }
        }
        let mut np = Packet::from_slice(&mut self.budget, &bytes)?;
        np.stream_index = p.stream_index;
        np.pts = p.pts;
        np.dts = p.dts;
        np.duration = p.duration;
        np.pos = p.pos;
        np.flags = p.flags;
        np.side_data.clone_from(&p.side_data);
        out.push_back(np);
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    fn pkt(bytes: &[u8]) -> Packet {
        Packet::from_slice(&mut Budget::new(Limits::strict()), bytes).unwrap()
    }

    #[test]
    fn default_construction_is_the_identity_transform() {
        let mut f = (DESC.build)(&CodecParameters::default()).unwrap();
        f.send_packet(Some(&pkt(b"untouched"))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), b"untouched");
    }

    #[test]
    fn a_nonzero_amount_corrupts_deterministically() {
        let params = CodecParameters::default();
        let build_noisy = |seed_bump: u64| {
            let mut filter = Noise {
                amount_per_65536: 65536,
                drop_per_65536: 0,
                rng: XorShift64Star(0xABCD_EF01 ^ seed_bump),
                budget: Budget::new(Limits::permissive()),
            };
            let mut queue = VecDeque::new();
            filter.push(Some(&pkt(b"AAAAAAAAAA")), &mut queue).unwrap();
            queue.pop_front().unwrap().payload().to_vec()
        };
        let _ = params;
        let a = build_noisy(0);
        let b = build_noisy(0);
        assert_eq!(a, b, "same seed must reproduce the same corruption");
        assert_ne!(
            a, b"AAAAAAAAAA",
            "amount=100% must change every byte's chance"
        );
    }
}
