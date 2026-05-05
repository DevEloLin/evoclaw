use crate::onboard::picker::ProviderChoice;
use eyre::Result;

/// After the API key is captured, fetch `<base_url>/models` and let the user
/// pick one of the models the key actually entitles them to.
///
/// - Local providers (Ollama / vLLM / llama.cpp) skip the network probe and
///   keep `profile.default_model`.
/// - ACP providers (`acp:*`) skip — model is irrelevant; the upstream CLI
///   manages that itself.
/// - On any error (no network, 401, parsing) we print one line and keep the
///   catalog default. This step is **best-effort**: never fail the wizard.
pub async fn pick_model(profile: &mut ProviderChoice, api_key: Option<&str>) -> Result<()> {
    use std::io::Write;

    if profile.local || profile.id.starts_with("acp:") || profile.base_url.is_empty() {
        return Ok(());
    }
    let url = format!("{}/models", profile.base_url.trim_end_matches('/'));
    let mut req = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| eyre::eyre!("build http client: {e}"))?
        .get(&url);
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    println!();
    println!("  fetching available models from {url} …");
    let models = match req.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<ModelList>().await {
            Ok(list) => list.data,
            Err(e) => {
                eprintln!(
                    "  (could not parse /models response: {e}; keeping default '{}')",
                    profile.default_model
                );
                return Ok(());
            }
        },
        Ok(resp) => {
            eprintln!(
                "  (provider returned HTTP {}; keeping default '{}')",
                resp.status(),
                profile.default_model
            );
            return Ok(());
        }
        Err(e) => {
            eprintln!(
                "  (could not reach {url}: {e}; keeping default '{}')",
                profile.default_model
            );
            return Ok(());
        }
    };
    if models.is_empty() {
        eprintln!(
            "  (provider returned 0 models; keeping default '{}')",
            profile.default_model
        );
        return Ok(());
    }
    println!();
    println!(
        "  Available models ({} total). Type a number, or press Enter for the default.",
        models.len()
    );
    let preview: Vec<&ModelEntry> = models.iter().take(30).collect();
    for (i, m) in preview.iter().enumerate() {
        let marker = if m.id == profile.default_model {
            " (default)"
        } else {
            ""
        };
        println!("    {:>2})  {}{}", i + 1, m.id, marker);
    }
    if models.len() > preview.len() {
        println!(
            "    … ({} more models hidden — type the model id directly to pick one)",
            models.len() - preview.len()
        );
    }
    println!();
    print!("  model [{}]: ", profile.default_model);
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        println!("  → keeping default '{}'", profile.default_model);
        return Ok(());
    }
    if let Ok(n) = line.parse::<usize>() {
        if let Some(m) = preview.get(n.checked_sub(1).unwrap_or(usize::MAX)) {
            profile.default_model = m.id.clone();
            println!("  → selected '{}'", profile.default_model);
            return Ok(());
        }
    }
    // user typed a model id directly
    if models.iter().any(|m| m.id == line) {
        profile.default_model = line.to_string();
        println!("  → selected '{}'", profile.default_model);
    } else {
        println!(
            "  (no exact match for '{line}'; keeping default '{}')",
            profile.default_model
        );
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct ModelEntry {
    id: String,
}
