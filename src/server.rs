//! Definisi MCP server beserta tools-nya.

use crate::cluster;
use crate::config::Config;
use crate::embed;
use crate::links;
use crate::mapping::regenerate_moc;
use crate::memory::{self, WriteInput};
use crate::project::{self, resolve_project, slugify};
use crate::prompts::{self, MemoryBrief};
use crate::recall;
use crate::resources;
use crate::similarity;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolResult, Content, GetPromptRequestParams, GetPromptResult,
    ListPromptsResult, ListResourcesResult, PaginatedRequestParams, Prompt, PromptArgument,
    PromptMessage, PromptMessageRole, RawResource, ReadResourceRequestParams, ReadResourceResult,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;

const INSTRUCTIONS: &str = "\
Server memori berbasis Obsidian Vault. Setiap memori adalah satu catatan Markdown \
dengan frontmatter (name, description, tags, type, links) dan body. Memori \
dikelompokkan per-project; bila argumen 'project' tidak diberikan, project \
dideteksi dari working directory. Gunakan memory_write untuk menyimpan, \
memory_read/memory_search/memory_list untuk mengambil, memory_map untuk \
meregenerasi peta (_MOC.md), memory_suggest untuk usulan relasi pintar \
(berdasarkan kemiripan tag & isi), memory_backlinks untuk melihat siapa yang \
menaut sebuah memori, memory_doctor untuk memeriksa broken link & orphan, \
memory_cluster untuk mengelompokkan memori menjadi tema (komunitas graf), \
memory_semantic_search untuk pencarian berdasarkan makna (bila fitur 'semantic' \
aktif), memory_recall untuk mengambil paket konteks terpadu sebuah topik \
(semantic + isi + graf + tema dalam satu panggilan), dan memory_delete untuk \
menghapus.";

#[derive(Clone)]
pub struct ObsidianServer {
    config: Config,
    // Dipakai oleh macro `#[tool_handler]` lewat ekspansi, bukan dibaca langsung
    // di kode kita — analisis dead-code tidak melihatnya.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    /// Lock global yang menserialkan semua operasi tulis/baca. rmcp memproses
    /// request secara konkuren, sementara `memory_write` melakukan
    /// read-modify-write (membaca file lama untuk mempertahankan `created`, lalu
    /// menulis ulang) dan setelahnya meregenerasi `_MOC.md` dengan memindai
    /// seluruh folder. Tanpa lock, dua write ke memori yang sama bisa balapan
    /// (created hilang) dan regenerasi peta bisa membaca folder yang sedang
    /// setengah jadi. Lock global ini sederhana & cukup untuk beban single-user.
    io_lock: Arc<Mutex<()>>,
}

