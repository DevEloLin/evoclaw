//! Skill, playbook, and memory sub-commands.

use crate::config::{memory_dir, playbooks_dir, skills_dir};
use crate::playbook::Playbook;
use crate::task::run_one_shot;
use evo_core::{Memory, MemoryLayer, Skill, SkillTree};
use eyre::{Result, WrapErr};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// skill_list
// ---------------------------------------------------------------------------

pub(crate) async fn skill_list() -> Result<()> {
    let dir = skills_dir()?;
    if !dir.exists() {
        println!("(no skills yet — run a task first)");
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(&dir).await?;
    println!(
        "{:<24} {:<10} {:>5} {:<8} NAME",
        "ID", "STATE", "SCORE", "VER"
    );
    while let Some(entry) = entries.next_entry().await? {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        match Skill::load_yaml(&p).await {
            Ok(sk) => println!(
                "{:<24} {:<10} {:>5.2} v{:<7} {}",
                sk.id,
                format!("{:?}", sk.state).to_uppercase(),
                sk.score,
                sk.version,
                sk.name
            ),
            Err(e) => println!("ERR {}: {e}", p.display()),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// skill_show
// ---------------------------------------------------------------------------

pub(crate) async fn skill_show(id: &str) -> Result<()> {
    let path = skills_dir()?.join(format!("{id}.yaml"));
    let content = tokio::fs::read_to_string(&path)
        .await
        .wrap_err_with(|| format!("read {}", path.display()))?;
    println!("{content}");
    Ok(())
}

// ---------------------------------------------------------------------------
// skill_tree
// ---------------------------------------------------------------------------

pub(crate) async fn skill_tree() -> Result<()> {
    let dir = skills_dir()?;
    let tree = SkillTree::rebuild_from_dir(&dir).await?;
    let index_path = SkillTree::default_index_path(&dir);
    tree.save(&index_path).await?;
    println!("{}", tree.render_tree());
    println!("(index: {})", index_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// playbook_list — user-authored playbooks under ~/.evoclaw/playbooks/
// ---------------------------------------------------------------------------

pub(crate) async fn playbook_list() -> Result<()> {
    let dir = playbooks_dir()?;
    let pbs = Playbook::list_dir(&dir).await?;
    if pbs.is_empty() {
        println!("(no playbooks in {})", dir.display());
        println!("Drop a *.yaml or *.md file there with `id:`, `name:`, `steps:` fields.");
        return Ok(());
    }
    println!("{:<28} {:<6} {}", "ID", "PARAMS", "NAME");
    for pb in pbs {
        println!("{:<28} {:<6} {}", pb.id, pb.parameters.len(), pb.name);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// playbook_show — print a playbook's resolved fields
// ---------------------------------------------------------------------------

pub(crate) async fn playbook_show(id: &str) -> Result<()> {
    let dir = playbooks_dir()?;
    let pb = Playbook::find(&dir, id)
        .await
        .wrap_err_with(|| format!("playbook '{id}' under {}", dir.display()))?;
    println!("id:          {}", pb.id);
    println!("name:        {}", pb.name);
    println!("description: {}", pb.description);
    println!("parameters:");
    for p in &pb.parameters {
        let req = if p.required { "required" } else { "optional" };
        let ex = p.example.as_deref().unwrap_or("");
        println!("  - {} ({req}) — {} [example: {ex}]", p.name, p.description);
    }
    println!("steps:\n{}", pb.steps);
    if let Some(n) = &pb.notes {
        println!("notes:\n{n}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// skill_run — execute a playbook as a one-shot task
// ---------------------------------------------------------------------------

pub(crate) async fn skill_run(id: &str, params: Vec<String>) -> Result<()> {
    let dir = playbooks_dir()?;
    let pb = Playbook::find(&dir, id)
        .await
        .wrap_err_with(|| format!("playbook '{id}' under {}", dir.display()))?;
    let mut provided: HashMap<String, String> = HashMap::new();
    for p in &params {
        match p.split_once('=') {
            Some((k, v)) => {
                provided.insert(k.trim().to_string(), v.to_string());
            }
            None => {
                return Err(eyre::eyre!(
                    "bad --param '{p}' — expected k=v (e.g. --param out_dir=/tmp)"
                ));
            }
        }
    }
    let rendered = pb
        .render_prompt(&provided)
        .map_err(|e| eyre::eyre!("{e}"))?;
    run_one_shot(&rendered).await
}

// ---------------------------------------------------------------------------
// memory_search
// ---------------------------------------------------------------------------

pub(crate) async fn memory_search(query: &str, limit: usize) -> Result<()> {
    let mem = Memory::at(memory_dir()?);
    let hits = mem
        .search(
            query,
            &[MemoryLayer::L1, MemoryLayer::L2, MemoryLayer::L3],
            limit,
        )
        .await?;
    if hits.is_empty() {
        println!("(no matches for '{query}')");
        return Ok(());
    }
    for r in hits {
        println!(
            "[{:?}] {} (conf={:.2}, src={})\n  {}",
            r.layer, r.id, r.confidence, r.source, r.content
        );
    }
    Ok(())
}
