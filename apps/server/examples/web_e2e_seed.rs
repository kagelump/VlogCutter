use anyhow::Result;
use rcdvlog_server_core::{
    BaselineEnvelope, Catalog, ControlCatalog, ProjectAssetAddedPayload, ServerConfig,
};

fn main() -> Result<()> {
    let config = ServerConfig::from_environment()?;
    let control = ControlCatalog::open_or_create(&config.data_dir, &config.diagnostics())?;
    let mac_catalog_path = config.data_dir.join("web-e2e-mac.sqlite3");
    let mac = Catalog::open(&mac_catalog_path)?;
    mac.set_local_node_id("node_web_e2e_mac")?;
    let project = mac.create_project("Chrome concurrency fixture")?;
    mac.add_portable_project_asset(
        &project.id,
        ProjectAssetAddedPayload {
            id: "asset_web_e2e".into(),
            fingerprint: "fingerprint-web-e2e".into(),
            filename: "chrome-concurrency.mov".into(),
            size_bytes: 10,
            duration_seconds: 1.0,
            frame_rate: 30.0,
            width: 1920,
            height: 1080,
            codec: "h264".into(),
            camera: "Fixture camera".into(),
            captured_at_raw: None,
            timestamp_candidates: None,
            best_time: None,
            metadata_rotation_degrees: 0,
            default_rotation_degrees: 0,
            created_at: "2026-07-28T00:00:00Z".into(),
        },
    )?;
    let baseline = BaselineEnvelope::export_catalog(&mac, &project.id)?;
    control.project_replicas().import_baseline(&baseline)?;
    println!("{}", project.id);
    Ok(())
}
