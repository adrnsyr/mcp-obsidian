//! mcp-obsidian — MCP server untuk menulis, membaca, dan memetakan memori
//! per-project ke dalam sebuah Obsidian Vault.

mod cluster;
mod config;
mod docs;
// Tanpa fitur `semantic`, helper index di `embed` sengaja tak terpakai
// (hanya dipakai jalur ber-feature) — bungkam dead-code khusus build itu.
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
    // PENTING: log harus ke stderr — stdout dipakai protokol MCP (JSON-RPC).
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
        "mcp-obsidian dimulai"
    );

    let server = ObsidianServer::new(config.clone());

    // Bila fitur `watch` aktif, pantau folder memori untuk auto-regen _MOC.md
    // saat memori diedit langsung di Obsidian. Guard harus hidup selama server.
    #[cfg(feature = "watch")]
    let _watch_guard = match watcher::spawn(config.clone(), server.io_lock()) {
        Ok(g) => Some(g),
        Err(e) => {
            tracing::warn!("file watcher gagal dimulai: {e}");
            None
        }
    };

    // Jalankan server di atas transport stdio dan tunggu sampai selesai.
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
