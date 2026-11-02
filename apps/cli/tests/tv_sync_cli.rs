use serde_json::Value;
use std::process::Command;
#[test]
fn sync_preview_is_catalog_free_versioned_and_rejects_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.json");
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../../core/rcdvlog-core/tests/fixtures/tv-review-sync/manual-v1.json"
    ))
    .unwrap();
    std::fs::write(&path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_rcdvlog"))
            .args(["tv-review", "sync-preview", "--input"])
            .arg(&path)
            .output()
            .unwrap()
    };
    let out = invoke();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["command"], "tv-review.sync-preview");
    assert_eq!(result["data"]["slices"][0]["state"], "estimated");
    fixture["request"]["schemaVersion"] = serde_json::json!(2);
    std::fs::write(&path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    let out = invoke();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stderr).unwrap()["ok"],
        false
    );
    fixture["request"]["schemaVersion"] = serde_json::json!(1);
    fixture["request"]["guessOffset"] = serde_json::json!(0);
    std::fs::write(&path, serde_json::to_vec(&fixture).unwrap()).unwrap();
    assert!(!invoke().status.success());
}
