# Semantic Search — Catatan & Dokumentasi

Catatan lengkap soal fitur **pencarian semantik** di `mcp-obsidian`: apa, kenapa,
cara kerja, keputusan desain, hasil uji nyata, dan cara memakainya.

> Status: fitur ini hidup di branch **`feature/semantic-search`**, terpisah dari
> `master`. Bersifat **opt-in** lewat cargo feature `semantic`.

---

## 1. Apa itu & kenapa perlu

Pencarian bawaan (`memory_search`) adalah **keyword search** — mencocokkan
**kata yang sama persis**. Ia buta terhadap makna.

Contoh nyata di vault uji: ada memori `keputusan-rust` berisi *"Kenapa pakai Rust
untuk MCP server"*.

```
memory_search "alasan memilih bahasa"   →  0 hasil   ❌  (tak ada kata yang persis)
```

**Semantic search** mencari berdasarkan **makna**, bukan kata:

```
memory_semantic_search "alasan memilih bahasa pemrograman"
    →  keputusan-rust  (skor 0.355, peringkat #1)   ✅
```

Ini mengubah server dari "grep berstruktur" menjadi sistem memori yang terasa
"mengerti" — fondasi untuk recall yang baik oleh AI agent.

---

## 2. Cara kerja

### Embedding
Sebuah model AI kecil mengubah teks jadi **vektor** (deretan angka, 384 dimensi).
Sifat kuncinya: teks yang **maknanya mirip** menghasilkan vektor yang
**berdekatan**. Pencarian = ubah query jadi vektor, lalu cari memori yang
vektornya paling dekat.

### Pipeline (`src/embed.rs`)
1. Teks tiap memori = `name + description + body`.
2. Di-embed dengan **candle** (pure-Rust) memakai model BERT.
3. **Masked mean-pooling** atas token (abaikan padding) → satu vektor per memori.
4. **L2-normalize** → panjang vektor = 1, sehingga cosine similarity = dot product.
5. Skor relevansi query↔memori = cosine; hasil diurutkan menurun.

### Index sidecar (cache)
Meng-embed itu mahal, jadi vektor disimpan di
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

- Tiap entri menyimpan **hash isi** (FNV-1a). Saat search, hanya memori yang
  **berubah** (hash beda) atau **baru** yang di-embed ulang; sisanya dipakai dari
  cache → search berikutnya cepat.
- Memori yang sudah dihapus otomatis dibuang dari index.
- Bila `model` atau `dim` di file beda dengan yang dipakai sekarang, **seluruh
  index dibuang & dibangun ulang** (lihat `EmbeddingIndex::load`). Inilah yang
  membuat ganti model otomatis aman.
- File ini **cache murni** — aman dihapus, akan dibangun ulang. Tidak dianggap
  memori (namanya diawali `.`, bukan `.md`).

---

## 3. Keputusan desain (dan alasannya)

| Keputusan | Pilihan | Alasan |
|-----------|---------|--------|
| Backend | **candle** (pure-Rust) | Setia ke ethos "binary tunggal & offline". Tak menarik ONNX/native seperti `fastembed`. |
| Integrasi | **opt-in `--features semantic`** | Build default tetap ringan & offline; yang tak butuh tak menanggung dependency berat. |
| Penyimpanan | **sidecar `.embeddings.json`** | Vault tetap bersih (file `.md` tak tersentuh); cache cepat & bisa dibuang. |
| Model | **multilingual** (lihat §4) | Memori bisa non-Inggris. |

Tanpa feature `semantic`, tool `memory_semantic_search` **tetap terdaftar** tapi
mengembalikan pesan ramah yang menjelaskan cara mengaktifkannya.

---

## 4. Pelajaran penting: model harus multilingual

Versi pertama memakai **`all-MiniLM-L6-v2`** — model **bahasa Inggris**. Karena
memori uji berbahasa Indonesia, hasilnya **noise** (ranking nyaris acak):

