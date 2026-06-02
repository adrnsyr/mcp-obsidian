# mcp-obsidian

MCP server (Rust) untuk **menulis, membaca, mencari, dan memetakan memori
per-project** ke dalam sebuah [Obsidian](https://obsidian.md) Vault.

Setiap memori disimpan sebagai satu file Markdown biasa dengan frontmatter YAML,
sehingga langsung bisa dibuka, di-link, dan divisualisasikan di Obsidian
(Graph View, tags, backlinks). Tidak ada database — vault-mu tetap portabel.

Server mengekspos ketiga kapabilitas MCP: **tools** (CRUD + analisis),
**resources** (tiap memori bisa di-attach sebagai konteks), dan **prompts**
(alur kerja siap-pakai berbasis isi memori).

## Konsep

```
<Obsidian Vault>/
└── memory/                  # OBSIDIAN_MEMORY_ROOT (default: "memory")
    ├── proyek-a/            # satu folder per project
    │   ├── _MOC.md          # peta (Map of Content) — digenerate otomatis
    │   ├── auth-flow.md     # satu memori = satu catatan
    │   └── deploy-pipeline.md
    └── proyek-b/
        └── ...
```

Contoh isi satu memori (`auth-flow.md`):

```markdown
---
name: auth-flow
description: Cara kerja autentikasi
tags:
- auth
- security
type: project
links:
- deploy-pipeline
created: 2026-05-30T22:40:00+07:00
updated: 2026-05-30T22:40:00+07:00
---

Pakai JWT. Lihat [[deploy-pipeline]].
```

`_MOC.md` dibuat ulang otomatis setiap kali ada perubahan: dikelompokkan per
kategori (`type`), plus bagian **🔗 Relasi** (tautan keluar), **⬅️ Backlink**
(tautan masuk, dihitung otomatis), **🏷️ Indeks Tag**, **💡 Saran Relasi**, dan
**🧩 Tema** (klaster komunitas).

## Tools yang disediakan

| Tool | Fungsi |
|------|--------|
| `memory_write` | Buat/perbarui satu memori (regenerasi peta otomatis) |
| `memory_read` | Baca isi lengkap satu memori |
| `memory_list` | Daftar ringkas semua memori dalam project (JSON) |
| `memory_search` | Cari berdasarkan kata kunci dan/atau tag |
| `memory_map` | Regenerasi `_MOC.md` & kembalikan isinya |
| `memory_suggest` | **Relasi pintar**: usulkan tautan antar-memori berdasarkan kemiripan tag + isi (opsi `apply` untuk menulis ke `links`) |
| `memory_backlinks` | Tampilkan memori mana yang menaut sebuah memori (dihitung dari graf) |
| `memory_doctor` | Periksa kesehatan graf: broken link & orphan (read-only) |
| `memory_cluster` | Kelompokkan memori jadi **tema** via komunitas graf (Louvain, read-only) |
| `memory_semantic_search` | Cari berdasarkan **makna** via embedding lokal (perlu feature `semantic`) |
| `memory_recall` | **Recall terpadu**: semantik + isi penuh + graf + tema dalam satu panggilan (perlu feature `semantic`) |
| `memory_delete` | Hapus satu memori (regenerasi peta otomatis) |

Pada setiap tool, argumen `project` **opsional**. Bila kosong, project ditentukan
berurutan dari: argumen → `OBSIDIAN_DEFAULT_PROJECT` → nama folder working
directory tempat server dijalankan.

## Resources

Tiap memori diekspos sebagai MCP **resource** ber-URI, sehingga klien bisa
me-list dan "attach" isinya langsung sebagai konteks tanpa memanggil tool.

| URI | Isi |
|-----|-----|
| `memory://<project>/<slug>` | Satu memori (frontmatter + body) |
| `memory://<project>/_MOC` | Peta (Map of Content) project |

MIME type: `text/markdown`. Lihat lewat `resources/list` & `resources/read`.

## Prompts

Tiga **prompt** siap-pakai yang merakit konteks dari isi memori. Semua menerima
argumen opsional `project` (auto-detect bila kosong).

| Prompt | Fungsi |
|--------|--------|
| `summarize-project` | Rangkum seluruh memori project jadi ikhtisar singkat |
| `review-decisions` | Tinjau memori bertipe `decision` & nilai relevansinya |
| `onboard` | Jelaskan project ke anggota baru berdasarkan memori yang ada |

## Konfigurasi (environment variable)

| Variabel | Wajib | Default | Keterangan |
|----------|:-----:|---------|------------|
| `OBSIDIAN_VAULT_PATH` | ✅ | — | Path absolut ke folder Obsidian Vault |
| `OBSIDIAN_MEMORY_ROOT` | | `memory` | Subfolder di dalam vault untuk memori |
| `OBSIDIAN_DEFAULT_PROJECT` | | — | Project default bila auto-detect gagal |
| `RUST_LOG` | | `info` | Level log (ditulis ke stderr) |

## Build

```bash
cargo build --release
# binary: target/release/mcp-obsidian
```

### Build dengan pencarian semantik (opsional)

Pencarian semantik (`memory_semantic_search`) bersifat **opt-in** lewat cargo
feature `semantic` agar build default tetap ringan & offline:

```bash
cargo build --release --features semantic
```

Build ini menyertakan embedding lokal **pure-Rust** (candle). Saat dipakai
pertama kali, model multilingual `paraphrase-multilingual-MiniLM-L12-v2`
(~470 MB) diunduh sekali ke cache HuggingFace (`~/.cache/huggingface`), lalu
berjalan offline. Model multilingual dipilih agar memori non-Inggris (mis.
bahasa Indonesia) tetap dicari dengan benar berdasarkan makna. Tanpa feature ini,
tool `memory_semantic_search` tetap terdaftar tapi mengembalikan pesan yang
menjelaskan cara mengaktifkannya.

> Saat fitur `semantic` aktif, `memory_suggest` & `memory_cluster` juga otomatis
> memakai embedding (by makna); tanpa fitur, keduanya fallback ke TF-IDF/graf.

### Build dengan auto-sync (opsional)

File watcher (fitur `watch`) memantau folder memori dan **meregenerasi `_MOC.md`
otomatis** saat memori diedit langsung di Obsidian (di luar tool MCP):

```bash
cargo build --release --features watch
```

Memakai `notify` (FSEvents/inotify) dengan debounce 2 detik untuk meredam
ledakan event saat editor menyimpan. File yang dihasilkan server sendiri
(`_MOC.md`, dotfile) diabaikan sehingga tak terjadi loop. Fitur bisa digabung:
`cargo build --release --features "semantic watch"`.

## Mendaftarkan ke Claude Code

```bash
claude mcp add obsidian-memory \
  --env OBSIDIAN_VAULT_PATH="/path/to/Obsidian Vault" \
  -- /path/to/mcp-obsidian/target/release/mcp-obsidian
```

Atau secara manual di `~/.claude.json` / config MCP klien lain:

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

> Ganti `/path/to/...` sesuai sistemmu. Contoh `OBSIDIAN_VAULT_PATH` per OS:
> macOS `~/Documents/Obsidian Vault`, Linux `~/obsidian/vault`,
> Windows `C:\Users\<nama>\Documents\Obsidian Vault`.

> Catatan: log sengaja ditulis ke **stderr** karena **stdout** dipakai protokol
> MCP (JSON-RPC). Jangan menulis apa pun ke stdout.

## Pengembangan & test

```bash
cargo test          # menjalankan unit + integration test
```

Test mencakup roundtrip tulis/baca, pelestarian timestamp `created`
saat update, search/list, generasi peta, skor relasi pintar, ekstraksi wikilink &
graf backlink, parsing URI resource, render prompt, serta **regression test
konkurensi**: karena `memory_write` melakukan read-modify-write lalu meregenerasi
`_MOC.md`, semua operasi diserialkan lewat sebuah mutex (`io_lock`) agar tidak
ada write yang balapan & peta tidak terbaca setengah jadi.

## Struktur kode

| File | Tanggung jawab |
|------|----------------|
| `src/main.rs` | Entry point: setup logging (stderr) + serve via stdio |
| `src/config.rs` | Resolusi konfigurasi & path dari environment |
| `src/project.rs` | Slugify + deteksi project (arg → env → cwd) |
| `src/memory.rs` | Frontmatter, CRUD memori, search |
| `src/mapping.rs` | Generasi `_MOC.md` (relasi + backlink + tag + saran + tema) |
| `src/similarity.rs` | Relasi pintar: skor kemiripan TF-IDF (isi) + Jaccard (tag) |
| `src/links.rs` | Graf tautan: ekstraksi wikilink, backlink, broken link & orphan |
| `src/cluster.rs` | Klaster tema: deteksi komunitas Louvain (modularity) |
| `src/embed.rs` | Pencarian semantik: embedding candle + index sidecar (feature `semantic`) |
| `src/recall.rs` | Recall terpadu: rangkai semantic + graf + tema jadi satu payload |
| `src/watcher.rs` | File watcher: auto-regen `_MOC.md` saat edit eksternal (feature `watch`) |
| `src/resources.rs` | URI resource (`memory://…`) + parsing |
| `src/prompts.rs` | Katalog prompt + render teksnya |
| `src/server.rs` | Definisi MCP server: tools, resources, prompts (rmcp) |

## Relasi pintar (`memory_suggest`)

Selain tautan manual (field `links`), server bisa **menyarankan** tautan secara
otomatis dengan menggabungkan dua sinyal kemiripan:

- **Kemiripan tag** — Jaccard pada himpunan tag: `|A∩B| / |A∪B|`
- **Kemiripan isi** — cosine similarity pada `name + description + body`.
  Bila build memakai fitur `semantic`, ini memakai **embedding** (by makna);
  selain itu fallback ke vektor **TF-IDF** (token dinormalkan, stopword ID/EN).

Skor akhir = `0.6 · tag + 0.4 · isi`. Memori yang sudah ada di `links` di-skip,
hasil di-ranking dan dipotong ke top-N di atas threshold. Tiap saran
**explainable**: ikut melaporkan `shared_tags` & `shared_terms`.

```jsonc
// Saran untuk semua memori di project (read-only)
{"name": "memory_suggest", "arguments": {"project": "demo"}}

// Saran untuk satu memori + langsung tulis ke field links
{"name": "memory_suggest", "arguments": {"project": "demo", "name": "auth-flow", "apply": true}}

// Atur jumlah & ambang: top 3, skor minimal 0.1
{"name": "memory_suggest", "arguments": {"project": "demo", "top": 3, "threshold": 0.1}}
```

`_MOC.md` juga otomatis menampilkan bagian **💡 Saran Relasi** (usulan, bukan
tautan nyata sampai kamu `apply`).

## Klaster / Tema (`memory_cluster`)

Memori dikelompokkan menjadi **tema** lewat deteksi komunitas **Louvain** pada
graf tautan tak-berarah (dibangun dari field `links` + `[[wikilink]]` di body;
bobot edge diakumulasi bila ada beberapa tautan antar pasangan yang sama).
Bila build memakai fitur `semantic`, graf juga diperkaya **edge kemiripan
embedding** (pasangan dengan cosine ≥ 0.6) sebelum Louvain — sehingga tema
terbentuk dari tautan **dan** kedekatan makna, bukan hanya tautan eksplisit.

Louvain memaksimalkan **modularity** Q:

```text
Q = Σ_c [ Σ_in(c) / 2m − ( Σ_tot(c) / 2m )² ]
```

dengan `Σ_in` bobot edge internal komunitas, `Σ_tot` total derajat komunitas,
dan `m` total bobot edge. Makin tinggi Q (maks ~1.0), makin tegas pemisahan
komunitasnya. Memori tanpa tautan menjadi komunitas singleton-nya sendiri.

```jsonc
// Kelompokkan memori project jadi tema (read-only)
{"name": "memory_cluster", "arguments": {"project": "demo"}}
```

> Catatan: `Q = 0` itu wajar bila semua memori yang tertaut membentuk **satu
> komponen terhubung tanpa sub-struktur** — tidak ada "komunitas dalam komunitas"
> untuk dipisah. Q naik begitu muncul beberapa kelompok yang relatif terpisah.

`_MOC.md` menampilkan bagian **🧩 Tema** bila ada lebih dari satu klaster
bermakna.

## Pencarian semantik (`memory_semantic_search`)

> Memerlukan build `--features semantic` (lihat bagian Build).

Berbeda dari `memory_search` (cocok-kata), pencarian semantik menemukan memori
berdasarkan **makna**. Contoh: kueri `"alasan memilih bahasa pemrograman"`
menemukan memori berjudul *"Kenapa pakai Rust"* walau tak ada kata yang sama.

Cara kerja:

- Tiap memori (`name + description + body`) di-embed jadi vektor 384-dimensi
  dengan model multilingual **paraphrase-multilingual-MiniLM-L12-v2** (candle,
  pure-Rust, lokal) — mendukung 50+ bahasa termasuk Indonesia.
- Vektor di-cache di file sidecar `memory/<project>/.embeddings.json`. Hanya
  memori yang **berubah** (deteksi via hash isi) yang di-embed ulang — jadi
  pencarian berikutnya cepat.
- Relevansi = cosine similarity antara vektor kueri dan tiap memori; hasil
  diurutkan dari paling relevan.

```jsonc
{"name": "memory_semantic_search",
 "arguments": {"project": "demo", "query": "kenapa pilih arsitektur ini", "top": 5}}
```

File `.embeddings.json` adalah cache murni — aman dihapus (akan dibangun ulang)
dan tidak dianggap sebagai memori.

## Recall terpadu (`memory_recall`)

> Memerlukan build `--features semantic` (lihat bagian Build).

`memory_recall` adalah **retrieval konteks satu-panggilan** untuk AI agent. Alih-alih
merangkai sendiri `semantic_search` → `read` → `backlinks` → `cluster`, satu
panggilan mengembalikan paket konteks yang sudah diperkaya:

1. **Pencarian semantik** → ambil top-K memori paling relevan dengan kueri.
2. Untuk tiap hasil, lampirkan **isi penuh** (body) + **tautan keluar** +
   **backlink** (dari graf) + **memori setema** (dari klaster).

```jsonc
{"name": "memory_recall",
 "arguments": {"project": "demo", "query": "kenapa proyek ini memakai bahasa Rust", "top": 3}}
```

Tiap item hasil berisi: `name`, `description`, `score` (relevansi semantik),
`type`, `tags`, `body`, `links`, `backlinks`, `theme`. Cocok dijadikan konteks
langsung untuk LLM tanpa langkah tambahan.

## Lisensi

Dirilis di bawah lisensi [MIT](LICENSE). Bebas dipakai, dimodifikasi, dan
didistribusikan ulang.
