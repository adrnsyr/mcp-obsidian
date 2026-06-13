# Semantic Search — Notes & Documentation

Complete notes on the **semantic search** feature in `mcp-obsidian`: what it is, why
it matters, how it works, the design decisions, real test results, and how to use it.

> Status: this feature is now **merged into `main`**, and is **opt-in** via the
> `semantic` cargo feature. This document holds the in-depth design notes — for a
> usage summary see the "Semantic search" section in the [README](README.md).

---

## 1. What it is & why it's needed

The built-in search (`memory_search`) is a **keyword search** — it matches **exactly
the same words**. It is blind to meaning.

A real example from the test vault: there is a memory `keputusan-rust` containing *"Why
use Rust for the MCP server"*.

```
memory_search "alasan memilih bahasa"   →  0 results   ❌  (no exact word match)
```

**Semantic search** finds results based on **meaning**, not words:

```
memory_semantic_search "alasan memilih bahasa pemrograman"
    →  keputusan-rust  (score 0.355, rank #1)   ✅
```

This turns the server from a "structured grep" into a memory system that feels like it
"understands" — the foundation for good recall by an AI agent.

---

## 2. How it works

### Embedding
A small AI model turns text into a **vector** (a sequence of numbers, 384 dimensions).
Its key property: text with **similar meaning** produces vectors that are **close
together**. Searching = turn the query into a vector, then find the memory whose vector
is closest.

### Pipeline (`src/embed.rs`)
1. The text of each memory = `name + description + body`.
2. Embedded with **candle** (pure-Rust) using a BERT model.
3. **Masked mean-pooling** over the tokens (ignoring padding) → one vector per memory.
4. **L2-normalize** → vector length = 1, so cosine similarity = dot product.
5. The query↔memory relevance score = cosine; results are sorted in descending order.

### Index sidecar (cache)
Embedding is expensive, so vectors are stored in
`memory/<project>/.embeddings.json`:

```jsonc
{
  "model": "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2",
  "dim": 384,
  "entries": {
    "keputusan-rust": { "hash": 12345678, "vector": [0.01, -0.04, ...] }
  }
}
```

- Each entry stores a **content hash** (FNV-1a). On a search, only memories that have
  **changed** (different hash) or are **new** are re-embedded; the rest are reused from
  the cache → subsequent searches are fast.
- Memories that have been deleted are automatically dropped from the index.
- If the `model` or `dim` in the file differs from the one currently in use, **the
  entire index is discarded & rebuilt** (see `EmbeddingIndex::load`). This is what makes
  switching models automatically safe.
- This file is **pure cache** — safe to delete, it will be rebuilt. It is not treated as
  a memory (its name starts with `.`, not `.md`).

---

## 3. Design decisions (and the reasoning)

| Decision | Choice | Reason |
|-----------|---------|--------|
| Backend | **candle** (pure-Rust) | Stays true to the "single binary & offline" ethos. No ONNX/native pull-in like `fastembed`. |
| Integration | **opt-in `--features semantic`** | The default build stays lightweight & offline; those who don't need it don't pay for a heavy dependency. |
| Storage | **sidecar `.embeddings.json`** | The vault stays clean (`.md` files are untouched); the cache is fast and disposable. |
| Model | **multilingual** (see §4) | Memories may be non-English. |

Without the `semantic` feature, the `memory_semantic_search` tool **is still
registered** but returns a friendly message explaining how to enable it.

---

## 4. Key lesson: the model must be multilingual

The first version used **`all-MiniLM-L6-v2`** — an **English** model. Because the test
memories were in Indonesian, the results were **noise** (nearly random ranking):

**Query "alasan memilih bahasa pemrograman" — English model (WRONG):**
```
#1  0.542  catatan-rapat     ← meeting agenda, NOT relevant, yet ranked top
...
#5  0.161  keputusan-rust    ← the correct answer, yet ranked LAST
```

Switched to **`paraphrase-multilingual-MiniLM-L12-v2`** (50+ languages, 384 dim, same
BERT architecture → drop-in). The results flipped and became correct:

**Same query — multilingual model (CORRECT):**
```
#1  0.355  keputusan-rust     ← rose from last to #1
#2  0.329  arsitektur-server
#4  0.071  catatan-rapat      ← dropped from #1 to #4
```

**An honest note about the second test.** The query *"cara kerja autentikasi dan login
pengguna"* returned `relasi-pintar` (#1, 0.437) — not a clearly correct answer. But this
is **expected**: the test vault has no memory about authentication at all, so there is
nothing correct to find. The scores are uniformly low (all < 0.44), which is actually a
healthy signal — the model isn't forcing a false match.

> Lesson: for a non-English corpus, a multilingual model is **mandatory**. Changing
> `MODEL_ID` in `src/embed.rs` automatically invalidates all old indexes.

---

## 5. How to use it

### Build (with the semantic feature)
```bash
cargo build --release --features semantic
```
On first use, the model (~470 MB) is downloaded once into the HuggingFace cache
(`~/.cache/huggingface`), then runs **offline**.

### Calling the tool
```jsonc
{"name": "memory_semantic_search",
 "arguments": {"project": "demo", "query": "kenapa pilih arsitektur ini", "top": 5}}
```

Arguments:
- `project` (optional) — auto-detected from the working dir if omitted.
- `query` (required) — a natural-language query.
- `top` (optional, default 5) — the number of results.

Result: a list of `{name, description, score}` sorted from most relevant. `score` is the
cosine similarity (−1..1); the higher, the more relevant.

---

## 6. Technical notes (for developers)

- **candle 0.10.2 API**: `candle_transformers::models::bert::{BertModel, Config,
  DTYPE}`; `VarBuilder::from_mmaped_safetensors` (unsafe, mmap);
  `model.forward(input_ids, token_type_ids, Some(&attention_mask))` (3 arguments in this
  version); extract the result via `tensor.to_vec2::<f32>()`.
- **candle is pure-Rust** — it does not use the ONNX Runtime (that's `fastembed`).
- **Global embedder**: the model is loaded **once** (lazy `static Mutex<Option<...>>`)
  then reused repeatedly — loading the model is expensive, inference afterwards is cheap.
- **Blocking**: candle inference is synchronous/CPU-bound. It currently runs inside the
  server's `io_lock` guard (sufficient for single-user load). If parallelism is needed,
  move it to `tokio::task::spawn_blocking`.
- **Tests**: the default build stays at **27 tests, 0 warnings** (3 embed tests: hash,
  cosine, error-without-feature). The `--features semantic` build: **26 tests, 0
  warnings**. The index helper is given `#[cfg_attr(not(feature="semantic"),
  allow(dead_code))]` to keep the default build clean.

---

## 7. Future work / further development ideas

- ✅ **Already implemented** — Upgrade `memory_suggest` & `memory_cluster` to use
  embedding vectors (instead of TF-IDF) → meaning-based relations & themes.
- ✅ **Already implemented** — A unified `memory_recall` tool: semantic search → take
  the top-K → include graph neighbors & themes → a single, ready-to-use context payload
  for the agent.
- **Cross-project** semantic search.
