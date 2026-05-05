//! Secret vault sub-commands and session helpers.

use crate::config::{logs_dir, vault_path};
use eyre::{Result, WrapErr};
use evo_policy::{Redactor, Vault};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// read_secret_from_stdin
// ---------------------------------------------------------------------------

pub(crate) async fn read_secret_from_stdin() -> Result<String> {
    use std::io::Write as _;
    print!("  paste value (input is echoed; clear scrollback after): ");
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    let v = buf.trim().to_string();
    if v.is_empty() {
        return Err(eyre::eyre!("empty value — aborted"));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// secret_add
// ---------------------------------------------------------------------------

pub(crate) async fn secret_add(name: &str, from_stdin: bool, value: Option<String>) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(eyre::eyre!("name must be [A-Za-z0-9_-]+"));
    }
    let raw = match (from_stdin, value) {
        (true, _) => read_secret_from_stdin().await?,
        (false, Some(v)) => v,
        (false, None) => return Err(eyre::eyre!("either pass a value or use --stdin")),
    };
    let path = vault_path()?;
    let mut vault = Vault::load(&path).await.unwrap_or_default();
    vault.upsert(name, &raw);
    vault.save(&path).await?;
    let entry = vault
        .get(name)
        .ok_or_else(|| eyre::eyre!("upsert vanished"))?;
    println!(
        "stored '{name}' (kind={}, fingerprint={}) at {}",
        entry.kind,
        entry.fingerprint,
        path.display()
    );
    println!("  the model will never see the raw value — only ${{SECRET:{name}}}");
    Ok(())
}

// ---------------------------------------------------------------------------
// secret_list
// ---------------------------------------------------------------------------

pub(crate) async fn secret_list() -> Result<()> {
    let vault = Vault::load(&vault_path()?).await.unwrap_or_default();
    if vault.entries.is_empty() {
        println!("(vault is empty — try `evoclaw secret add NAME --stdin`)");
        return Ok(());
    }
    println!("{:<24} {:<14} {:<10} CREATED", "NAME", "KIND", "FINGER");
    for e in vault.list() {
        println!(
            "{:<24} {:<14} {:<10} {}",
            e.name,
            e.kind,
            e.fingerprint,
            e.created_at.format("%Y-%m-%d %H:%M")
        );
    }
    println!(
        "\n(values are stored at {} — chmod 600)",
        vault_path()?.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// secret_remove
// ---------------------------------------------------------------------------

pub(crate) async fn secret_remove(name: &str) -> Result<()> {
    let path = vault_path()?;
    let mut vault = Vault::load(&path).await.unwrap_or_default();
    if !vault.remove(name) {
        return Err(eyre::eyre!("no such secret: {name}"));
    }
    vault.save(&path).await?;
    println!("removed '{name}'");
    Ok(())
}

// ---------------------------------------------------------------------------
// secret_test
// ---------------------------------------------------------------------------

pub(crate) async fn secret_test(input: &str) -> Result<()> {
    let vault = Vault::load(&vault_path()?).await.unwrap_or_default();
    let r = Redactor::from_vault(&vault);
    let (out, hits) = r.scrub(input);
    println!("input  : {input}");
    println!("output : {out}");
    println!("hits   : {hits} substitution(s)");
    Ok(())
}

// ---------------------------------------------------------------------------
// most_recent_session
// ---------------------------------------------------------------------------

pub(crate) async fn most_recent_session() -> Result<PathBuf> {
    let dir = logs_dir()?;
    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .wrap_err_with(|| format!("read {}", dir.display()))?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Some(entry) = entries.next_entry().await? {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let meta = entry.metadata().await?;
        let mtime = meta.modified()?;
        if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            newest = Some((mtime, p));
        }
    }
    newest
        .map(|(_, p)| p)
        .ok_or_else(|| eyre::eyre!("no JSONL sessions in {}", dir.display()))
}
