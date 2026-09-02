//! Shared plumbing for every `vaco-bsf-*` crate.
//!
//! # What this is
//!
//! [`vaco_codec_core::BitstreamFilter`] is a hand-rolled push/pull state
//! machine — `send_packet`/`receive_packet`, the same shape as [`Decoder`] —
//! and every filter in `vaco-bsf-generic` and `vaco-bsf-h2645` needs the same
//! boilerplate around it: a bounded output queue, the end-of-stream bookkeeping,
//! and the `Err(NeedMoreInput)` / `Err(Eof)` convention
//! [`vaco_format_core::mux::BsfChain::filter`]'s `drain_filter` helper already
//! relies on. [`MappedFilter`] writes that once. A filter author implements
//! [`PacketMap::push`] — "given this packet, what packets come out" — and gets
//! a full [`BitstreamFilter`] for free.
//!
//! [`BsfDesc`] is the other half: the static, registry-facing description a
//! `vaco-component.toml` fragment's `ctor` names, mirroring
//! [`vaco_codec_core::ParserDesc`] and [`vaco_codec_core::DecoderDesc`] one
//! layer down. `vaco-registry` does not have a typed table for
//! `kind = "bitstream_filter"` yet (`vaco_registry::Kind::has_table` says so
//! directly), so `vaco-registry`'s own `BsfProvider` impl matches on `name`
//! against the [`BsfDesc`] each filter crate exports — see that crate's docs
//! for why that is a deliberate, scoped workaround rather than a widening of
//! the frozen `BsfProvider` trait.
//!
//! # Why a queue cap that is not [`MAX_BSF_EXPANSION`]
//!
//! [`vaco_format_core::mux::MAX_BSF_EXPANSION`] bounds a whole **chain**'s
//! output per input packet, enforced by the driver that owns the chain. That
//! driver is not the only caller a filter can have: a fuzz target drives a
//! `Box<dyn BitstreamFilter>` directly, with no chain and no chain-level cap
//! above it. [`MAX_QUEUED_PACKETS`] is the same idea, moved one layer down, so a
//! single filter instance fed pathological input cannot grow its own internal
//! queue without bound even when nothing above it is watching.
//!
//! # How to change it
//!
//! Add a filter by implementing [`PacketMap`] in `vaco-bsf-generic` or
//! `vaco-bsf-h2645`, wrapping it in [`MappedFilter::new`], and exporting a
//! [`BsfDesc`] for it. Nothing here should need to change for a new filter —
//! if it does, the filter needs something [`PacketMap`] cannot express (see
//! `planning/INTERFACE-GAPS.md` for the one already recorded: no per-instance
//! option string reaches [`BsfProvider::open`](vaco_format_core::mux::BsfProvider::open)).
//!
//! # Configuration
//!
//! None. This crate has no options of its own.
//!
//! # Dependencies
//!
//! `vaco-codec-core` for [`BitstreamFilter`] and [`CodecParameters`];
//! `vaco-packet` for [`Packet`]; `vaco-core`/`vaco-limits` for the error and
//! budget types every filter needs.

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use vaco_codec_core::{BitstreamFilter, CodecParameters};
use vaco_core::{Error, Result};
use vaco_packet::Packet;

/// The most packets one filter instance will hold before it refuses more
/// input (see the module docs for why this is not [`MAX_BSF_EXPANSION`]).
///
/// Generous relative to what any filter here actually produces per packet —
/// every one of them is 1-in/0-or-1-out, or 1-in/2-out for `dump_extra` — so
/// this bounds a filter that is either broken or being driven by a crafted
/// input designed to grow its queue forever, not normal operation.
pub const MAX_QUEUED_PACKETS: usize = 4096;

/// Static description of one bitstream-filter implementation, the registry
/// analogue of [`vaco_codec_core::DecoderDesc`]/[`vaco_codec_core::ParserDesc`].
///
/// `build` is a plain function pointer, not a closure, so a descriptor is a
/// `'static` value a fragment's `ctor` can name and `vaco-registry` can list
/// without constructing anything — the same "inspectable without
/// instantiating" property every other descriptor type has.
#[derive(Clone, Copy)]
pub struct BsfDesc {
    /// The name a `BsfProvider::open` call and a `-bsf:v name` spelling use.
    pub name: &'static str,
    pub long_name: &'static str,
    /// Construct one instance, configured for a stream described by `params`.
    ///
    /// Takes no options string: [`BsfProvider::open`](vaco_format_core::mux::BsfProvider::open)
    /// has none to pass. Every filter here implements the reference's
    /// *default*-option behaviour for exactly that reason — see the crate
    /// docs of whichever `vaco-bsf-*` crate exports this descriptor.
    pub build: fn(&CodecParameters) -> Result<Box<dyn BitstreamFilter>>,
}

