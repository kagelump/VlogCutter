//! Frame-pack behaviour against a real proxy.
//!
//! Ignored by default: needs a generated proxy. Run with
//! `RCDVLOG_PROXY=<path to a .v3.mp4> cargo test -p rcdvlog-core --test framepack_smoke -- --ignored --nocapture`.

use std::path::PathBuf;

#[test]
#[ignore = "requires a real generated proxy"]
fn a_real_proxy_yields_a_deterministic_frame_pack() {
    let Ok(proxy) = std::env::var("RCDVLOG_PROXY") else {
        panic!("set RCDVLOG_PROXY to a generated proxy file");
    };
    let proxy = PathBuf::from(proxy);

    let first = rcdvlog_core::build_frame_pack(&proxy, "fp-1").expect("build frame pack");
    let second = rcdvlog_core::build_frame_pack(&proxy, "fp-1").expect("rebuild");

    println!("selected: {:?}", first.timestamps_ms);
    println!("count: {}", first.timestamps_ms.len());
    assert_eq!(first, second, "frame-pack selection must be deterministic");
    assert!(!first.timestamps_ms.is_empty());
    assert!(first.timestamps_ms.windows(2).all(|p| p[0] < p[1]));
}
