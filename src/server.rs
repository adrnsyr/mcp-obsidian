//! MCP server definition along with its tools.

use crate::cluster;
use crate::config::Config;
use crate::docs::{self, DocInput, WriteMode};
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
Obsidian Vault-based memory server. Each memory is a single Markdown note \
with frontmatter (name, description, tags, type, links) and a body. Memories \
are grouped per-project; if the 'project' argument is not provided, the project \
is detected from the working directory. Use memory_write to store, \
memory_read/memory_search/memory_list to retrieve, memory_map to \
regenerate the map (_MOC.md), memory_suggest for smart relation suggestions \
(based on tag & content similarity), memory_backlinks to see who \
links to a memory, memory_doctor to check for broken links & orphans, \
memory_cluster to group memories into themes (graph communities), \
memory_semantic_search to search by meaning (when the 'semantic' feature is \
enabled), memory_recall to fetch a unified context package for a topic \
(semantic + content + graph + theme in a single call), memory_hybrid_search for \
combined keyword+meaning search, memory_link to add/remove links \
without rewriting the body, memory_rename to rename a memory while \
updating all inbound links, and memory_delete to delete. \
For long documents (spec/runbook/brainstorm/worklog) use the doc_* family: \
doc_write/doc_append to write, doc_read to read, doc_list/doc_search \
to find, doc_rename to rename, doc_delete to delete. \
Documents are stored in a separate folder and are DELIBERATELY not \
indexed into the graph/semantic/MOC, so use memories for atomic, \
interlinked facts, and documents for long notes read/edited by humans.";

#[derive(Clone)]
pub struct ObsidianServer {
    config: Config,
    // Used by the `#[tool_handler]` macro via expansion, not read directly
    // in our code — dead-code analysis doesn't see it.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    /// Global lock that serializes all write/read operations. rmcp processes
    /// requests concurrently, while `memory_write` performs a
    /// read-modify-write (reading the old file to preserve `created`, then
    /// rewriting) and afterwards regenerates `_MOC.md` by scanning the
    /// entire folder. Without the lock, two writes to the same memory could race
    /// (losing `created`) and map regeneration could read a folder that is
    /// half-finished. This global lock is simple & sufficient for single-user load.
    io_lock: Arc<Mutex<()>>,
}

