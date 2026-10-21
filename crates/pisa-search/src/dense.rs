//! The dense lane: embedding similarity, through a Text Embeddings Inference
//! (TEI) server running an instruction-aware model (Qwen3-Embedding).
//!
//! Documents are embedded once, cached by (model, text hash) in object
//! storage, and stored in bundles as int8 vectors truncated to `DIMS`
//! (Matryoshka). Search is exact brute force: fine for the tens of thousands
//! of vectors embedded on CPU.

use std::collections::HashMap;
use std::io::{Read, Write};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::corpus::Decl;
use crate::index::StoredDecl;

/// Matryoshka truncation of the model's output.
pub const DIMS: usize = 512;

/// Qwen3-Embedding's query instruction; documents get none.
pub const QUERY_INSTRUCTION: &str = "Instruct: Given a description of a mathematical or physical statement, retrieve the Lean declaration that states it\nQuery: ";

/// The text a declaration is embedded as, in LeanSearch v2's style: header,
/// informal block, formal block. Short, for CPU embedding.
pub fn document_view(d: &Decl, informal: Option<&pisa_protocol::search::Informal>) -> String {
    let mut s = format!("{} {}\n", d.kind, d.name);
    if let Some(i) = informal {
        s.push_str(&i.informal_name);
        s.push('\n');
        s.push_str(&i.statement_nl);
        s.push('\n');
    } else if let Some(doc) = &d.docstring {
        s.push_str(doc);
        s.push('\n');
    }
    s.push_str(&d.signature);
    s.chars().take(600).collect()
}

pub fn text_hash(model: &str, text: &str) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update(model.as_bytes());
    h.update([0]);
    h.update(text.as_bytes());
    h.finalize()[..16].try_into().expect("16 bytes")
}

/// Normalizes after truncation, so dot products are cosines.
pub fn truncate_normalize(mut v: Vec<f32>) -> Vec<f32> {
    v.truncate(DIMS);
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= norm);
    v
}

pub fn quantize(v: &[f32]) -> Vec<i8> {
    v.iter()
        .map(|x| (x * 127.0).round().clamp(-127.0, 127.0) as i8)
        .collect()
}

#[derive(Clone)]
pub struct Tei {
    url: String,
    http: reqwest::blocking::Client,
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    inputs: &'a [String],
    normalize: bool,
    truncate: bool,
}

impl Tei {
    pub fn new(url: &str) -> Tei {
        Tei {
            url: url.trim_end_matches('/').to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .expect("http client"),
        }
    }

    pub fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let req = EmbedRequest {
            inputs: texts,
            normalize: true,
            truncate: true,
        };
        let r = self
            .http
            .post(format!("{}/embed", self.url))
            .json(&req)
            .send()?;
        let r = r.error_for_status()?;
        Ok(r.json::<Vec<Vec<f32>>>()?
            .into_iter()
            .map(truncate_normalize)
            .collect())
    }

    /// The model id TEI reports.
    pub fn model(&self) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Info {
            model_id: String,
        }
        Ok(self
            .http
            .get(format!("{}/info", self.url))
            .send()?
            .error_for_status()?
            .json::<Info>()?
            .model_id)
    }
}

/// Cached document vectors: `vectors/<batch>.bin.zst` objects of
/// (16-byte text hash, `DIMS` f32) records.
pub fn encode_batch(vectors: &[([u8; 16], Vec<f32>)]) -> anyhow::Result<Vec<u8>> {
    let mut enc = zstd::Encoder::new(Vec::new(), 3)?;
    for (hash, v) in vectors {
        anyhow::ensure!(
            v.len() == DIMS,
            "vector has {} dims, expected {DIMS}",
            v.len()
        );
        enc.write_all(hash)?;
        for x in v {
            enc.write_all(&x.to_le_bytes())?;
        }
    }
    Ok(enc.finish()?)
}

