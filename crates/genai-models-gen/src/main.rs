// genai-models-gen — dev tool.
//
// Fetches `models.list` from the Gemini API and reports a diff against the
// bundled `genai-cli/src/models/data.toml`. Prints a curation report to
// stderr; never writes to the registry file.
//
// Usage:
//   GEMINI_API_KEY=... cargo run -p genai-models-gen
//
// Optional flags:
//   --api-base <url>   override API base (default: generativelanguage.googleapis.com)
//   --json             emit machine-readable JSON instead of human report

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

const BUNDLED_TOML: &str = include_str!("../../genai-cli/src/models/data.toml");

#[derive(Debug, Deserialize)]
struct Bundled {
    #[serde(default)]
    models: Vec<BundledEntry>,
}

#[derive(Debug, Deserialize)]
struct BundledEntry {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    context_window: u32,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ApiList {
    #[serde(default)]
    models: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiModel {
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    input_token_limit: Option<u32>,
    #[serde(default)]
    output_token_limit: Option<u32>,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
    #[serde(default)]
    description: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut api_base = "https://generativelanguage.googleapis.com".to_string();
    let mut json_out = false;
    let mut iter = args.iter().skip(1);
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--api-base" => {
                api_base = iter
                    .next()
                    .cloned()
                    .context("--api-base requires a value")?;
            }
            "--json" => json_out = true,
            "-h" | "--help" => {
                println!("usage: genai-models-gen [--api-base URL] [--json]");
                return Ok(());
            }
            other => bail!("unknown arg: {other}"),
        }
    }

    let api_key = std::env::var("GEMINI_API_KEY")
        .context("set GEMINI_API_KEY to fetch the live model list")?;

    let bundled: Bundled = toml::from_str(BUNDLED_TOML).context("parsing bundled data.toml")?;
    let bundled_ids: BTreeMap<String, &BundledEntry> =
        bundled.models.iter().map(|m| (m.id.clone(), m)).collect();

    let live = fetch_models(&api_base, &api_key).await?;
    let live_ids: BTreeSet<String> = live
        .models
        .iter()
        .map(|m| strip_model_prefix(&m.name).to_string())
        .collect();

    let mut new_ids: Vec<&str> = live_ids
        .iter()
        .filter(|id| !bundled_ids.contains_key(*id))
        .map(|s| s.as_str())
        .collect();
    new_ids.sort();

    let mut deprecated_ids: Vec<&str> = bundled_ids
        .keys()
        .filter(|id| !live_ids.contains(*id))
        .map(|s| s.as_str())
        .collect();
    deprecated_ids.sort();

    let mut changed: Vec<(String, Vec<String>)> = Vec::new();
    for (id, b) in &bundled_ids {
        if let Some(api) = live
            .models
            .iter()
            .find(|m| strip_model_prefix(&m.name) == id)
        {
            let diffs = diff_fields(b, api);
            if !diffs.is_empty() {
                changed.push((id.clone(), diffs));
            }
        }
    }

    if json_out {
        let report = serde_json::json!({
            "new": new_ids,
            "deprecated": deprecated_ids,
            "changed": changed,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        eprintln!("== models registry diff ==");
        eprintln!("bundled: {}, live: {}", bundled_ids.len(), live_ids.len());
        eprintln!();
        if new_ids.is_empty() && deprecated_ids.is_empty() && changed.is_empty() {
            eprintln!("Registry is in sync. Nothing to do.");
            return Ok(());
        }
        if !new_ids.is_empty() {
            eprintln!("New models (consider adding to data.toml):");
            for id in &new_ids {
                let meta = live.models.iter().find(|m| strip_model_prefix(&m.name) == *id);
                let name = meta.and_then(|m| m.display_name.as_deref()).unwrap_or("");
                let ctx = meta.and_then(|m| m.input_token_limit).unwrap_or(0);
                eprintln!("  + {id:<40}  {name:<40}  ctx={ctx}");
            }
            eprintln!();
        }
        if !deprecated_ids.is_empty() {
            eprintln!("Bundled but not in live list (may be deprecated):");
            for id in &deprecated_ids {
                eprintln!("  - {id}");
            }
            eprintln!();
        }
        if !changed.is_empty() {
            eprintln!("Changed entries:");
            for (id, diffs) in &changed {
                eprintln!("  ~ {id}");
                for d in diffs {
                    eprintln!("      {d}");
                }
            }
        }
    }

    Ok(())
}

async fn fetch_models(base: &str, api_key: &str) -> Result<ApiList> {
    let url = format!("{base}/v1beta/models");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("x-goog-api-key", api_key)
        .send()
        .await
        .context("GET /v1beta/models")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("models.list {status}: {body}");
    }
    let parsed: ApiList = resp.json().await.context("parsing models.list response")?;
    Ok(parsed)
}

fn strip_model_prefix(s: &str) -> &str {
    s.strip_prefix("models/").unwrap_or(s)
}

fn diff_fields(b: &BundledEntry, a: &ApiModel) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(api_ctx) = a.input_token_limit {
        if b.context_window != 0 && b.context_window != api_ctx {
            out.push(format!(
                "context_window: bundled={} api={api_ctx}",
                b.context_window
            ));
        }
    }
    // Note: we don't infer a 'chat' capability from generateContent support —
    // image, TTS, music, and embedding models all expose generateContent for
    // their respective output modalities. The bundled `capabilities` list is
    // authoritative.
    if let Some(api_name) = &a.display_name {
        if !api_name.is_empty() && b.status.as_deref() == Some("deprecated") {
            out.push(format!("api still lists '{api_name}' but bundled status is deprecated"));
        }
    }
    out
}
