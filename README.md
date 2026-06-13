# mcp-obsidian

[![CI](https://github.com/adrnsyr/mcp-obsidian/actions/workflows/ci.yml/badge.svg)](https://github.com/adrnsyr/mcp-obsidian/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

MCP server (Rust) for **writing, reading, searching, and mapping per-project
memory** into an [Obsidian](https://obsidian.md) Vault.

Each memory is stored as a plain Markdown file with YAML frontmatter, so it can
be opened, linked, and visualized directly in Obsidian (Graph View, tags,
backlinks). No database — your vault stays portable.

The server exposes all three MCP capabilities: **tools** (CRUD + analysis),
**resources** (each memory can be attached as context), and **prompts**
(ready-to-use workflows based on memory content).

## Concept

```
<Obsidian Vault>/
├── memory/                  # OBSIDIAN_MEMORY_ROOT (default: "memory")
│   ├── project-a/           # one folder per project
│   │   ├── _MOC.md          # map (Map of Content) — auto-generated
│   │   ├── auth-flow.md     # one memory = one note
│   │   └── deploy-pipeline.md
│   └── project-b/
│       └── ...
└── docs/                    # OBSIDIAN_DOCS_ROOT (default: "docs")
    └── project-a/           # long documents, SEPARATE from the memory graph
        ├── _DOCS.md         # document index — auto-generated
        ├── login-spec.md    # spec / runbook / brainstorm / worklog
        └── sprint-log.md
```

**Memory vs document.** A memory is an atomic, interlinked fact that is indexed
into the graph, semantic search, and `_MOC.md`. A document is a long note
(spec, runbook, brainstorm, worklog) stored in a separate root and **deliberately
not** indexed — so that long text does not pollute the quality of search and the
map. For that reason, documents are found via `doc_list`/`doc_search`, not
semantic search.

Example contents of a single memory (`auth-flow.md`):

```markdown
---
name: auth-flow
description: How authentication works
tags:
- auth
- security
type: project
links:
- deploy-pipeline
created: 2026-05-30T22:40:00+07:00
updated: 2026-05-30T22:40:00+07:00
---

Use JWT. See [[deploy-pipeline]].
```

`_MOC.md` is rebuilt automatically whenever something changes: grouped by
category (`type`), plus the sections **🔗 Relations** (outgoing links),
**⬅️ Backlinks** (incoming links, auto-counted), **🏷️ Tag Index**,
**💡 Suggested Relations**, and **🧩 Themes** (community clusters).

## Tools

| Tool | Function |
|------|----------|
| `memory_write` | Create/update a single memory (auto-regenerates the map) |
| `memory_read` | Read the full contents of a single memory |
| `memory_list` | Concise list of all memories in a project (JSON) |
| `memory_search` | Search by keyword and/or tag |
| `memory_map` | Regenerate `_MOC.md` and return its contents |
| `memory_suggest` | **Smart relations**: propose links between memories based on tag + content similarity (option `apply` to write into `links`) |
| `memory_backlinks` | Show which memories link to a given memory (computed from the graph) |
| `memory_doctor` | Check graph health: broken links (+ cross-project detection), orphans, **stubs**, empty metadata, & **near-duplicates** (read-only) |
| `memory_cluster` | Group memories into **themes** via graph communities (Louvain, read-only) |
| `memory_semantic_search` | Search by **meaning** via local embeddings (requires the `semantic` feature) |
| `memory_hybrid_search` | **Hybrid**: combine keyword and semantic matching in a single ranking (falls back to keyword when `semantic` is off) |
| `memory_recall` | **Unified recall**: semantic + full content + graph + themes in one call (requires the `semantic` feature) |
| `memory_link` | Add/remove links (`links`) **without rewriting the body**; warns about dangling links |
| `memory_rename` | Rename a memory **and** update all incoming links (field + `[[wikilink]]`); `created` is preserved |
| `memory_delete` | Delete a single memory (auto-regenerates the map) |

### Documents (separate `docs/` folder, not indexed into the graph)

| Tool | Function |
|------|----------|
| `doc_write` | Write a long document. `type` (spec/runbook/brainstorm/worklog) sets the initial template & default mode; `mode` `overwrite`/`append` can be overridden |
| `doc_append` | Add a timestamped entry to a document (auto-creates from template if it doesn't exist) — ideal for worklog/brainstorm |
| `doc_read` | Read the full contents of a single document |
| `doc_list` | Concise list of documents (optional `type` filter) — **the primary way to find documents**, since they are not indexed |
| `doc_search` | Search documents by keyword (optional `type` filter) |
| `doc_rename` | Rename (slug) a document; `created` is preserved |
| `doc_delete` | Delete a single document |

Default mode per `type`: `brainstorm`/`worklog` → **append**, `spec`/`runbook` → **overwrite**.

On every tool, the `project` argument is **optional**. If empty, the project is
determined in order from: the argument → `OBSIDIAN_DEFAULT_PROJECT` → the name
of the working directory where the server is run.

## Resources

Memories **and** documents are exposed as URI-addressed MCP **resources**, so
clients can list and "attach" their contents directly as context without calling
a tool.

| URI | Contents |
|-----|----------|
| `memory://<project>/<slug>` | A single memory (frontmatter + body) |
| `memory://<project>/_MOC` | The project's map (Map of Content) |
| `docs://<project>/<slug>` | A single document (frontmatter + body) |
| `docs://<project>/_DOCS` | The project's document index (auto-generated) |

MIME type: `text/markdown`. Browse via `resources/list` & `resources/read`.
The `_DOCS.md` index is regenerated automatically on every `doc_write`/`doc_append`/`doc_rename`/`doc_delete`.

## Prompts

Three ready-to-use **prompts** that assemble context from memory content. All
accept an optional `project` argument (auto-detected if empty).

| Prompt | Function |
|--------|----------|
| `summarize-project` | Summarize all of a project's memories into a brief overview |
| `review-decisions` | Review memories of type `decision` & assess their relevance |
| `onboard` | Explain the project to a new member based on existing memories |

## Configuration (environment variables)

| Variable | Required | Default | Description |
|----------|:--------:|---------|-------------|
| `OBSIDIAN_VAULT_PATH` | ✅ | — | Absolute path to the Obsidian Vault folder |
| `OBSIDIAN_MEMORY_ROOT` | | `memory` | Subfolder inside the vault for memories |
| `OBSIDIAN_DOCS_ROOT` | | `docs` | Subfolder inside the vault for documents (separate from memories) |
| `OBSIDIAN_DEFAULT_PROJECT` | | — | Default project when auto-detection fails |
| `RUST_LOG` | | `info` | Log level (written to stderr) |

## Build

```bash
cargo build --release
# binary: target/release/mcp-obsidian
```

### Build with semantic search (optional)

Semantic search (`memory_semantic_search`) is **opt-in** via the cargo feature
`semantic`, so the default build stays lightweight & offline:

```bash
cargo build --release --features semantic
```

This build bundles **pure-Rust** local embeddings (candle). On first use, the
multilingual model `paraphrase-multilingual-MiniLM-L12-v2` (~470 MB) is
downloaded once into the HuggingFace cache (`~/.cache/huggingface`), then runs
offline. A multilingual model was chosen so that non-English memories (e.g.
Indonesian) are still searched correctly by meaning. Without this feature, the
`memory_semantic_search` tool remains registered but returns a message
explaining how to enable it.

> When the `semantic` feature is active, `memory_suggest` & `memory_cluster`
> also automatically use embeddings (by meaning); without the feature, both fall
> back to TF-IDF/graph.

### Build with auto-sync (optional)

The file watcher (the `watch` feature) monitors the memory folder and
**regenerates `_MOC.md` automatically** when memories are edited directly in
Obsidian (outside the MCP tools):

```bash
cargo build --release --features watch
```

It uses `notify` (FSEvents/inotify) with a 2-second debounce to dampen the burst
of events when an editor saves. Files produced by the server itself (`_MOC.md`,
dotfiles) are ignored so no loop occurs. The features can be combined:
`cargo build --release --features "semantic watch"`.

## Registering with Claude Code

```bash
claude mcp add obsidian-memory \
  --env OBSIDIAN_VAULT_PATH="/path/to/Obsidian Vault" \
  -- /path/to/mcp-obsidian/target/release/mcp-obsidian
```

Or manually in `~/.claude.json` / another MCP client's config:

```json
{
  "mcpServers": {
    "obsidian-memory": {
      "command": "/path/to/mcp-obsidian/target/release/mcp-obsidian",
      "env": {
        "OBSIDIAN_VAULT_PATH": "/path/to/Obsidian Vault"
      }
    }
  }
}
```

> Replace `/path/to/...` to match your system. Example `OBSIDIAN_VAULT_PATH` per
> OS: macOS `~/Documents/Obsidian Vault`, Linux `~/obsidian/vault`,
> Windows `C:\Users\<name>\Documents\Obsidian Vault`.

> Note: logs are intentionally written to **stderr** because **stdout** is used
> by the MCP protocol (JSON-RPC). Do not write anything to stdout.

## Development & testing

```bash
cargo test          # runs unit + integration tests
```

The tests cover write/read roundtrips, preservation of the `created` timestamp
on update, search/list, map generation, smart-relation scoring, wikilink
extraction & backlink graph, resource URI parsing, prompt rendering, and a
**concurrency regression test**: because `memory_write` performs a
read-modify-write and then regenerates `_MOC.md`, all operations are serialized
through a mutex (`io_lock`) so that no writes race and the map is never read
half-built.

## Code structure

| File | Responsibility |
|------|----------------|
| `src/main.rs` | Entry point: set up logging (stderr) + serve via stdio |
| `src/config.rs` | Resolve configuration & paths from the environment |
| `src/project.rs` | Slugify + project detection (arg → env → cwd) |
| `src/memory.rs` | Frontmatter, memory CRUD, search |
| `src/docs.rs` | Long documents (spec/runbook/brainstorm/worklog): write/append/read/list/search in a separate folder |
| `src/mapping.rs` | `_MOC.md` generation (relations + backlinks + tags + suggestions + themes) |
| `src/similarity.rs` | Smart relations: TF-IDF (content) + Jaccard (tags) similarity scoring |
| `src/links.rs` | Link graph: wikilink extraction, backlinks, broken links & orphans |
| `src/cluster.rs` | Theme clustering: Louvain community detection (modularity) |
| `src/embed.rs` | Semantic search: candle embeddings + sidecar index (feature `semantic`) |
| `src/recall.rs` | Unified recall: assemble semantic + graph + themes into a single payload |
| `src/watcher.rs` | File watcher: auto-regenerate `_MOC.md` on external edits (feature `watch`) |
| `src/resources.rs` | Resource URIs (`memory://…`) + parsing |
| `src/prompts.rs` | Prompt catalog + text rendering |
| `src/server.rs` | MCP server definition: tools, resources, prompts (rmcp) |

## Smart relations (`memory_suggest`)

Beyond manual links (the `links` field), the server can **suggest** links
automatically by combining two similarity signals:

- **Tag similarity** — Jaccard over the tag sets: `|A∩B| / |A∪B|`
- **Content similarity** — cosine similarity over `name + description + body`.
  If the build uses the `semantic` feature, this uses **embeddings** (by
  meaning); otherwise it falls back to **TF-IDF** vectors (normalized tokens,
  ID/EN stopwords).

Final score = `0.6 · tag + 0.4 · content`. Memories already present in `links`
are skipped, results are ranked and cut to top-N above a threshold. Each
suggestion is **explainable**: it also reports `shared_tags` & `shared_terms`.

```jsonc
// Suggestions for all memories in the project (read-only)
{"name": "memory_suggest", "arguments": {"project": "demo"}}

// Suggestions for a single memory + write directly into the links field
{"name": "memory_suggest", "arguments": {"project": "demo", "name": "auth-flow", "apply": true}}

// Tune count & threshold: top 3, minimum score 0.1
{"name": "memory_suggest", "arguments": {"project": "demo", "top": 3, "threshold": 0.1}}
```

`_MOC.md` also automatically shows a **💡 Suggested Relations** section
(suggestions, not real links until you `apply`).

## Clusters / Themes (`memory_cluster`)

Memories are grouped into **themes** via **Louvain** community detection on the
undirected link graph (built from the `links` field + `[[wikilink]]` in the
body; edge weights accumulate when there are multiple links between the same
pair). If the build uses the `semantic` feature, the graph is also enriched with
**embedding-similarity edges** (pairs with cosine ≥ 0.6) before Louvain — so
themes form from links **and** semantic proximity, not just explicit links.

Louvain maximizes **modularity** Q:

```text
Q = Σ_c [ Σ_in(c) / 2m − ( Σ_tot(c) / 2m )² ]
```

where `Σ_in` is the weight of a community's internal edges, `Σ_tot` is the
community's total degree, and `m` is the total edge weight. The higher Q (max
~1.0), the sharper the community separation. A memory with no links becomes its
own singleton community.

```jsonc
// Group the project's memories into themes (read-only)
{"name": "memory_cluster", "arguments": {"project": "demo"}}
```

> Note: `Q = 0` is normal when all linked memories form **one connected
> component with no sub-structure** — there is no "community within a community"
> to split. Q rises as soon as several relatively separate groups appear.

`_MOC.md` shows a **🧩 Themes** section when there is more than one meaningful
cluster.

## Semantic search (`memory_semantic_search`)

> Requires a `--features semantic` build (see the Build section).

Unlike `memory_search` (keyword-matching), semantic search finds memories by
**meaning**. For example, the query `"reasons for choosing a programming
language"` finds a memory titled *"Why we use Rust"* even though no words match.

> Full design notes (how embeddings work, the multilingual model decision,
> real-world test results, & technical notes): see [semantic.md](semantic.md).

How it works:

- Each memory (`name + description + body`) is embedded into a 384-dimensional
  vector with the multilingual model **paraphrase-multilingual-MiniLM-L12-v2**
  (candle, pure-Rust, local) — supporting 50+ languages including Indonesian.
- Vectors are cached in the sidecar file `memory/<project>/.embeddings.json`.
  Only **changed** memories (detected via a content hash) are re-embedded — so
  subsequent searches are fast.
- Relevance = cosine similarity between the query vector and each memory; results
  are ordered from most relevant.

```jsonc
{"name": "memory_semantic_search",
 "arguments": {"project": "demo", "query": "why this architecture was chosen", "top": 5}}
```

The `.embeddings.json` file is a pure cache — safe to delete (it will be rebuilt)
and not treated as a memory.

## Unified recall (`memory_recall`)

> Requires a `--features semantic` build (see the Build section).

`memory_recall` is a **single-call context retrieval** for AI agents. Instead of
chaining `semantic_search` → `read` → `backlinks` → `cluster` yourself, a single
call returns an already-enriched context package:

1. **Semantic search** → take the top-K memories most relevant to the query.
2. For each result, attach the **full content** (body) + **outgoing links** +
   **backlinks** (from the graph) + **same-theme memories** (from the cluster).

```jsonc
{"name": "memory_recall",
 "arguments": {"project": "demo", "query": "why this project uses the Rust language", "top": 3}}
```

Each result item contains: `name`, `description`, `score` (semantic relevance),
`type`, `tags`, `body`, `links`, `backlinks`, `theme`. Ideal to use directly as
context for an LLM with no extra steps.

## License

Released under the [MIT](LICENSE) license. Free to use, modify, and
redistribute.
