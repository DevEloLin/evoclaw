//! Skill and memory sub-commands.

use crate::config::{memory_dir, skills_dir};
use evo_core::{Memory, MemoryLayer, Skill, SkillTree};
use eyre::{Result, WrapErr};

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
