//! Long-form binary `evoclaw` — type the project name to enter the runtime.

#[tokio::main]
async fn main() -> eyre::Result<()> {
    evo_cli::entry().await
}