pub fn decode_batch(bytes: &[u8], into: &mut HashMap<[u8; 16], Vec<f32>>) -> anyhow::Result<()> {
    let mut data = Vec::new();
    zstd::Decoder::new(bytes)?.read_to_end(&mut data)?;
    let record = 16 + 4 * DIMS;
    anyhow::ensure!(data.len() % record == 0, "truncated vector batch");
    for chunk in data.chunks_exact(record) {
        let hash: [u8; 16] = chunk[..16].try_into().expect("16 bytes");
        let v = chunk[16..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect();
        into.insert(hash, v);
    }
    Ok(())
}

/// Vectors for the declarations that have them, and a way to embed queries.
pub struct Dense {
    pub model: String,
    ids: Vec<u32>,
    vectors: Vec<i8>,
    tei: Option<Tei>,
}

#[derive(Serialize, Deserialize)]
struct VectorsHeader {
    model: String,
    dims: usize,
    count: usize,
}

impl Dense {
    pub fn new(model: String, entries: Vec<(u32, Vec<i8>)>) -> Dense {
        let mut ids = Vec::with_capacity(entries.len());
        let mut vectors = Vec::with_capacity(entries.len() * DIMS);
        for (id, v) in entries {
            ids.push(id);
            vectors.extend(v);
        }
        Dense {
            model,
            ids,
            vectors,
            tei: None,
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn with_tei(mut self, tei: Tei) -> Dense {
        self.tei = Some(tei);
        self
    }

    /// `vectors.bin`: a JSON header line, then u32 ids, then int8 vectors.
    pub fn write(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        let header = VectorsHeader {
            model: self.model.clone(),
            dims: DIMS,
            count: self.ids.len(),
        };
        serde_json::to_writer(&mut f, &header)?;
        f.write_all(b"\n")?;
        for id in &self.ids {
            f.write_all(&id.to_le_bytes())?;
        }
        f.write_all(&self.vectors.iter().map(|&x| x as u8).collect::<Vec<u8>>())?;
        Ok(())
    }

    pub fn read(path: &std::path::Path) -> anyhow::Result<Dense> {
        let data = std::fs::read(path)?;
        let nl = data
            .iter()
            .position(|&b| b == b'\n')
            .ok_or_else(|| anyhow::anyhow!("no header"))?;
        let header: VectorsHeader = serde_json::from_slice(&data[..nl])?;
        anyhow::ensure!(
            header.dims == DIMS,
            "vectors have {} dims, expected {DIMS}",
            header.dims
        );
        let body = &data[nl + 1..];
        let ids_len = header.count * 4;
        anyhow::ensure!(
            body.len() == ids_len + header.count * DIMS,
            "truncated vectors.bin"
        );
        let ids = body[..ids_len]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| u32::from_le_bytes(b))
            .collect();
        let vectors = body[ids_len..].iter().map(|&b| b as i8).collect();
        Ok(Dense {
            model: header.model,
            ids,
            vectors,
            tei: None,
        })
    }

    /// Ids of the `n` vectors nearest to `query` (a normalized vector).
    pub fn nearest(&self, query: &[f32], n: usize) -> Vec<u32> {
        self.nearest_quantized(&quantize(query), n)
    }

    fn nearest_quantized(&self, q: &[i8], n: usize) -> Vec<u32> {
        let mut scored: Vec<(i32, u32)> = self
            .vectors
            .par_chunks_exact(DIMS)
            .zip(self.ids.par_iter())
            .map(|(v, &id)| {
                (
                    v.iter()
                        .zip(q)
                        .map(|(&a, &b)| i32::from(a) * i32::from(b))
                        .sum(),
                    id,
                )
            })
            .collect();
        let n = n.min(scored.len());
        if n == 0 {
            return Vec::new();
        }
        scored.select_nth_unstable_by(n - 1, |a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(n);
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, id)| id).collect()
    }

    /// Declaration ids nearest to `text`, or `None` without an embedder.
    pub fn search(&self, text: &str, n: usize) -> Option<Vec<u32>> {
        let tei = self.tei.as_ref()?;
        let query = format!("{QUERY_INSTRUCTION}{text}");
        match tei.embed(&[query]) {
            Ok(mut v) if !v.is_empty() => Some(self.nearest(&v.swap_remove(0), n)),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!("query embedding failed: {e}");
                None
            }
        }
    }
}

/// Where a model's cached vectors live in the store.
pub fn cache_dir(model: &str) -> String {
    format!("vectors/{}", model.replace('/', "--"))
}

/// Every cached vector for `model`, by text hash.
pub async fn load_cache(
    store: &crate::informal_store::Store,
    model: &str,
) -> anyhow::Result<HashMap<[u8; 16], Vec<f32>>> {
    let mut out = HashMap::new();
    for bytes in store.get_all(&cache_dir(model)).await? {
        decode_batch(&bytes, &mut out)?;
    }
    Ok(out)
}

