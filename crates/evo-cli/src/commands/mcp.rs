//! MCP server sub-commands.

use eyre::Result;

// ---------------------------------------------------------------------------
// mcp_catalog
// ---------------------------------------------------------------------------

pub(crate) fn mcp_catalog() {
    println!("== Available MCP servers ==");
    for p in evo_mcp_client::catalog() {
        println!("  {:<14} {}", p.id, p.name);
        println!("    desc   : {}", p.description);
        println!("    install: {}", p.install_hint);
        if !p.auth_env.is_empty() {
            println!("    env    : {}", p.auth_env.join(", "));
        }
    }
    let paths = evo_mcp_client::registry_paths();
    println!();
    println!("add one with: evoclaw mcp add <id>");
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
// mcp_list
// ---------------------------------------------------------------------------

pub(crate) async fn mcp_list() -> Result<()> {
    let servers = evo_mcp_client::list_servers()
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    if servers.is_empty() {
        println!("(no MCP servers — try `evoclaw mcp catalog`)");
        return Ok(());
    }
    println!("== Configured MCP servers ==");
    for s in servers {
        println!("  {:<14} cmd={} args={:?}", s.id, s.command, s.args);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// mcp_add
// ---------------------------------------------------------------------------

pub(crate) async fn mcp_add(id: &str) -> Result<()> {
    let prof = evo_mcp_client::find_server(id)
        .ok_or_else(|| eyre::eyre!("unknown server '{id}' — run `evoclaw mcp catalog`"))?;
    let mut cfg = evo_mcp_client::ServerConfig::from_profile(prof);
    if !prof.auth_env.is_empty() {
        for var in &prof.auth_env {
            if let Ok(v) = std::env::var(var) {
                cfg.env.push((var.clone(), v));
            }
        }
    }
    let path = evo_mcp_client::save_server(&cfg)
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    println!("added MCP server '{id}' -> {}", path.display());
    println!("  install: {}", prof.install_hint);
    if !prof.auth_env.is_empty() {
        let captured: Vec<&String> = cfg.env.iter().map(|(k, _)| k).collect();
        let missing: Vec<&String> = prof
            .auth_env
            .iter()
            .filter(|v| !captured.iter().any(|c| c.as_str() == v.as_str()))
            .collect();
        if !missing.is_empty() {
            println!("  missing env vars: {:?}", missing);
            println!("  set them and re-run `evoclaw mcp add {id}` to capture");
        }
    }
    println!("  test it: evoclaw mcp test {id}");
    Ok(())
}

// ---------------------------------------------------------------------------
// mcp_remove
// ---------------------------------------------------------------------------

pub(crate) async fn mcp_remove(id: &str) -> Result<()> {
    evo_mcp_client::remove_server(id)
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    println!("removed MCP server '{id}'");
    Ok(())
}

// ---------------------------------------------------------------------------
// mcp_test
// ---------------------------------------------------------------------------

pub(crate) async fn mcp_test(id: &str) -> Result<()> {
    let cfg = evo_mcp_client::load_server(id)
        .await
        .map_err(|e| eyre::eyre!("{e:#}; did you `evoclaw mcp add {id}` first?"))?;
    println!("-> spawning '{}' ({} {:?})", cfg.id, cfg.command, cfg.args);
    let client = evo_mcp_client::McpClient::new();
    client.spawn(&cfg).await.map_err(|e| eyre::eyre!("{e}"))?;
    let _ = client
        .initialize("evoclaw", env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|e| eyre::eyre!("initialize failed: {e}"))?;
    println!("initialize OK");
    let tools = client
        .list_tools()
        .await
        .map_err(|e| eyre::eyre!("tools/list failed: {e}"))?;
    println!("  exposed tools: {}", tools.len());
    for t in tools.iter().take(20) {
        println!(
            "    - {} : {}",
            t.name,
            t.description.lines().next().unwrap_or("")
        );
    }
    client.shutdown().await.ok();
    Ok(())
}
