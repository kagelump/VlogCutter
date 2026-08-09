//! Both dev-only agent seams must be absent from a shipped binary.
//!
//! The markers are spelled as byte arrays so that this test file does not
//! itself contain the strings it greps for — otherwise a compiler that
//! interned the literal from the test would make the check pass for the wrong
//! reason.
//!
//! Each seam is a remote-access surface: the eval bridge evaluates arbitrary
//! JavaScript in the running webview, and the media diagnostics endpoint
//! reports full media paths. Both are `#[cfg(debug_assertions)]` and both need
//! an explicit environment opt-in on top of that; this proves the first half.

use std::fs;

/// `RCDVLOG_EVAL_BRIDGE_COMPILED`
const EVAL_BRIDGE_MARKER: &[u8] = &[
    82, 67, 68, 86, 76, 79, 71, 95, 69, 86, 65, 76, 95, 66, 82, 73, 68, 71, 69, 95, 67, 79, 77, 80,
    73, 76, 69, 68,
];

/// `RCDVLOG_MEDIA_DEBUG_COMPILED`
const MEDIA_DEBUG_MARKER: &[u8] = &[
    82, 67, 68, 86, 76, 79, 71, 95, 77, 69, 68, 73, 65, 95, 68, 69, 66, 85, 71, 95, 67, 79, 77, 80,
    73, 76, 69, 68,
];

#[test]
fn dev_only_agent_seams_are_absent_from_the_release_binary() {
    let binary = fs::read(env!("CARGO_BIN_EXE_rcdvlog-desktop")).expect("read desktop binary");

    for (name, marker) in [
        ("eval bridge", EVAL_BRIDGE_MARKER),
        ("media diagnostics", MEDIA_DEBUG_MARKER),
    ] {
        let present = binary.windows(marker.len()).any(|window| window == marker);
        if cfg!(debug_assertions) {
            assert!(
                present,
                "the debug binary should contain the {name} marker, or the release check proves nothing"
            );
        } else {
            assert!(!present, "the release binary contains the dev-only {name}");
        }
    }
}