// ---- Arguments for each tool ----

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct WriteArgs {
    /// Project name. Leave empty to auto-detect from the working directory.
    #[serde(default)]
    pub project: Option<String>,
    /// Memory name/title (will be turned into a slug and the file name).
    pub name: String,
    /// One-line summary of this memory's content.
    pub description: String,
    /// Memory content (Markdown). May contain [[wikilink]]s to other memories.
    pub body: String,
    /// Tags for grouping (optional).
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Memory category: project | reference | decision | note (default: note).
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Slugs of other related memories (optional), rendered as links in the map.
    #[serde(default)]
    pub links: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ReadArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Name/slug of the memory to read.
    pub name: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SearchArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Search keyword (matched against name/description/tags/body).
    #[serde(default)]
    pub query: Option<String>,
    /// Filter by a single tag (optional).
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MapArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DeleteArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Name/slug of the memory to delete.
    pub name: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SuggestArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Slug of a specific memory. If empty, suggest for ALL memories.
    #[serde(default)]
    pub name: Option<String>,
    /// Maximum number of suggestions per memory (default 5).
    #[serde(default)]
    pub top: Option<usize>,
    /// Minimum similarity score 0.0–1.0 (default 0.05).
    #[serde(default)]
    pub threshold: Option<f64>,
    /// If true AND `name` is provided: write the suggestions to that memory's
    /// `links` field (union, no duplicates) then regenerate the map.
    #[serde(default)]
    pub apply: Option<bool>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DoctorArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ClusterArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SemanticSearchArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Natural-language search query (searched by MEANING, not words).
    pub query: String,
    /// Maximum number of results (default 5).
    #[serde(default)]
    pub top: Option<usize>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RecallArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Natural-language query/topic to recall.
    pub query: String,
    /// How many top memories to fetch & enrich (default 3).
    #[serde(default)]
    pub top: Option<usize>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct BacklinksArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Slug of the memory whose backlinks to view (who links to it).
    pub name: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct LinkArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Slug of the memory whose `links` field will be modified.
    pub name: String,
    /// Slugs to add to `links` (optional).
    #[serde(default)]
    pub add: Option<Vec<String>>,
    /// Slugs to remove from `links` (optional).
    #[serde(default)]
    pub remove: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RenameArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Slug of the memory to rename.
    pub name: String,
    /// New name/slug.
    pub new_name: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct HybridSearchArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Search query (combined: keyword match + meaning).
    pub query: String,
    /// Maximum number of results (default 5).
    #[serde(default)]
    pub top: Option<usize>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DocWriteArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Document name/title (turned into a slug & file name).
    pub name: String,
    /// Document type: spec | runbook | brainstorm | worklog (default note).
    /// Determines the initial template & default mode when 'mode' is not set.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// One-line title (stored as description). Optional.
    #[serde(default)]
    pub title: Option<String>,
    /// Document content (Markdown). Empty on a new document → use the kind template.
    #[serde(default)]
    pub body: Option<String>,
    /// Tags for grouping (optional).
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Write mode: "overwrite" or "append". Empty → default per-kind
    /// (brainstorm/worklog = append, others = overwrite).
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DocAppendArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Document name/slug. Created automatically from a template if it doesn't exist.
    pub name: String,
    /// Document type when a new document is created (e.g. worklog). Optional if
    /// the document already exists (the existing type is preserved).
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Content to append (will be given a timestamped heading).
    pub body: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DocReadArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Name/slug of the document to read.
    pub name: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DocListArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Filter by document type (optional).
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DocSearchArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Search keyword (matched against name/description/tags/body).
    #[serde(default)]
    pub query: Option<String>,
    /// Filter by document type (optional).
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DocDeleteArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Name/slug of the document to delete.
    pub name: String,
}

#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DocRenameArgs {
    /// Project name (optional, auto-detected when empty).
    #[serde(default)]
    pub project: Option<String>,
    /// Slug of the document to rename.
    pub name: String,
    /// New name/slug.
    pub new_name: String,
}

/// Cosine threshold for flagging two memories as near-duplicates in doctor.
const NEAR_DUP_THRESHOLD: f32 = 0.88;