// ---- Argumen tiap tool ----

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct WriteArgs {
    /// Nama project. Kosongkan untuk deteksi otomatis dari working directory.
    #[serde(default)]
    pub project: Option<String>,
    /// Nama/judul memori (akan dijadikan slug, sekaligus nama file).
    pub name: String,
    /// Ringkasan satu baris tentang isi memori ini.
    pub description: String,
    /// Isi memori (Markdown). Boleh memuat [[wikilink]] ke memori lain.
    pub body: String,
    /// Tag untuk pengelompokan (opsional).
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Kategori memori: project | reference | decision | note (default: note).
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Slug memori lain yang terkait (opsional), dirender sebagai link di peta.
    #[serde(default)]
    pub links: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ReadArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
    /// Nama/slug memori yang ingin dibaca.
    pub name: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SearchArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
    /// Kata kunci pencarian (cocokkan ke name/description/tags/body).
    #[serde(default)]
    pub query: Option<String>,
    /// Filter berdasarkan satu tag (opsional).
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MapArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DeleteArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
    /// Nama/slug memori yang ingin dihapus.
    pub name: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SuggestArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
    /// Slug memori tertentu. Bila kosong, beri saran untuk SEMUA memori.
    #[serde(default)]
    pub name: Option<String>,
    /// Maksimum jumlah saran per memori (default 5).
    #[serde(default)]
    pub top: Option<usize>,
    /// Skor kemiripan minimum 0.0–1.0 (default 0.05).
    #[serde(default)]
    pub threshold: Option<f64>,
    /// Bila true DAN `name` diisi: tuliskan saran ke field `links` memori itu
    /// (union, tanpa duplikat) lalu regenerasi peta.
    #[serde(default)]
    pub apply: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DoctorArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ClusterArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SemanticSearchArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
    /// Kueri pencarian dalam bahasa alami (dicari berdasarkan MAKNA, bukan kata).
    pub query: String,
    /// Maksimum jumlah hasil (default 5).
    #[serde(default)]
    pub top: Option<usize>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RecallArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
    /// Kueri/topik dalam bahasa alami untuk diingat kembali.
    pub query: String,
    /// Berapa memori teratas yang diambil & diperkaya (default 3).
    #[serde(default)]
    pub top: Option<usize>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct BacklinksArgs {
    /// Nama project (opsional, auto-detect bila kosong).
    #[serde(default)]
    pub project: Option<String>,
    /// Slug memori yang ingin dilihat backlink-nya (siapa yang menautnya).
    pub name: String,
}

fn err(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

#[tool_router]
impl ObsidianServer {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            tool_router: Self::tool_router(),
            io_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Lock I/O bersama, agar file watcher (fitur `watch`) bisa menserialkan
    /// regenerasi peta terhadap operasi tulis tool.
    #[cfg(feature = "watch")]
    pub fn io_lock(&self) -> Arc<Mutex<()>> {
        self.io_lock.clone()
    }

    /// Resolve project lalu regenerasi peta setelah perubahan.
    fn project_of(&self, explicit: Option<&str>) -> Result<String, McpError> {
        resolve_project(&self.config, explicit).map_err(err)
    }

    #[tool(
        description = "Simpan (buat atau perbarui) satu memori ke Obsidian Vault. \
        Saat memperbarui, timestamp 'created' dipertahankan dan peta project \
        diregenerasi otomatis."
    )]
    async fn memory_write(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let outcome = memory::write_memory(
            &self.config,
            &project,
            WriteInput {
                name: args.name,
                description: args.description,
                body: args.body,
                tags: args.tags.unwrap_or_default(),
                kind: args.kind,
                links: args.links.unwrap_or_default(),
            },
        )
        .map_err(err)?;

        regenerate_moc(&self.config, &project).map_err(err)?;

        let verb = if outcome.created {
            "Dibuat"
        } else {
            "Diperbarui"
        };
        let text = format!(
            "{verb} memori '{}' di project '{}'.\nPath: {}",
            outcome.slug,
            project,
            outcome.path.display()
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Baca isi lengkap satu memori (frontmatter + body).")]
    async fn memory_read(
        &self,
        Parameters(args): Parameters<ReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let mem = memory::read_memory(&self.config, &project, &args.name).map_err(err)?;
        let text = mem.to_file_string().map_err(err)?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Daftar ringkas semua memori dalam satu project (JSON).")]
    async fn memory_list(
        &self,
        Parameters(args): Parameters<ListArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let entries = memory::list_entries(&self.config, &project);
        let json = serde_json::to_string_pretty(&entries).map_err(err)?;
        let header = format!("Project '{project}' — {} memori:\n", entries.len());
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(description = "Cari memori dalam satu project berdasarkan kata kunci \
        dan/atau tag. Hasil terurut berdasarkan relevansi (JSON).")]
    async fn memory_search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let hits = memory::search(
            &self.config,
            &project,
            args.query.as_deref(),
            args.tag.as_deref(),
        );
        let json = serde_json::to_string_pretty(&hits).map_err(err)?;
        let header = format!("Project '{project}' — {} hasil:\n", hits.len());
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(description = "Regenerasi peta memori (_MOC.md) untuk satu project: \
        dikelompokkan per kategori & tag, plus relasi antar-memori. \
        Kembalikan isi peta.")]
    async fn memory_map(
        &self,
        Parameters(args): Parameters<MapArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let content = regenerate_moc(&self.config, &project).map_err(err)?;
        Ok(CallToolResult::success(vec![Content::text(content)]))
    }

    #[tool(
        description = "Sarankan relasi pintar antar-memori berdasarkan kemiripan \
        tag (Jaccard) + isi (cosine TF-IDF). Tanpa 'name': saran untuk semua \
        memori. Dengan 'name' + 'apply: true': tuliskan saran ke field links \
        memori tersebut. Mengembalikan JSON saran (name, score, shared_tags, \
        shared_terms)."
    )]
    async fn memory_suggest(
        &self,
        Parameters(args): Parameters<SuggestArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let top = args.top.unwrap_or(similarity::DEFAULT_TOP_N);
        let threshold = args.threshold.unwrap_or(similarity::DEFAULT_THRESHOLD);
        let memories = memory::load_all(&self.config, &project);
        // Bila fitur `semantic` aktif, pakai embedding (by makna) untuk komponen
        // isi; bila tidak, `None` → fallback TF-IDF.
        let emb = embed::vectors_for(&self.config, &project, &memories);

        match args.name {
            // ---- Saran untuk satu memori (opsional apply ke links) ----
            Some(name) => {
                let slug = slugify(&name);
                let suggestions =
                    similarity::suggest_for_ext(&slug, &memories, top, threshold, emb.as_ref());

                let mut applied: Vec<String> = Vec::new();
                if args.apply.unwrap_or(false) && !suggestions.is_empty() {
                    let mem = memory::read_memory(&self.config, &project, &slug).map_err(err)?;
                    let mut links = mem.front.links.clone();
                    for s in &suggestions {
                        if !links.contains(&s.name) {
                            links.push(s.name.clone());
                            applied.push(s.name.clone());
                        }
                    }
                    if !applied.is_empty() {
                        memory::write_memory(
                            &self.config,
                            &project,
                            WriteInput {
                                name: mem.front.name.clone(),
                                description: mem.front.description.clone(),
                                body: mem.body.clone(),
                                tags: mem.front.tags.clone(),
                                kind: Some(mem.front.kind.clone()),
                                links,
                            },
                        )
                        .map_err(err)?;
                        regenerate_moc(&self.config, &project).map_err(err)?;
                    }
                }

                let json = serde_json::to_string_pretty(&suggestions).map_err(err)?;
                let header = if applied.is_empty() {
                    format!(
                        "Saran untuk '{slug}' di project '{project}' — {} kandidat:\n",
                        suggestions.len()
                    )
                } else {
                    format!(
                        "Ditautkan ke '{slug}': {}.\nSaran lengkap ({} kandidat):\n",
                        applied.join(", "),
                        suggestions.len()
                    )
                };
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "{header}{json}"
                ))]))
            }
            // ---- Saran untuk seluruh project ----
            None => {
                let all = similarity::suggest_all_ext(&memories, top, threshold, emb.as_ref());
                let json = serde_json::to_string_pretty(&all).map_err(err)?;
                let header = format!(
                    "Saran relasi project '{project}' — {} memori punya kandidat:\n",
                    all.len()
                );
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "{header}{json}"
                ))]))
            }
        }
    }

    #[tool(description = "Periksa kesehatan graf memori sebuah project: temukan \
        broken link (tautan ke memori yang tidak ada, baik dari field links \
        maupun [[wikilink]] di body) dan orphan (memori tanpa relasi masuk/keluar). \
        Read-only. Mengembalikan JSON laporan.")]
    async fn memory_doctor(
        &self,
        Parameters(args): Parameters<DoctorArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let memories = memory::load_all(&self.config, &project);
        let report = links::doctor(&memories);
        let json = serde_json::to_string_pretty(&report).map_err(err)?;
        let header = format!(
            "Project '{project}': {} memori, {} broken link, {} orphan.\n",
            report.total,
            report.broken_links.len(),
            report.orphans.len()
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(
        description = "Tampilkan backlink sebuah memori: daftar memori lain yang \
        menautnya (via field links atau [[wikilink]] di body). Backlink dihitung \
        dari graf, tidak disimpan ke file. Read-only."
    )]
    async fn memory_backlinks(
        &self,
        Parameters(args): Parameters<BacklinksArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let slug = slugify(&args.name);
        let memories = memory::load_all(&self.config, &project);
        let graph = links::LinkGraph::build(&memories);
        let back = graph.backlinks_of(&slug);
        let json = serde_json::to_string_pretty(&back).map_err(err)?;
        let header = format!(
            "'{slug}' ditaut oleh {} memori di project '{project}':\n",
            back.len()
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(
        description = "Kelompokkan memori sebuah project menjadi 'tema' via deteksi \
        komunitas Louvain pada graf tautan (links + [[wikilink]]). Mengembalikan \
        JSON: nilai modularity + daftar klaster (anggota per tema). Read-only."
    )]
    async fn memory_cluster(
        &self,
        Parameters(args): Parameters<ClusterArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let memories = memory::load_all(&self.config, &project);
        // Bila fitur `semantic` aktif, perkaya graf dengan edge kemiripan
        // embedding sebelum Louvain; bila tidak, klaster berbasis tautan saja.
        let emb = embed::vectors_for(&self.config, &project, &memories);
        let result = cluster::cluster_ext(&memories, emb.as_ref(), cluster::DEFAULT_SIM_THRESHOLD);
        let json = serde_json::to_string_pretty(&result).map_err(err)?;
        let header = format!(
            "Project '{project}': {} tema (modularity {:.3}).\n",
            result.clusters.len(),
            result.modularity
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(
        description = "Pencarian SEMANTIK: temukan memori berdasarkan MAKNA kueri, \
        bukan kecocokan kata (mis. 'alasan memilih bahasa' menemukan memori soal \
        'kenapa pakai Rust'). Memakai embedding lokal; index di-cache per project \
        & di-update otomatis untuk memori yang berubah. Catatan: hanya tersedia \
        bila server dibangun dengan fitur 'semantic'."
    )]
    async fn memory_semantic_search(
        &self,
        Parameters(args): Parameters<SemanticSearchArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let top = args.top.unwrap_or(5);
        let memories = memory::load_all(&self.config, &project);
        let hits = embed::semantic_search(&self.config, &project, &args.query, top, &memories)
            .map_err(err)?;
        let json = serde_json::to_string_pretty(&hits).map_err(err)?;
        let header = format!(
            "Pencarian semantik '{}' di project '{project}' — {} hasil:\n",
            args.query,
            hits.len()
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(
        description = "RECALL konteks terpadu untuk satu topik: cari memori paling \
        relevan secara semantik, lalu untuk tiap hasil sertakan isi penuh + \
        tautan keluar + backlink + memori setema. Satu panggilan menggantikan \
        rangkaian semantic_search→read→backlinks→cluster, menghasilkan paket \
        konteks siap pakai. Catatan: butuh fitur 'semantic'."
    )]
    async fn memory_recall(
        &self,
        Parameters(args): Parameters<RecallArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let top = args.top.unwrap_or(3);
        let result = recall::recall(&self.config, &project, &args.query, top).map_err(err)?;
        let json = serde_json::to_string_pretty(&result).map_err(err)?;
        let header = format!(
            "Recall '{}' di project '{project}' — {} memori:\n",
            args.query,
            result.items.len()
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(description = "Hapus satu memori dari project, lalu regenerasi peta.")]
    async fn memory_delete(
        &self,
        Parameters(args): Parameters<DeleteArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let removed = memory::delete_memory(&self.config, &project, &args.name).map_err(err)?;
        if removed {
            regenerate_moc(&self.config, &project).map_err(err)?;
        }
        let text = if removed {
            format!("Memori '{}' dihapus dari project '{}'.", args.name, project)
        } else {
            format!(
                "Memori '{}' tidak ditemukan di project '{}'.",
                args.name, project
            )
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }
}

#[tool_handler]
impl ServerHandler for ObsidianServer {
    fn get_info(&self) -> ServerInfo {
        let projects = project::list_projects(&self.config);
        let extra = if projects.is_empty() {
            String::new()
        } else {
            format!("\n\nProject yang sudah ada: {}.", projects.join(", "))
        };
        // `ServerInfo` bersifat #[non_exhaustive], jadi mulai dari default lalu
        // ubah field yang diperlukan (bukan dengan struct literal).
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        info.instructions = Some(format!("{INSTRUCTIONS}{extra}"));
        info
    }

    // ---- Resources: tiap memori = resource `memory://<project>/<slug>` ----

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let mut out = Vec::new();
        for project in project::list_projects(&self.config) {
            // peta project
            out.push(
                RawResource::new(
                    resources::uri_for(&project, "_MOC"),
                    format!("{project} / peta"),
                )
                .with_description(format!("Peta memori (MOC) project {project}"))
                .with_mime_type(resources::MIME_MARKDOWN)
                .no_annotation(),
            );
            // tiap memori
            for entry in memory::list_entries(&self.config, &project) {
                out.push(
                    RawResource::new(
                        resources::uri_for(&project, &entry.name),
                        format!("{project} / {}", entry.name),
                    )
                    .with_description(entry.description)
                    .with_mime_type(resources::MIME_MARKDOWN)
                    .no_annotation(),
                );
            }
        }
        let result = ListResourcesResult {
            resources: out,
            ..Default::default()
        };
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let r = resources::parse_uri(&request.uri).ok_or_else(|| {
            McpError::invalid_params(format!("URI resource tidak valid: {}", request.uri), None)
        })?;

        // `_MOC` dibaca dari file peta; selain itu baca file memori biasa.
        let text = if r.slug == "_MOC" {
            std::fs::read_to_string(self.config.moc_file(&r.project)).map_err(|e| {
                McpError::invalid_params(format!("peta '{}' tidak ditemukan: {e}", r.project), None)
            })?
        } else {
            let mem = memory::read_memory(&self.config, &r.project, &r.slug).map_err(err)?;
            mem.to_file_string().map_err(err)?
        };

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            text,
            request.uri,
        )
        .with_mime_type(resources::MIME_MARKDOWN)]))
    }

    // ---- Prompts: template siap-pakai untuk bekerja dengan memori ----

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let prompts = prompts::CATALOG
            .iter()
            .map(|spec| {
                let args: Vec<PromptArgument> = spec
                    .arguments
                    .iter()
                    .map(|(name, desc, required)| {
                        PromptArgument::new(*name)
                            .with_description(*desc)
                            .with_required(*required)
                    })
                    .collect();
                Prompt::new(spec.name, Some(spec.description), Some(args))
            })
            .collect();
        let result = ListPromptsResult {
            prompts,
            ..Default::default()
        };
        Ok(result)
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let _guard = self.io_lock.lock().await;
        // argumen `project` opsional → resolve seperti tool lain.
        let arg_project = request
            .arguments
            .as_ref()
            .and_then(|m| m.get("project"))
            .and_then(|v| v.as_str());
        let project = self.project_of(arg_project)?;

        let briefs: Vec<MemoryBrief> = memory::list_entries(&self.config, &project)
            .into_iter()
            .map(|e| MemoryBrief {
                name: e.name,
                kind: e.kind,
                description: e.description,
            })
            .collect();

        let text = match request.name.as_str() {
            prompts::names::SUMMARIZE_PROJECT => prompts::render_summarize(&project, &briefs),
            prompts::names::ONBOARD => prompts::render_onboard(&project, &briefs),
            prompts::names::REVIEW_DECISIONS => {
                let decisions: Vec<MemoryBrief> = briefs
                    .into_iter()
                    .filter(|b| b.kind == "decision")
                    .collect();
                prompts::render_review_decisions(&project, &decisions)
            }
            other => {
                return Err(McpError::invalid_params(
                    format!("prompt tidak dikenal: {other}"),
                    None,
                ))
            }
        };

        let mut result =
            GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, text)]);
        result.description = Some(format!(
            "Prompt '{}' untuk project '{project}'",
            request.name
        ));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CNT: AtomicU64 = AtomicU64::new(0);

    fn tmp_server() -> ObsidianServer {
        let n = CNT.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mcpobs-srv-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        ObsidianServer::new(Config {
            vault_path: dir,
            memory_root: "memory".into(),
            default_project: Some("test".into()),
        })
    }

    /// Banyak write konkuren ke memori yang berbeda harus menghasilkan file
    /// valid (frontmatter utuh, tepat 1 baris `created:`) dan `_MOC.md` yang
    /// konsisten — peta tidak boleh kehilangan entri karena diregenerasi
    /// bersamaan dengan write lain.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writes_distinct_files() {
        let srv = tmp_server();
        const N: usize = 32;

        let mut handles = Vec::new();
        for i in 0..N {
            let s = srv.clone();
            handles.push(tokio::spawn(async move {
                s.memory_write(Parameters(WriteArgs {
                    project: Some("demo".into()),
                    name: format!("Memo {i}"),
                    description: format!("deskripsi {i}"),
                    body: format!("isi memori nomor {i}"),
                    tags: Some(vec![format!("t{i}")]),
                    kind: None,
                    links: None,
                }))
                .await
                .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        for i in 0..N {
            let path = srv.config.memory_file("demo", &format!("memo-{i}"));
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("file hilang: {}", path.display()));
            let created = raw.lines().filter(|l| l.starts_with("created:")).count();
            assert_eq!(created, 1, "file korup ({}):\n{raw}", path.display());
            let mem = crate::memory::Memory::from_file_string(&raw)
                .unwrap_or_else(|e| panic!("gagal parse {}: {e}\n{raw}", path.display()));
            assert_eq!(mem.front.name, format!("memo-{i}"));
        }
        // _MOC.md harus memuat seluruh 32 entri (regenerasi tidak kehilangan data).
        let moc = std::fs::read_to_string(srv.config.moc_file("demo")).unwrap();
        for i in 0..N {
            assert!(
                moc.contains(&format!("[[memo-{i}]]")),
                "MOC kehilangan memo-{i}"
            );
        }
    }

    /// Inilah race yang sebenarnya dijaga oleh `io_lock`: banyak write konkuren
    /// ke SLUG yang sama. Tiap write membaca file lama untuk mempertahankan
    /// `created`, lalu menulis ulang. Tanpa serialisasi, langkah ini bisa balapan
    /// dan menghasilkan frontmatter ganda/rusak. Dengan lock, hasil akhir tetap
    /// satu file valid dengan tepat 1 baris `created:`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writes_same_slug() {
        let srv = tmp_server();
        const N: usize = 24;

        let mut handles = Vec::new();
        for i in 0..N {
            let s = srv.clone();
            handles.push(tokio::spawn(async move {
                s.memory_write(Parameters(WriteArgs {
                    project: Some("demo".into()),
                    name: "Same Note".into(),
                    description: format!("revisi {i}"),
                    body: format!("body {i}"),
                    tags: None,
                    kind: None,
                    links: None,
                }))
                .await
                .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let path = srv.config.memory_file("demo", "same-note");
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            raw.lines().filter(|l| l.starts_with("created:")).count(),
            1,
            "frontmatter rusak akibat race:\n{raw}"
        );
        let mem = crate::memory::Memory::from_file_string(&raw).unwrap();
        assert_eq!(mem.front.name, "same-note");
    }

    #[tokio::test]
    async fn write_then_map_lists_memory() {
        let srv = tmp_server();
        srv.memory_write(Parameters(WriteArgs {
            project: Some("demo".into()),
            name: "Hello".into(),
            description: "world".into(),
            body: "isi".into(),
            tags: Some(vec!["x".into()]),
            kind: Some("note".into()),
            links: None,
        }))
        .await
        .unwrap();

        let moc = std::fs::read_to_string(srv.config.moc_file("demo")).unwrap();
        assert!(moc.contains("[[hello]]"), "MOC tidak memuat memori:\n{moc}");
        assert!(moc.contains("#x"), "MOC tidak memuat indeks tag:\n{moc}");
    }
}