/// Document vectors by declaration name, for the declarations that have one.
pub fn vectors_by_name(
    decls: &[Decl],
    informal: &HashMap<String, pisa_protocol::search::Informal>,
    model: &str,
    cache: &HashMap<[u8; 16], Vec<f32>>,
) -> HashMap<String, Vec<f32>> {
    decls
        .iter()
        .filter_map(|d| {
            let text = document_view(d, informal.get(&d.name));
            Some((d.name.clone(), cache.get(&text_hash(model, &text))?.clone()))
        })
        .collect()
}

/// Declarations in `libraries`, plus (with `with_deps`) everything they use
/// directly, in corpus order.
pub fn select<'a>(decls: &'a [Decl], libraries: &[String], with_deps: bool) -> Vec<&'a Decl> {
    let mut wanted: std::collections::HashSet<&str> = decls
        .iter()
        .filter(|d| libraries.contains(&d.library))
        .map(|d| d.name.as_str())
        .collect();
    if with_deps {
        let deps: Vec<&str> = decls
            .iter()
            .filter(|d| wanted.contains(d.name.as_str()))
            .flat_map(|d| d.type_refs.iter().chain(&d.value_refs).map(String::as_str))
            .collect();
        wanted.extend(deps);
    }
    decls
        .iter()
        .filter(|d| wanted.contains(d.name.as_str()))
        .collect()
}

/// Embeds the document views not yet cached, saving a batch object every
/// `save_every` documents so an interrupted run keeps its progress.
pub async fn embed_missing(
    store: &crate::informal_store::Store,
    tei: &Tei,
    model: &str,
    docs: Vec<String>,
    batch: usize,
    save_every: usize,
) -> anyhow::Result<usize> {
    let cache = load_cache(store, model).await?;
    let mut todo: Vec<String> = docs
        .into_iter()
        .filter(|t| !cache.contains_key(&text_hash(model, t)))
        .collect();
    todo.sort();
    todo.dedup();
    let total = todo.len();
    tracing::info!(
        cached = cache.len(),
        to_embed = total,
        "embedding document views"
    );
    let started = std::time::Instant::now();
    let mut done = 0;
    for chunk in todo.chunks(save_every) {
        let mut pending = Vec::with_capacity(chunk.len());
        for texts in chunk.chunks(batch) {
            let texts = texts.to_vec();
            let tei = tei.clone();
            let vectors =
                tokio::task::spawn_blocking(move || tei.embed(&texts).map(|v| (texts, v)))
                    .await??;
            for (t, v) in vectors.0.iter().zip(vectors.1) {
                pending.push((text_hash(model, t), v));
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        store
            .put(
                &format!("{}/{stamp}.bin.zst", cache_dir(model)),
                encode_batch(&pending)?,
            )
            .await?;
        done += pending.len();
        let rate = done as f64 / started.elapsed().as_secs_f64();
        tracing::info!(done, total, rate = format!("{rate:.1}/s"), "saved vectors");
    }
    Ok(done)
}

/// Which stored declarations a dense index covers, for labelling results.
pub fn coverage(decls: &[StoredDecl], dense: &Dense) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    for &id in &dense.ids {
        *out.entry(decls[id as usize].library.clone()).or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(xs: &[f32]) -> Vec<f32> {
        let mut v = vec![0.0; DIMS];
        v[..xs.len()].copy_from_slice(xs);
        truncate_normalize(v)
    }

    #[test]
    fn nearest_ranks_by_cosine_and_round_trips() {
        let d = Dense::new(
            "m".into(),
            vec![
                (7, quantize(&unit(&[1.0, 0.0]))),
                (8, quantize(&unit(&[0.6, 0.8]))),
                (9, quantize(&unit(&[0.0, 1.0]))),
            ],
        );
        assert_eq!(d.nearest(&unit(&[0.0, 1.0]), 2), [9, 8]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.bin");
        d.write(&path).unwrap();
        let back = Dense::read(&path).unwrap();
        assert_eq!(back.nearest(&unit(&[1.0, 0.1]), 3), [7, 8, 9]);
        assert_eq!(back.model, "m");
    }

    #[test]
    fn vector_batches_round_trip() {
        let v = unit(&[0.3, 0.4]);
        let bytes = encode_batch(&[([1; 16], v.clone())]).unwrap();
        let mut map = HashMap::new();
        decode_batch(&bytes, &mut map).unwrap();
        assert_eq!(map[&[1; 16]], v);
    }
}
