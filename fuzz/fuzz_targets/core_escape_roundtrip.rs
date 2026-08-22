//! Filtergraph and option-value escaping.
//!
//! Three nested escaping levels make this genuinely easy to get wrong, and a
//! mistake corrupts a user's filter description silently rather than failing.
#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_core::escape::{self, Mode, FILTERGRAPH_SPECIAL, OPT_VALUE_SPECIAL};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else { return };
    for special in [OPT_VALUE_SPECIAL, FILTERGRAPH_SPECIAL] {
        for mode in [Mode::Auto, Mode::Backslash, Mode::Quote] {
            let escaped = escape::escape(s, special, mode);
            match escape::unescape(&escaped) {
                Ok(back) => assert_eq!(
                    back, s,
                    "escape/unescape not identity ({mode:?}, {special:?}) for {s:?}"
                ),
                Err(e) => panic!("our own output failed to unescape ({mode:?}): {e:?}"),
            }
        }
    }
});