impl core::fmt::Debug for BsfDesc {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BsfDesc")
            .field("name", &self.name)
            .field("long_name", &self.long_name)
            .finish_non_exhaustive()
    }
}

/// One step of a bitstream filter: given a packet (or `None` for end of
/// stream), append zero or more output packets to `out`.
///
/// This is deliberately narrower than [`BitstreamFilter`] itself: it has no
/// `Err(OutputPending)` escape, because
/// [`BsfChain::filter`](vaco_format_core::mux::BsfChain::filter) never handles
/// one — it calls `send_packet` and immediately drains, propagating anything
/// else as a hard error. A [`PacketMap`] therefore always accepts its input;
/// [`MappedFilter`] is what turns "accepted but not bounded" into
/// `Err(LimitExceeded)` once [`MAX_QUEUED_PACKETS`] is reached.
pub trait PacketMap: Send {
    /// # Errors
    ///
    /// Whatever the filter's own logic needs to report — a malformed
    /// extradata record, for instance. Never
    /// [`Error::NeedMoreInput`]/[`Error::Eof`]/[`Error::OutputPending`]; those
    /// three are [`MappedFilter`]'s vocabulary, not [`PacketMap`]'s.
    fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()>;

    /// Set one filter-private option by name (gap 12,
    /// `planning/INTERFACE-GAPS.md`) — [`MappedFilter`]'s
    /// [`BitstreamFilter::set_option`] forwards here, so a filter written
    /// against this trait gets the same seam without implementing
    /// [`BitstreamFilter`] by hand.
    ///
    /// The default matches [`BitstreamFilter::set_option`]'s own: "no such
    /// option" for every name. A filter that has options overrides this, not
    /// [`MappedFilter`].
    ///
    /// # Errors
    /// [`Error::Option`] naming `name`. The default always errs.
    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        let _ = value;
        Err(Error::Option {
            name: name.to_owned(),
            detail: "this bitstream filter has no such option".to_owned(),
        })
    }
}

/// Turns a [`PacketMap`] into a full [`BitstreamFilter`].
pub struct MappedFilter<T> {
    inner: T,
    queue: VecDeque<Packet>,
    eof: bool,
}

impl<T: PacketMap> MappedFilter<T> {
    #[must_use]
    pub const fn new(inner: T) -> Self {
        Self {
            inner,
            queue: VecDeque::new(),
            eof: false,
        }
    }
}

impl<T> core::fmt::Debug for MappedFilter<T> {
    /// Hand-written so a filter's own inner state need not be `Debug`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MappedFilter")
            .field("queued", &self.queue.len())
            .field("eof", &self.eof)
            .finish_non_exhaustive()
    }
}

