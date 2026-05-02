//! Compact binary `evo` — alias for `evoclaw`. Same logic, shorter name.

#[tokio::main]
async fn main() -> eyre::Result<()> {
    evo_cli::entry().await
}
