//! Onboarding and provider wizard commands.

use crate::config::{config_path, ensure_layout};
use crate::onboard;
use evo_providers::AuthMethod;
use eyre::Result;
use onboard::ProviderChoice;

// ---------------------------------------------------------------------------
// onboard_cmd
// ---------------------------------------------------------------------------

pub(crate) async fn onboard_cmd() -> Result<()> {
    use std::io::Write as _;

    let cfg_path = config_path()?;
    let already = cfg_path.exists();
    if already {
        println!("config.toml exists at {}", cfg_path.display());
        print!("Overwrite with the wizard? [y/N] ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            println!("kept existing config; run `evoclaw login` to switch provider only.");
            return Ok(());
        }
    }
    ensure_layout().await?;
    run_provider_wizard().await?;
    println!();
    println!("Done. Run `evoclaw` (no args) to enter the interactive shell.");
    Ok(())
}

// ---------------------------------------------------------------------------
// login_cmd
// ---------------------------------------------------------------------------

pub(crate) async fn login_cmd() -> Result<()> {
    ensure_layout().await?;
    run_provider_wizard().await?;
    println!();
    println!("Login complete. Resume with `evoclaw`.");
    Ok(())
}

// ---------------------------------------------------------------------------
// run_provider_wizard
// ---------------------------------------------------------------------------

pub(crate) async fn run_provider_wizard() -> Result<()> {
    let mut choice = onboard::pick_provider().await?;
    // ACP-prefixed providers (advanced "evoclaw agent add" path) keep the
    // legacy flow — they're not part of the new 3-way auth picker.
    if choice.id.starts_with("acp:") {
        let cfg_path = onboard::save_config(&choice).await?;
        println!("  saved config -> {}", cfg_path.display());
        return Ok(());
    }
    let auth = onboard::pick_auth_method(&choice)?;
    match auth {
        AuthMethod::ApiKey => {
            let key_opt = onboard::ask_api_key(&choice).await?;
            if let Some(ref key) = key_opt {
                let path = onboard::save_secret(&choice.id, key).await?;
                println!("  saved key    -> {}", path.display());
            }
            onboard::pick_model(&mut choice, key_opt.as_deref()).await?;
            let cfg_path = onboard::save_config_with_auth(&choice, AuthMethod::ApiKey).await?;
            println!("  saved config -> {}", cfg_path.display());
        }
        AuthMethod::Browser => {
            let profile = onboard::capture_browser_profile(&choice).await?;
            let path = onboard::save_browser_profile(&profile).await?;
            println!("  saved browser profile -> {}", path.display());
            onboard::pick_model(&mut choice, Some(&profile.session_token)).await?;
            let cfg_path = onboard::save_config_with_auth(&choice, AuthMethod::Browser).await?;
            println!("  saved config -> {}", cfg_path.display());
        }
        AuthMethod::Acp => {
            let agent_id = onboard::provider_to_acp_agent(&choice.id).ok_or_else(|| {
                eyre::eyre!("No ACP agent available for provider '{}'", choice.id)
            })?;

            let agent_profile = evo_acp_client::find_agent(agent_id)
                .ok_or_else(|| eyre::eyre!("ACP agent '{}' not found in catalog", agent_id))?;

            let agent_config = evo_acp_client::AgentConfig::from_profile(agent_profile);
            let agent_path = evo_acp_client::save_agent(&agent_config)
                .await
                .map_err(|e| eyre::eyre!("save agent {}: {e}", agent_id))?;

            println!();
            println!("  ✓ saved ACP agent profile -> {}", agent_path.display());
            println!("    Agent: {}", agent_profile.name);
            println!(
                "    Command: {} {}",
                agent_config.command,
                agent_config.args.join(" ")
            );
            println!("    Install: {}", agent_profile.install_hint);
            println!("    Auth: {}", agent_profile.auth_hint);

            let acp_choice = ProviderChoice {
                id: format!("acp:{}", agent_id),
                name: agent_profile.name.clone(),
                base_url: String::new(),
                default_model: format!("acp:{}", agent_id),
                fallback: Vec::new(),
                key_url: None,
                local: true,
            };

            let cfg_path = onboard::save_config(&acp_choice).await?;
            println!("  saved config -> {}", cfg_path.display());

            println!();
            println!("  Testing ACP agent connection...");

            match test_acp_connection(&agent_config).await {
                Ok(_) => {
                    println!("  ✓ Connection test PASSED");
                    println!("  ✓ ACP agent '{}' is ready to use", agent_profile.name);
                }
                Err(e) => {
                    println!("  ✗ Connection test FAILED: {}", e);
                    println!();
                    println!("  Troubleshooting:");
                    println!(
                        "    1. Check if the agent is installed: {}",
                        agent_profile.install_hint
                    );
                    println!("    2. Verify authentication: {}", agent_profile.auth_hint);
                    println!("    3. Try running the command manually:");
                    println!(
                        "       {} {}",
                        agent_config.command,
                        agent_config.args.join(" ")
                    );
                    println!();
                    println!("  Configuration saved but connection failed.");
                    println!("  Run `evoclaw doctor` to diagnose or retry with `evoclaw login`.");
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// test_acp_connection
// ---------------------------------------------------------------------------

/// Test ACP agent connection by spawning the agent and performing a handshake.
/// Returns `Ok(())` if connection succeeds, `Err` with details if it fails.
/// Uses a timeout to avoid hanging on unresponsive agents.
pub(crate) async fn test_acp_connection(agent_config: &evo_acp_client::AgentConfig) -> Result<()> {
    use tokio::time::{timeout, Duration};

    let client = evo_acp_client::AcpClient::new();

    let spawn_result = timeout(Duration::from_secs(30), client.spawn(agent_config)).await;

    match spawn_result {
        Ok(Ok(())) => {
            // Spawn succeeded — proceed to initialize handshake
        }
        Ok(Err(e)) => {
            return Err(eyre::eyre!("spawn failed: {}", e));
        }
        Err(_) => {
            drop(client);
            return Err(eyre::eyre!(
                "spawn timed out after 30s. Agent may need installation or user input."
            ));
        }
    }

    let init_result = timeout(
        Duration::from_secs(30),
        client.initialize("evoclaw-test", env!("CARGO_PKG_VERSION")),
    )
    .await;

    match init_result {
        Ok(Ok(result)) => {
            if let Some(info) = result.get("serverInfo") {
                println!("  Server info: {}", info);
            }
            client.shutdown().await.ok();
            Ok(())
        }
        Ok(Err(e)) => {
            client.shutdown().await.ok();
            Err(eyre::eyre!("initialize handshake failed: {}", e))
        }
        Err(_) => {
            client.shutdown().await.ok();
            Err(eyre::eyre!(
                "initialize timed out after 30s. Agent may require authentication first."
            ))
        }
    }
}
