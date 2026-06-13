//! mcp-obsidian — MCP server for writing, reading, and mapping per-project
//! memories into an Obsidian Vault.

mod cluster;
mod config;
mod docs;
// Without the `semantic` feature, the index helper in `embed` is intentionally
// unused (only used on the feature-gated path) — silence dead-code for that build.
#[cfg_attr(not(feature = "semantic"), allow(dead_code))]
mod embed;
mod links;
mod mapping;
mod memory;
mod project;
mod prompts;
mod recall;
mod resources;
mod server;
mod similarity;
mod watcher;

use config::Config;
use rmcp::transport::stdio;
use rmcp::ServiceExt;
use server::ObsidianServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // IMPORTANT: logs must go to stderr — stdout is used by the MCP protocol (JSON-RPC).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = Config::from_env()?;
    tracing::info!(
        vault = %config.vault_path.display(),
        memory_root = %config.memory_root,
        docs_root = %config.docs_root,
        "mcp-obsidian started"
    );

    let server = ObsidianServer::new(config.clone());

    // When the `watch` feature is enabled, watch the memory folder to auto-regenerate
    // _MOC.md when a memory is edited directly in Obsidian. The guard must live as long
    // as the server.
    #[cfg(feature = "watch")]
    let _watch_guard = match watcher::spawn(config.clone(), server.io_lock()) {
        Ok(g) => Some(g),
        Err(e) => {
            tracing::warn!("file watcher failed to start: {e}");
            None
        }
    };

    // Run the server over the stdio transport and wait until it finishes.
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