fn err(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

/// Determine the document write mode: an explicit argument wins; if empty, use
/// the per-kind default (append for brainstorm/worklog, overwrite for the rest).
fn resolve_mode(mode: Option<&str>, kind: &str) -> anyhow::Result<WriteMode> {
    match mode {
        Some(m) => match m.trim().to_lowercase().as_str() {
            "overwrite" => Ok(WriteMode::Overwrite),
            "append" => Ok(WriteMode::Append),
            other => {
                anyhow::bail!("unknown mode: '{other}' (use 'overwrite' or 'append')")
            }
        },
        None => Ok(docs::doc_kind(kind)
            .map(|k| k.default_mode)
            .unwrap_or(WriteMode::Overwrite)),
    }
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

    /// Shared I/O lock, so the file watcher (the `watch` feature) can serialize
    /// map regeneration against tool write operations.
    #[cfg(feature = "watch")]
    pub fn io_lock(&self) -> Arc<Mutex<()>> {
        self.io_lock.clone()
    }

    /// Resolve the project, then regenerate the map after changes.
    fn project_of(&self, explicit: Option<&str>) -> Result<String, McpError> {
        resolve_project(&self.config, explicit).map_err(err)
    }

    #[tool(
        description = "Store (create or update) a single memory in the Obsidian Vault. \
        On update, the 'created' timestamp is preserved and the project map \
        is regenerated automatically."
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

        // Warn (without blocking) if this memory links to a target that doesn't
        // exist yet — dangling links are valid Obsidian-style, but easily become broken links.
        let memories = memory::load_all(&self.config, &project);
        let existing: std::collections::BTreeSet<String> =
            memories.iter().map(|m| slugify(&m.front.name)).collect();
        let missing = memories
            .iter()
            .find(|m| slugify(&m.front.name) == outcome.slug)
            .map(|m| links::missing_targets(m, &existing))
            .unwrap_or_default();

        let verb = if outcome.created {
            "Created"
        } else {
            "Updated"
        };
        let mut text = format!(
            "{verb} memory '{}' in project '{}'.\nPath: {}",
            outcome.slug,
            project,
            outcome.path.display()
        );
        if !missing.is_empty() {
            text.push_str(&format!(
                "\n⚠️ Links to memories that don't exist yet: {}. (Dangling links — create \
                 the target memory, or use memory_rename if its name has shifted.)",
                missing.join(", ")
            ));
        }
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Read the full content of a single memory (frontmatter + body).")]
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

    #[tool(description = "Concise list of all memories in a single project (JSON).")]
    async fn memory_list(
        &self,
        Parameters(args): Parameters<ListArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let entries = memory::list_entries(&self.config, &project);
        let json = serde_json::to_string_pretty(&entries).map_err(err)?;
        let header = format!("Project '{project}' — {} memories:\n", entries.len());
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(description = "Search memories in a single project by keyword \
        and/or tag. Results sorted by relevance (JSON).")]
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
        let header = format!("Project '{project}' — {} results:\n", hits.len());
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(
        description = "Regenerate the memory map (_MOC.md) for a single project: \
        grouped by category & tag, plus relations between memories. \
        Returns the map content."
    )]
    async fn memory_map(
        &self,
        Parameters(args): Parameters<MapArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let content = regenerate_moc(&self.config, &project).map_err(err)?;
        Ok(CallToolResult::success(vec![Content::text(content)]))
    }

    #[tool(description = "Suggest smart relations between memories based on tag \
        similarity (Jaccard) + content (cosine TF-IDF). Without 'name': suggestions for all \
        memories. With 'name' + 'apply: true': write the suggestions to that \
        memory's links field. Returns JSON suggestions (name, score, shared_tags, \
        shared_terms).")]
    async fn memory_suggest(
        &self,
        Parameters(args): Parameters<SuggestArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let top = args.top.unwrap_or(similarity::DEFAULT_TOP_N);
        let threshold = args.threshold.unwrap_or(similarity::DEFAULT_THRESHOLD);
        let memories = memory::load_all(&self.config, &project);
        // When the `semantic` feature is enabled, use embeddings (by meaning) for the
        // content component; otherwise, `None` → fall back to TF-IDF.
        let emb = embed::vectors_for(&self.config, &project, &memories);

        match args.name {
            // ---- Suggestions for a single memory (optionally apply to links) ----
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
                        "Suggestions for '{slug}' in project '{project}' — {} candidates:\n",
                        suggestions.len()
                    )
                } else {
                    format!(
                        "Linked to '{slug}': {}.\nFull suggestions ({} candidates):\n",
                        applied.join(", "),
                        suggestions.len()
                    )
                };
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "{header}{json}"
                ))]))
            }
            // ---- Suggestions for the entire project ----
            None => {
                let all = similarity::suggest_all_ext(&memories, top, threshold, emb.as_ref());
                let json = serde_json::to_string_pretty(&all).map_err(err)?;
                let header = format!(
                    "Relation suggestions for project '{project}' — {} memories have candidates:\n",
                    all.len()
                );
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "{header}{json}"
                ))]))
            }
        }
    }

    #[tool(description = "Check the health of a project's memory graph: find \
        broken links (links to non-existent memories, from either the links field \
        or [[wikilink]]s in the body) and orphans (memories with no inbound/outbound relations). \
        Read-only. Returns a JSON report.")]
    async fn memory_doctor(
        &self,
        Parameters(args): Parameters<DoctorArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let memories = memory::load_all(&self.config, &project);
        let mut report = links::doctor(&memories);

        // Cross-project: flag broken links whose target actually exists in
        // another project (likely wrong scope / rename candidate).
        let mut elsewhere: std::collections::BTreeMap<String, String> = Default::default();
        for other in project::list_projects(&self.config) {
            if other == project {
                continue;
            }
            for e in memory::list_entries(&self.config, &other) {
                elsewhere
                    .entry(slugify(&e.name))
                    .or_insert_with(|| other.clone());
            }
        }
        let mut also_elsewhere = 0usize;
        for b in report.broken_links.iter_mut() {
            if let Some(p) = elsewhere.get(&b.to) {
                b.also_in_project = Some(p.clone());
                also_elsewhere += 1;
            }
        }

        // Near-duplicates (only when embeddings are available / semantic feature).
        let near = self.near_duplicates(&project, &memories);

        let mut val = serde_json::to_value(&report).map_err(err)?;
        if let serde_json::Value::Object(ref mut map) = val {
            map.insert(
                "near_duplicates".into(),
                serde_json::Value::Array(near.clone()),
            );
        }
        let json = serde_json::to_string_pretty(&val).map_err(err)?;
        let header = format!(
            "Project '{project}': {} memories, {} broken links ({} exist in another project), \
             {} orphans, {} stubs, {} no-desc, {} no-tags, {} near-dup.\n",
            report.total,
            report.broken_links.len(),
            also_elsewhere,
            report.orphans.len(),
            report.stubs.len(),
            report.no_description.len(),
            report.no_tags.len(),
            near.len(),
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    /// Pairs of memories with embedding similarity ≥ `NEAR_DUP_THRESHOLD`.
    /// Empty when the `semantic` feature is off (embeddings unavailable).
    fn near_duplicates(
        &self,
        project: &str,
        memories: &[memory::Memory],
    ) -> Vec<serde_json::Value> {
        let Some(vecs) = embed::vectors_for(&self.config, project, memories) else {
            return Vec::new();
        };
        let entries: Vec<(&String, &Vec<f32>)> = vecs.iter().collect();
        let mut pairs: Vec<(String, String, f32)> = Vec::new();
        for i in 0..entries.len() {
            for j in (i + 1)..entries.len() {
                let s = embed::cosine_sim(entries[i].1, entries[j].1);
                if s >= NEAR_DUP_THRESHOLD {
                    pairs.push((entries[i].0.clone(), entries[j].0.clone(), s));
                }
            }
        }
        pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        pairs
            .into_iter()
            .map(|(a, b, s)| serde_json::json!({ "a": a, "b": b, "score": s }))
            .collect()
    }

    #[tool(
        description = "Show a memory's backlinks: the list of other memories that \
        link to it (via the links field or [[wikilink]]s in the body). Backlinks are computed \
        from the graph, not stored in the file. Read-only."
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
            "'{slug}' is linked by {} memories in project '{project}':\n",
            back.len()
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(
        description = "Group a project's memories into 'themes' via Louvain community \
        detection on the link graph (links + [[wikilink]]). Returns \
        JSON: modularity value + list of clusters (members per theme). Read-only."
    )]
    async fn memory_cluster(
        &self,
        Parameters(args): Parameters<ClusterArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let memories = memory::load_all(&self.config, &project);
        // When the `semantic` feature is enabled, enrich the graph with embedding
        // similarity edges before Louvain; otherwise, cluster based on links only.
        let emb = embed::vectors_for(&self.config, &project, &memories);
        let result = cluster::cluster_ext(&memories, emb.as_ref(), cluster::DEFAULT_SIM_THRESHOLD);
        let json = serde_json::to_string_pretty(&result).map_err(err)?;
        let header = format!(
            "Project '{project}': {} themes (modularity {:.3}).\n",
            result.clusters.len(),
            result.modularity
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(
        description = "SEMANTIC search: find memories by the MEANING of the query, \
        not word matches (e.g. 'reasons for choosing a language' finds a memory about \
        'why we use Rust'). Uses local embeddings; the index is cached per project \
        & auto-updated for changed memories. Note: only available \
        when the server is built with the 'semantic' feature."
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
            "Semantic search '{}' in project '{project}' — {} results:\n",
            args.query,
            hits.len()
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(
        description = "RECALL a unified context for a single topic: find the most \
        semantically relevant memories, then for each result include the full content + \
        outbound links + backlinks + same-theme memories. A single call replaces the \
        semantic_search→read→backlinks→cluster sequence, producing a ready-to-use \
        context package. Note: requires the 'semantic' feature."
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
            "Recall '{}' in project '{project}' — {} memories:\n",
            args.query,
            result.items.len()
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(
        description = "Add/remove links (the `links` field) of a memory WITHOUT \
        rewriting the body. Provide 'add' and/or 'remove' (lists of slugs). \
        The map is regenerated automatically. Warns when an added slug \
        doesn't exist yet (dangling link)."
    )]
    async fn memory_link(
        &self,
        Parameters(args): Parameters<LinkArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let mem = memory::read_memory(&self.config, &project, &args.name).map_err(err)?;
        let self_slug = slugify(&mem.front.name);

        let mut links = mem.front.links.clone();
        let mut removed: Vec<String> = Vec::new();
        if let Some(rm) = args.remove {
            for r in rm {
                let s = slugify(&r);
                let before = links.len();
                links.retain(|l| slugify(l) != s);
                if links.len() < before {
                    removed.push(s);
                }
            }
        }
        let mut added: Vec<String> = Vec::new();
        if let Some(ad) = args.add {
            for a in ad {
                let s = slugify(&a);
                if s.is_empty() || s == self_slug {
                    continue;
                }
                if !links.iter().any(|l| slugify(l) == s) {
                    links.push(s.clone());
                    added.push(s);
                }
            }
        }

        memory::write_memory(
            &self.config,
            &project,
            WriteInput {
                name: mem.front.name.clone(),
                description: mem.front.description.clone(),
                body: mem.body.clone(),
                tags: mem.front.tags.clone(),
                kind: Some(mem.front.kind.clone()),
                links: links.clone(),
            },
        )
        .map_err(err)?;
        regenerate_moc(&self.config, &project).map_err(err)?;

        // Warn about slugs that were added but don't exist yet.
        let existing: std::collections::BTreeSet<String> = memory::load_all(&self.config, &project)
            .iter()
            .map(|m| slugify(&m.front.name))
            .collect();
        let dangling: Vec<String> = added
            .iter()
            .filter(|s| !existing.contains(*s))
            .cloned()
            .collect();

        let mut text = format!("Links for '{self_slug}' updated in project '{project}'.");
        if !added.is_empty() {
            text.push_str(&format!("\n+ added: {}", added.join(", ")));
        }
        if !removed.is_empty() {
            text.push_str(&format!("\n- removed: {}", removed.join(", ")));
        }
        if added.is_empty() && removed.is_empty() {
            text.push_str(" (no changes)");
        }
        if !dangling.is_empty() {
            text.push_str(&format!("\n⚠️ No target yet: {}.", dangling.join(", ")));
        }
        text.push_str(&format!("\nLinks now: [{}]", links.join(", ")));
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Rename (slug) a memory AND update all inbound links \
        (the links field + [[wikilink]]s in the body) of other memories so they keep \
        resolving. The 'created' timestamp is preserved. The map is regenerated automatically. \
        Fails if the new slug is already in use.")]
    async fn memory_rename(
        &self,
        Parameters(args): Parameters<RenameArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let outcome = memory::rename_memory(&self.config, &project, &args.name, &args.new_name)
            .map_err(err)?;
        regenerate_moc(&self.config, &project).map_err(err)?;
        let text = format!(
            "Memory '{}' → '{}' in project '{}'. {} referrers updated{}.",
            outcome.old_slug,
            outcome.new_slug,
            project,
            outcome.updated_referrers.len(),
            if outcome.updated_referrers.is_empty() {
                String::new()
            } else {
                format!(": {}", outcome.updated_referrers.join(", "))
            }
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "HYBRID search: combine keyword matching and \
        meaning (semantic) into a single ranking — covering each other's gaps (keyword \
        fails on synonyms; semantic is weak on distant paraphrases). Each result \
        carries a combined score + keyword & semantic components. When the 'semantic' \
        feature is off, it automatically falls back to keyword only (noted in the header).")]
    async fn memory_hybrid_search(
        &self,
        Parameters(args): Parameters<HybridSearchArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let top = args.top.unwrap_or(5);
        let memories = memory::load_all(&self.config, &project);

        let kw = memory::search(&self.config, &project, Some(&args.query), None);
        let (sem, sem_active) = match embed::semantic_search(
            &self.config,
            &project,
            &args.query,
            memories.len().max(1),
            &memories,
        ) {
            Ok(v) => (v, true),
            Err(_) => (Vec::new(), false),
        };
        let hits = memory::merge_hybrid(&kw, &sem, top);

        let json = serde_json::to_string_pretty(&hits).map_err(err)?;
        let mode = if sem_active {
            "keyword+semantic"
        } else {
            "keyword only (semantic disabled)"
        };
        let header = format!(
            "Hybrid search [{mode}] '{}' in project '{project}' — {} results:\n",
            args.query,
            hits.len()
        );
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(description = "Delete a single memory from the project, then regenerate the map.")]
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
            format!("Memory '{}' deleted from project '{}'.", args.name, project)
        } else {
            format!("Memory '{}' not found in project '{}'.", args.name, project)
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    // ---- Documents (separate docs folder, not indexed into the graph) ----

    #[tool(
        description = "Write a long document (spec/runbook/brainstorm/worklog/etc.) \
        to the Obsidian docs folder — SEPARATE from memories, so it isn't indexed \
        into the graph/semantic/MOC. 'type' determines the initial template & default mode. \
        'mode' overwrite (default spec/runbook) replaces the content; 'append' (default \
        brainstorm/worklog) adds a timestamped entry. On update, 'created' \
        is preserved."
    )]
    async fn doc_write(
        &self,
        Parameters(args): Parameters<DocWriteArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let kind = args.kind.unwrap_or_default();
        let mode = resolve_mode(args.mode.as_deref(), &kind).map_err(err)?;

        let outcome = docs::write_doc(
            &self.config,
            &project,
            DocInput {
                name: args.name,
                title: args.title.unwrap_or_default(),
                kind,
                body: args.body.unwrap_or_default(),
                tags: args.tags.unwrap_or_default(),
            },
            mode,
        )
        .map_err(err)?;
        docs::regenerate_docs_index(&self.config, &project).map_err(err)?;

        let verb = match (outcome.created, outcome.mode) {
            (true, _) => "Created",
            (false, WriteMode::Append) => "Appended to",
            (false, WriteMode::Overwrite) => "Updated",
        };
        let text = format!(
            "{verb} document '{}' in project '{}'.\nPath: {}",
            outcome.slug,
            project,
            outcome.path.display()
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Append a single timestamped entry to a document \
        (e.g. worklog/brainstorm). If the document doesn't exist, it's created automatically from \
        the 'type' template. Does not overwrite existing content.")]
    async fn doc_append(
        &self,
        Parameters(args): Parameters<DocAppendArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let outcome = docs::write_doc(
            &self.config,
            &project,
            DocInput {
                name: args.name,
                title: String::new(),
                kind: args.kind.unwrap_or_default(),
                body: args.body,
                tags: Vec::new(),
            },
            WriteMode::Append,
        )
        .map_err(err)?;
        docs::regenerate_docs_index(&self.config, &project).map_err(err)?;

        let verb = if outcome.created {
            "Created & appended to"
        } else {
            "Appended to"
        };
        let text = format!(
            "{verb} document '{}' in project '{}'.\nPath: {}",
            outcome.slug,
            project,
            outcome.path.display()
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Read the full content of a single document (frontmatter + body).")]
    async fn doc_read(
        &self,
        Parameters(args): Parameters<DocReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let doc = docs::read_doc(&self.config, &project, &args.name).map_err(err)?;
        let text = doc.to_file_string().map_err(err)?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Concise list of documents in a single project (JSON), optionally \
        filtered by 'type'. Since documents aren't indexed into semantic search, this tool \
        is the primary way to find existing documents."
    )]
    async fn doc_list(
        &self,
        Parameters(args): Parameters<DocListArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let entries = docs::list_docs(&self.config, &project, args.kind.as_deref());
        let json = serde_json::to_string_pretty(&entries).map_err(err)?;
        let header = format!("Project '{project}' — {} documents:\n", entries.len());
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(description = "Search documents in a single project by keyword \
        (name/description/tags/body), optionally filtered by 'type'. Results sorted \
        by relevance (JSON).")]
    async fn doc_search(
        &self,
        Parameters(args): Parameters<DocSearchArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let hits = docs::search_docs(
            &self.config,
            &project,
            args.query.as_deref(),
            args.kind.as_deref(),
        );
        let json = serde_json::to_string_pretty(&hits).map_err(err)?;
        let header = format!("Project '{project}' — {} results:\n", hits.len());
        Ok(CallToolResult::success(vec![Content::text(format!(
            "{header}{json}"
        ))]))
    }

    #[tool(description = "Delete a single document from the project.")]
    async fn doc_delete(
        &self,
        Parameters(args): Parameters<DocDeleteArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let removed = docs::delete_doc(&self.config, &project, &args.name).map_err(err)?;
        if removed {
            docs::regenerate_docs_index(&self.config, &project).map_err(err)?;
        }
        let text = if removed {
            format!(
                "Document '{}' deleted from project '{}'.",
                args.name, project
            )
        } else {
            format!(
                "Document '{}' not found in project '{}'.",
                args.name, project
            )
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Rename (slug) a document. The 'created' timestamp \
        is preserved. No links need updating because documents \
        aren't part of the graph.")]
    async fn doc_rename(
        &self,
        Parameters(args): Parameters<DocRenameArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let project = self.project_of(args.project.as_deref())?;
        let out =
            docs::rename_doc(&self.config, &project, &args.name, &args.new_name).map_err(err)?;
        docs::regenerate_docs_index(&self.config, &project).map_err(err)?;
        let text = format!(
            "Document '{}' renamed to '{}' in project '{}'.",
            out.old_slug, out.new_slug, project
        );
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
            format!("\n\nExisting projects: {}.", projects.join(", "))
        };
        // `ServerInfo` is #[non_exhaustive], so start from the default then
        // change the fields we need (rather than a struct literal).
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        info.instructions = Some(format!("{INSTRUCTIONS}{extra}"));
        info
    }

    // ---- Resources: memory = `memory://<project>/<slug>`,
    //                 document = `docs://<project>/<slug>` ----

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let _guard = self.io_lock.lock().await;
        let mut out = Vec::new();
        for project in project::list_projects(&self.config) {
            // project map
            out.push(
                RawResource::new(
                    resources::uri_for(&project, "_MOC"),
                    format!("{project} / map"),
                )
                .with_description(format!("Memory map (MOC) for project {project}"))
                .with_mime_type(resources::MIME_MARKDOWN)
                .no_annotation(),
            );
            // each memory
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

        // Document resources: the `_DOCS` index + each document per project. Doc
        // projects are enumerated separately because they can differ from memory projects.
        for project in project::list_doc_projects(&self.config) {
            let entries = docs::list_docs(&self.config, &project, None);
            if entries.is_empty() {
                continue;
            }
            out.push(
                RawResource::new(
                    resources::docs_uri_for(&project, "_DOCS"),
                    format!("{project} / document index"),
                )
                .with_description(format!("Document index for project {project}"))
                .with_mime_type(resources::MIME_MARKDOWN)
                .no_annotation(),
            );
            for entry in entries {
                out.push(
                    RawResource::new(
                        resources::docs_uri_for(&project, &entry.name),
                        format!("{project} / doc / {}", entry.name),
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

        // Scheme `memory://` → memory/map; `docs://` → document/index.
        let text = if let Some(r) = resources::parse_uri(&request.uri) {
            if r.slug == "_MOC" {
                std::fs::read_to_string(self.config.moc_file(&r.project)).map_err(|e| {
                    McpError::invalid_params(format!("map '{}' not found: {e}", r.project), None)
                })?
            } else {
                let mem = memory::read_memory(&self.config, &r.project, &r.slug).map_err(err)?;
                mem.to_file_string().map_err(err)?
            }
        } else if let Some(r) = resources::parse_docs_uri(&request.uri) {
            if r.slug == "_DOCS" {
                std::fs::read_to_string(self.config.docs_index_file(&r.project)).map_err(|e| {
                    McpError::invalid_params(
                        format!("document index '{}' not found: {e}", r.project),
                        None,
                    )
                })?
            } else {
                let doc = docs::read_doc(&self.config, &r.project, &r.slug).map_err(err)?;
                doc.to_file_string().map_err(err)?
            }
        } else {
            return Err(McpError::invalid_params(
                format!("invalid resource URI: {}", request.uri),
                None,
            ));
        };

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            text,
            request.uri,
        )
        .with_mime_type(resources::MIME_MARKDOWN)]))
    }

    // ---- Prompts: ready-to-use templates for working with memories ----

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
        // the `project` argument is optional → resolve like other tools.
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
                    format!("unknown prompt: {other}"),
                    None,
                ))
            }
        };

        let mut result =
            GetPromptResult::new(vec![PromptMessage::new_text(PromptMessageRole::User, text)]);
        result.description = Some(format!("Prompt '{}' for project '{project}'", request.name));
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
            docs_root: "docs".into(),
            default_project: Some("test".into()),
        })
    }

    /// Many concurrent writes to different memories must produce valid files
    /// (intact frontmatter, exactly 1 `created:` line) and a consistent
    /// `_MOC.md` — the map must not lose entries because it was regenerated
    /// concurrently with another write.
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
                    description: format!("description {i}"),
                    body: format!("memory content number {i}"),
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
                .unwrap_or_else(|_| panic!("missing file: {}", path.display()));
            let created = raw.lines().filter(|l| l.starts_with("created:")).count();
            assert_eq!(created, 1, "corrupt file ({}):\n{raw}", path.display());
            let mem = crate::memory::Memory::from_file_string(&raw)
                .unwrap_or_else(|e| panic!("failed to parse {}: {e}\n{raw}", path.display()));
            assert_eq!(mem.front.name, format!("memo-{i}"));
        }
        // _MOC.md must contain all 32 entries (regeneration loses no data).
        let moc = std::fs::read_to_string(srv.config.moc_file("demo")).unwrap();
        for i in 0..N {
            assert!(moc.contains(&format!("[[memo-{i}]]")), "MOC lost memo-{i}");
        }
    }

    /// This is the actual race that `io_lock` guards against: many concurrent
    /// writes to the same SLUG. Each write reads the old file to preserve
    /// `created`, then rewrites. Without serialization, this step can race
    /// and produce duplicate/corrupt frontmatter. With the lock, the final result
    /// is still a single valid file with exactly 1 `created:` line.
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
                    description: format!("revision {i}"),
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
            "frontmatter corrupted by a race:\n{raw}"
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
            body: "content".into(),
            tags: Some(vec!["x".into()]),
            kind: Some("note".into()),
            links: None,
        }))
        .await
        .unwrap();

        let moc = std::fs::read_to_string(srv.config.moc_file("demo")).unwrap();
        assert!(
            moc.contains("[[hello]]"),
            "MOC does not contain the memory:\n{moc}"
        );
        assert!(
            moc.contains("#x"),
            "MOC does not contain the tag index:\n{moc}"
        );
    }
}
