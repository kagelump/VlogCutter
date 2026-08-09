use std::fs;

#[test]
fn eval_bridge_code_is_absent_from_release_binary() {
    let binary = fs::read(env!("CARGO_BIN_EXE_rcdvlog-desktop")).expect("read desktop binary");
    let marker = [
        82, 67, 68, 86, 76, 79, 71, 95, 69, 86, 65, 76, 95, 66, 82, 73, 68, 71, 69, 95, 67, 79, 77,
        80, 73, 76, 69, 68,
    ];

    if cfg!(debug_assertions) {
        assert!(
            binary.windows(marker.len()).any(|window| window == marker),
            "debug binary should contain the eval bridge marker so the release check is meaningful"
        );
    } else {
        assert!(
            !binary.windows(marker.len()).any(|window| window == marker),
            "release binary contains the dev-only eval bridge"
        );
    }
}
