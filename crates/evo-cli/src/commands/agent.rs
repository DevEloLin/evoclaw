//! ACP agent sub-commands.

use eyre::Result;

// ---------------------------------------------------------------------------
// agent_catalog
// ---------------------------------------------------------------------------

pub(crate) fn agent_catalog() {
    println!("== Available external agents ==");
    println!("  [native] = speaks Zed Agent Client Protocol out of the box");
    println!(
        "  [shim]   = does NOT speak Zed ACP; needs a custom shim or use a different provider"
    );
    println!();
    for p in evo_acp_client::catalog() {
        let badge = if p.acp_native { "[native]" } else { "[shim]  " };
        println!("  {} {:<10} {}", badge, p.id, p.name);
        println!("    install: {}", p.install_hint);
        println!("    auth   : {}", p.auth_hint);
        println!("    note   : {}", p.notes);
    }
    let paths = evo_acp_client::registry_paths();
    println!();
    println!("add one with: evoclaw agent add <id>");
    if let Some(p) = paths.user_full {
        println!(
            "customise:    write {} (full override) or drop *.json into {} (per-id patches)",
            p.display(),
            paths
                .user_patch_dir
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
        );
    }
}

// ---------------------------------------------------------------------------
// agent_list
// ---------------------------------------------------------------------------

pub(crate) async fn agent_list() -> Result<()> {
    let agents = evo_acp_client::list_agents()
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    if agents.is_empty() {
        println!("(no agents configured — try `evoclaw agent catalog`)");
        return Ok(());
    }
    println!("== Configured ACP agents ==");
    for a in agents {
        let badge = match evo_acp_client::find_agent(&a.id) {
            Some(p) if p.acp_native => "[native]",
            Some(_) => "[shim]  ",
            None => "[custom]",
        };
        println!(
            "  {} {:<10} bin={} args={:?}",
            badge, a.id, a.command, a.args
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// agent_add
// ---------------------------------------------------------------------------

pub(crate) async fn agent_add(id: &str) -> Result<()> {
    let prof = evo_acp_client::find_agent(id).ok_or_else(|| {
        eyre::eyre!("unknown agent '{id}' — run `evoclaw agent catalog` to see options")
    })?;
    let cfg = evo_acp_client::AgentConfig::from_profile(prof);
    let path = evo_acp_client::save_agent(&cfg)
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    println!("added agent '{id}' -> {}", path.display());
    println!("  install : {}", prof.install_hint);
    println!("  auth    : {}", prof.auth_hint);
    println!("  test it : evoclaw agent test {id}");
    Ok(())
}

// ---------------------------------------------------------------------------
// agent_remove
// ---------------------------------------------------------------------------

pub(crate) async fn agent_remove(id: &str) -> Result<()> {
    evo_acp_client::remove_agent(id)
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    println!("removed agent '{id}'");
    Ok(())
}

// ---------------------------------------------------------------------------
// agent_test
// ---------------------------------------------------------------------------

pub(crate) async fn agent_test(id: &str) -> Result<()> {
    let cfg = evo_acp_client::load_agent(id)
        .await
        .map_err(|e| eyre::eyre!("{e:#}; did you `evoclaw agent add {id}` first?"))?;
    println!("-> spawning '{}' ({} {:?})", cfg.id, cfg.command, cfg.args);
    let client = evo_acp_client::AcpClient::new();
    client.spawn(&cfg).await.map_err(|e| {
        eyre::eyre!(
            "{e}; install with: {}",
            evo_acp_client::find_agent(id)
                .map(|p| p.install_hint.as_str())
                .unwrap_or("see catalog")
        )
    })?;
    let result = client
        .initialize("evoclaw", env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|e| eyre::eyre!("initialize failed: {e}"))?;
    println!("initialize OK");
    if let Some(info) = result.get("serverInfo") {
        println!("  serverInfo: {}", info);
    }
    client.shutdown().await.ok();
    Ok(())
}
