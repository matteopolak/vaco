//! The channel-layout description grammar against arbitrary text.
//!
//! Reachable from a command line (`-ch_layout`) and from container metadata, so
//! the input is attacker-chosen. Findings are a panic, a hang, an arithmetic
//! overflow (this profile turns overflow checks on), or an unbounded allocation
//! — the layout string can name a very large channel count, and a parser that
//! eagerly materialised one would be a denial of service in a single argument.
//!
//! The properties asserted here are the ones that hold for *every* accepted
//! string, not just the well-formed ones:
//!
//! 1. Whatever comes back can be described, and describing it terminates.
//! 2. The description parses back to an equal layout — the grammar is a fixed
//!    point after one pass, which is what makes `ffprobe` output round-trippable
//!    through `-ch_layout`.
//! 3. `channel_at` agrees with `index_of` on the first occurrence of a channel,
//!    and returns `None` exactly at the channel count.
//! fuzz-crate: vaco-chlayout
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_chlayout::{ChannelLayout, ChannelOrder};

/// A layout can legitimately claim millions of channels (`ambisonic 255` has
/// 65 536), so indexing every one of them would make the target time out rather
/// than find bugs. Walk a bounded prefix instead.
const WALK: u32 = 512;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Some(layout) = ChannelLayout::from_name(text) else {
        return;
    };

    assert!(layout.channels > 0, "{text:?} produced a zero-channel layout");

    let described = layout.describe();
    let reparsed = ChannelLayout::from_name(&described)
        .unwrap_or_else(|| panic!("{text:?} described as {described:?}, which does not parse"));
    assert_eq!(
        reparsed, layout,
        "{text:?} -> {described:?} -> a different layout"
    );
    assert_eq!(
        reparsed.describe(),
        described,
        "{described:?} is not a fixed point"
    );

    let unspecified = matches!(layout.order, ChannelOrder::Unspecified);
    for i in 0..layout.channels.min(WALK) {
        let channel = layout
            .channel_at(i)
            .unwrap_or_else(|| panic!("{text:?} has no channel at {i} of {}", layout.channels));
        match layout.index_of(channel) {
            // An unspecified layout has channels but no positions, so it never
            // answers `index_of`. Everything else must find the channel at or
            // before where we read it — before, when the channel repeats.
            None => assert!(unspecified, "{text:?} lost the channel it just yielded"),
            Some(j) => {
                assert!(!unspecified);
                assert!(j <= i);
                assert_eq!(layout.channel_at(j), Some(channel));
            }
        }
    }
    assert_eq!(layout.channel_at(layout.channels), None);
});
