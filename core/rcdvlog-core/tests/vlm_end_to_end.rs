//! Opt-in live project batch for clip-analysis acceptance.

use anyhow::{Result, anyhow, bail};
use rcdvlog_core::Catalog;

#[test]
#[ignore = "live project/OpenRouter batch"]
fn analyzes_a_real_project_within_the_explicit_budget() -> Result<()> {
    if std::env::var("RCDVLOG_LIVE_VLM").as_deref() != Ok("1") {
        bail!("set RCDVLOG_LIVE_VLM=1 to acknowledge live OpenRouter spend");
    }
    let catalog_path = std::env::var("RCDVLOG_CATALOG")?;
    let project_id = std::env::var("RCDVLOG_PROJECT_ID")?;
    let budget = std::env::var("RCDVLOG_VLM_MAX_COST_USD")
        .unwrap_or_else(|_| "1.0".to_string())
        .parse::<f64>()?;
    let catalog = Catalog::open_existing(catalog_path)?;
    let report = catalog.analyze_missing(&project_id, false)?;
    if report.cost_usd > budget {
        return Err(anyhow!(
            "analysis spent ${:.6}, above ${budget:.2} budget",
            report.cost_usd
        ));
    }
    eprintln!("{}", serde_json::to_string(&report)?);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    Ok(())
}