impl<T: PacketMap> BitstreamFilter for MappedFilter<T> {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        if self.eof {
            // The chain protocol never does this — `flush` sends `None`
            // exactly once and then reads until `Eof` — so this only fires
            // against a driver with a bug, or a fuzz target probing the
            // contract directly. Either way the answer is "no", not a panic.
            return Err(Error::InvalidData(
                "bitstream filter: packet sent after end of stream",
            ));
        }
        if packet.is_none() {
            self.eof = true;
        }
        self.inner.push(packet, &mut self.queue)?;
        if self.queue.len() > MAX_QUEUED_PACKETS {
            return Err(Error::LimitExceeded {
                limit: "bsf queued packets",
                requested: u64::try_from(self.queue.len()).unwrap_or(u64::MAX),
                cap: u64::try_from(MAX_QUEUED_PACKETS).unwrap_or(u64::MAX),
            });
        }
        Ok(())
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        match self.queue.pop_front() {
            Some(p) => Ok(p),
            None if self.eof => Err(Error::Eof),
            None => Err(Error::NeedMoreInput),
        }
    }

    // Forwarded explicitly, not inherited from the default: the default
    // would answer "no such option" for every `MappedFilter`, silently
    // hiding whatever `T`'s own `PacketMap::set_option` overrides — the
    // same `Box<dyn Muxer>`/wrapper trap gap 9 and gap 12's own doc comment
    // name, one layer down.
    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        self.inner.set_option(name, value)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    fn pkt(n: u8) -> Packet {
        let mut budget = Budget::new(Limits::strict());
        Packet::from_slice(&mut budget, &[n]).unwrap()
    }

    /// Doubles every packet, so the queue side of `MappedFilter` gets
    /// exercised without a real filter.
    struct Doubler;
    impl PacketMap for Doubler {
        fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
            if let Some(p) = packet {
                out.push_back(p.clone());
                out.push_back(p.clone());
            }
            Ok(())
        }
    }

    #[test]
    fn packets_flow_through_send_and_receive() {
        let mut f = MappedFilter::new(Doubler);
        f.send_packet(Some(&pkt(1))).unwrap();
        assert_eq!(f.receive_packet().unwrap().payload(), &[1]);
        assert_eq!(f.receive_packet().unwrap().payload(), &[1]);
        assert!(matches!(f.receive_packet(), Err(Error::NeedMoreInput)));
    }

    #[test]
    fn end_of_stream_then_eof() {
        let mut f = MappedFilter::new(Doubler);
        f.send_packet(None).unwrap();
        assert!(matches!(f.receive_packet(), Err(Error::Eof)));
    }

    #[test]
    fn a_packet_after_end_of_stream_is_refused() {
        let mut f = MappedFilter::new(Doubler);
        f.send_packet(None).unwrap();
        assert!(f.send_packet(Some(&pkt(1))).is_err());
    }

    #[test]
    fn set_option_default_refuses_every_name() {
        let mut f = MappedFilter::new(Doubler);
        assert!(f.set_option("anything", "1").is_err());
    }

    /// A `PacketMap` with a real option, wrapped in `MappedFilter` — proving
    /// `MappedFilter::set_option` reaches `T::set_option` rather than
    /// silently taking the trait default (gap 12's own named trap, one
    /// layer down from `Box<dyn Muxer>`). `known` starts `false`; a caller
    /// setting `known=1` through the `BitstreamFilter` face and observing
    /// `push` change its output is the deliberate-wrong-value check.
    struct Configurable {
        known: bool,
    }
    impl PacketMap for Configurable {
        fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
            if let Some(p) = packet {
                out.push_back(if self.known { pkt(9) } else { p.clone() });
            }
            Ok(())
        }
        fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
            if name == "known" {
                self.known = value == "1";
                Ok(())
            } else {
                Err(Error::Option {
                    name: name.to_owned(),
                    detail: "unknown".to_owned(),
                })
            }
        }
    }

    #[test]
    fn mapped_filter_forwards_set_option_to_the_inner_packet_map() {
        let mut f: MappedFilter<Configurable> = MappedFilter::new(Configurable { known: false });
        // Reached through `BitstreamFilter::set_option` — the face a
        // `Box<dyn BitstreamFilter>` caller actually holds.
        let bf: &mut dyn BitstreamFilter = &mut f;
        bf.set_option("known", "1").unwrap();
        bf.send_packet(Some(&pkt(1))).unwrap();
        assert_eq!(bf.receive_packet().unwrap().payload(), &[9]);
    }

    #[test]
    fn mapped_filter_still_rejects_an_unknown_option_name() {
        let mut f = MappedFilter::new(Configurable { known: false });
        assert!(f.set_option("nope", "1").is_err());
    }

    /// A filter that emits far more than it consumes trips the queue cap
    /// rather than growing forever.
    struct Flooder;
    impl PacketMap for Flooder {
        fn push(&mut self, packet: Option<&Packet>, out: &mut VecDeque<Packet>) -> Result<()> {
            if packet.is_some() {
                for _ in 0..=MAX_QUEUED_PACKETS {
                    out.push_back(pkt(0));
                }
            }
            Ok(())
        }
    }

    #[test]
    fn the_queue_cap_is_enforced() {
        let mut f = MappedFilter::new(Flooder);
        let err = f.send_packet(Some(&pkt(1))).unwrap_err();
        assert!(matches!(err, Error::LimitExceeded { .. }));
    }

    #[test]
    fn falsified_the_queue_cap_would_pass_a_pathological_flood_without_it() {
        // Planting the defect: a `MappedFilter` with no cap at all would
        // accept the flood silently. Asserting that *this* is what "no cap"
        // looks like is what makes `the_queue_cap_is_enforced` a real test of
        // the guard rather than of `Flooder`'s output count.
        let mut q = VecDeque::new();
        let mut flooder = Flooder;
        flooder.push(Some(&pkt(1)), &mut q).unwrap();
        assert!(q.len() > MAX_QUEUED_PACKETS);
    }
}