**Query "alasan memilih bahasa pemrograman" — model Inggris (SALAH):**
```
#1  0.542  catatan-rapat     ← agenda rapat, TAK relevan, malah teratas
...
#5  0.161  keputusan-rust    ← jawaban benar, malah PALING BAWAH
```

Diganti ke **`paraphrase-multilingual-MiniLM-L12-v2`** (50+ bahasa, 384 dim,
arsitektur BERT sama → drop-in). Hasilnya terbalik dan benar:

**Query yang sama — model multilingual (BENAR):**
```
#1  0.355  keputusan-rust     ← naik dari terbawah ke #1
#2  0.329  arsitektur-server
#4  0.071  catatan-rapat      ← turun dari #1 ke #4
```

**Catatan jujur soal uji kedua.** Query *"cara kerja autentikasi dan login
pengguna"* memberi `relasi-pintar` (#1, 0.437) — bukan jawaban yang jelas. Tapi
ini **wajar**: vault uji tidak punya memori soal autentikasi sama sekali, jadi
tak ada yang benar untuk ditemukan. Skornya rendah merata (semua < 0.44), yang
justru sinyal sehat — model tidak memaksakan kecocokan palsu.

> Pelajaran: untuk korpus non-Inggris, **wajib** model multilingual. Mengganti
> `MODEL_ID` di `src/embed.rs` otomatis meng-invalidasi semua index lama.

---

## 5. Cara pakai

### Build (dengan fitur semantik)
```bash
cargo build --release --features semantic
```
Saat dipakai pertama kali, model (~470 MB) diunduh sekali ke cache HuggingFace
(`~/.cache/huggingface`), lalu berjalan **offline**.

### Memanggil tool
```jsonc
{"name": "memory_semantic_search",
 "arguments": {"project": "demo", "query": "kenapa pilih arsitektur ini", "top": 5}}
```

Argumen:
- `project` (opsional) — auto-detect dari working dir bila kosong.
- `query` (wajib) — kueri bahasa alami.
- `top` (opsional, default 5) — jumlah hasil.

Hasil: daftar `{name, description, score}` terurut dari paling relevan. `score`
adalah cosine similarity (−1..1); makin tinggi makin relevan.

---

## 6. Catatan teknis (untuk pengembang)

- **API candle 0.10.2**: `candle_transformers::models::bert::{BertModel, Config,
  DTYPE}`; `VarBuilder::from_mmaped_safetensors` (unsafe, mmap);
  `model.forward(input_ids, token_type_ids, Some(&attention_mask))` (3 argumen di
  versi ini); ekstraksi hasil via `tensor.to_vec2::<f32>()`.
- **candle itu pure-Rust** — tidak memakai ONNX Runtime (itu `fastembed`).
- **Embedder global**: model dimuat **sekali** (lazy `static Mutex<Option<...>>`)
  lalu dipakai berulang — pemuatan model mahal, inferensi sesudahnya murah.
- **Blocking**: inferensi candle sinkron/CPU-bound. Saat ini dijalankan di dalam
  `io_lock` guard server (cukup untuk beban single-user). Bila perlu paralel,
  pindahkan ke `tokio::task::spawn_blocking`.
- **Test**: build default tetap **27 test, 0 warning** (3 test embed: hash,
  cosine, error-tanpa-feature). Build `--features semantic`: **26 test, 0
  warning**. Helper index diberi `#[cfg_attr(not(feature="semantic"),
  allow(dead_code))]` agar build default bersih.

---

## 7. Ide pengembangan lanjutan

- Naikkan `memory_suggest` & `memory_cluster` agar memakai vektor embedding
  (bukan TF-IDF) → relasi & tema berbasis makna.
- Tool `recall` terpadu: semantic search → ambil top-K → sertakan tetangga graf
  & tema → satu payload konteks siap pakai untuk agent.
- Pencarian semantik **lintas-project**.
