//! What the graph layer needs from a filter library, and nothing more.
//!
//! `vaco-filter-graph` deliberately knows no filters. It knows how to read a
//! description, how to wire pads together, and — for auto-conversion — the two
//! *names* `scale` and `aresample`. Everything else arrives through this trait,
//! which is what lets the DSL be tested against `vaco-filter-core`'s mock
//! filters long before a filter library exists.

use vaco_core::MediaType;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{Filter, FilterDesc, Pad};

use crate::ast::Arg;

/// A request to instantiate one filter.
#[derive(Debug)]
pub struct Instantiate<'a> {
    /// The registered filter name, without any `@id`.
    pub name: &'a str,
    /// The instance name this node will carry, e.g. `Parsed_scale_1` or
    /// `scale@big`.
    pub instance: &'a str,
    /// The argument text as the graph scanner decoded it, before the `:` split.
    /// `None` when the filter was written with no `=`.
    pub args: Option<&'a str>,
    /// The same text split into positional and `key=value` arguments, with the
    /// values **still escaped** at the option level.
    pub arguments: &'a [Arg],
}

impl Instantiate<'_> {
    /// The value of a named argument, unescaped.
    #[must_use]
    pub fn named(&self, key: &str) -> Option<String> {
        self.arguments
            .iter()
            .find(|a| a.key.as_deref() == Some(key))
            .map(Arg::value)
    }

    /// The `n`th positional argument, unescaped.
    #[must_use]
    pub fn positional(&self, n: usize) -> Option<String> {
        self.arguments
            .iter()
            .filter(|a| a.key.is_none())
            .nth(n)
            .map(Arg::value)
    }
}

/// One instantiated filter, ready to be added to a [`Graph`](vaco_filter_core::Graph).
///
/// `desc` and `formats` must agree on pad counts: the scheduler takes pad
/// *media types* from the descriptor and pad *counts* from the format sets, and
/// a disagreement shows up much later as an unconnectable pad. The builder
/// checks it at instantiation instead.
pub struct Instance {
    pub desc: FilterDesc,
    pub formats: NodeFormats,
    pub filter: Box<dyn Filter>,
}

impl core::fmt::Debug for Instance {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Instance")
            .field("desc", &self.desc.name)
            .field("inputs", &self.formats.inputs.len())
            .field("outputs", &self.formats.outputs.len())
            .finish_non_exhaustive()
    }
}

/// Where filters come from.
pub trait FilterRegistry {
    /// Every registered name, used for the "did you mean" suggestion.
    fn names(&self) -> Vec<&str>;

    /// Whether `name` is registered.
    fn contains(&self, name: &str) -> bool {
        self.names().contains(&name)
    }

    /// Instantiate a filter.
    ///
    /// Pad counts that depend on options (`amix=inputs=3`, `split=4`) are
    /// realised here, by returning a `desc` whose pad slices and a `formats`
    /// whose vectors are both that long — see [`pads`].
    ///
    /// # Errors
    ///
    /// A message describing what was wrong with the arguments. It is rendered
    /// under the filter's span.
    fn create(&self, req: &Instantiate<'_>) -> Result<Instance, String>;
}

/// Static pad slices for filters whose pad count depends on their options.
///
/// [`FilterDesc`] is `Copy + 'static` and carries `&'static [Pad]`, so a filter
/// that realises `N` pads at instantiation has nowhere to put them — the gap
/// `vaco-filter-core` records as signature gap 8. A subslice of a static array
/// closes it for every realistic graph without changing a frozen signature.
pub mod pads {
    use super::{MediaType, Pad};

    /// The largest dynamic pad count these slices can express.
    ///
    /// The reference allows far more (`amix` accepts up to 32767 inputs), but a
    /// static table that large would cost most of a megabyte for a case nobody
    /// writes. A registry that needs more supplies its own slice; this is a
    /// convenience, not a limit of the design.
    pub const MAX: usize = 64;

    const VIDEO: Pad = Pad {
        name: "dynamic",
        media_type: MediaType::Video,
    };
    const AUDIO: Pad = Pad {
        name: "dynamic",
        media_type: MediaType::Audio,
    };

    static VIDEO_PADS: [Pad; MAX] = [VIDEO; MAX];
    static AUDIO_PADS: [Pad; MAX] = [AUDIO; MAX];

    /// `n` video pads, or `None` if `n > MAX`.
    #[must_use]
    pub fn video(n: usize) -> Option<&'static [Pad]> {
        VIDEO_PADS.get(..n)
    }

    /// `n` audio pads, or `None` if `n > MAX`.
    #[must_use]
    pub fn audio(n: usize) -> Option<&'static [Pad]> {
        AUDIO_PADS.get(..n)
    }

    /// `n` pads of `media`, or `None` if `n > MAX`.
    #[must_use]
    pub fn of(media: MediaType, n: usize) -> Option<&'static [Pad]> {
        match media {
            MediaType::Audio => audio(n),
            _ => video(n),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn dynamic_pads_have_the_requested_length_and_media() {
        let p = pads::video(5).unwrap();
        assert_eq!(p.len(), 5);
        assert!(p.iter().all(|p| p.media_type == MediaType::Video));
        assert_eq!(pads::audio(0).map(<[Pad]>::len), Some(0));
        assert!(pads::video(pads::MAX.saturating_add(1)).is_none());
    }
}
