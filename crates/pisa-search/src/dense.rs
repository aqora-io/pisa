//! The dense lane: embedding similarity, through a Text Embeddings Inference
//! (TEI) server or an OpenAI-compatible `/embeddings` endpoint running an
//! instruction-aware model (Qwen3-Embedding).
//!
//! Documents are embedded once, cached by (model, text hash) in object
//! storage, and stored in bundles as int8 vectors truncated to `DIMS`
//! (Matryoshka). Search is exact brute force: fine for the tens of thousands
//! of vectors embedded on CPU.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::corpus::Decl;

/// Matryoshka truncation of the model's output.
pub const DIMS: usize = 512;

/// Qwen3-Embedding's query instruction; documents get none.
pub const QUERY_INSTRUCTION: &str = "Instruct: Given a description of a mathematical or physical statement, retrieve the Lean declaration that states it\nQuery: ";

/// The most characters a declaration is embedded as.
pub const VIEW_CHARS: usize = 2000;

/// The text a declaration is embedded as, in LeanSearch v2's style: header,
/// informal block, formal block, in at most [`VIEW_CHARS`] characters. The
/// description is cut so that the signature fits; only a signature too long
/// on its own is cut.
pub fn document_view(d: &Decl, informal: Option<&pisa_protocol::search::Informal>) -> String {
    let head = format!("{} {}\n", d.kind, d.name);
    let mut description = String::new();
    if let Some(i) = informal {
        description.push_str(&i.informal_name);
        description.push('\n');
        description.push_str(&i.statement_nl);
        description.push('\n');
    } else if let Some(doc) = &d.docstring {
        description.push_str(doc);
        description.push('\n');
    }
    let room = VIEW_CHARS.saturating_sub(head.chars().count() + d.signature.chars().count());
    let mut s = head;
    s.extend(description.chars().take(room));
    s.push_str(&d.signature);
    s.chars().take(VIEW_CHARS).collect()
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

/// Where vectors come from.
#[derive(Clone)]
pub struct Embedder {
    url: String,
    api: Api,
    /// For bulk embedding: patient, and retries rate limits and server errors.
    http: reqwest::blocking::Client,
    /// For search queries: connects within 2 s, and never retries.
    query_http: reqwest::blocking::Client,
}

#[derive(Clone)]
enum Api {
    /// TEI's `/embed` and `/info`.
    Tei,
    /// `/embeddings` as OpenAI defines it (Scaleway, llama.cpp, ...).
    OpenAi { model: String, key: Option<String> },
}

#[derive(Serialize)]
struct TeiRequest<'a> {
    inputs: &'a [String],
    normalize: bool,
    truncate: bool,
}

#[derive(Deserialize)]
struct OpenAiResponse {
    data: Vec<OpenAiVector>,
}

#[derive(Deserialize)]
struct OpenAiVector {
    index: usize,
    embedding: Vec<f32>,
}

impl Embedder {
    /// A TEI server at `spec`, or, for
    /// `openai+<base url>?model=<model>[&key_env=<VAR>]`, an OpenAI-compatible
    /// endpoint whose bearer key is read from the environment variable `VAR`.
    pub fn parse(spec: &str) -> anyhow::Result<Embedder> {
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()?;
        let query_http = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(2))
            .build()?;
        let Some(rest) = spec.strip_prefix("openai+") else {
            return Ok(Embedder {
                url: spec.trim_end_matches('/').to_string(),
                api: Api::Tei,
                http,
                query_http,
            });
        };
        let mut url = url::Url::parse(rest)?;
        let mut model = None;
        let mut key = None;
        for (k, v) in url.query_pairs() {
            match k.as_ref() {
                "model" => model = Some(v.into_owned()),
                "key_env" => {
                    key = Some(
                        std::env::var(v.as_ref()).map_err(|_| anyhow::anyhow!("{v} is not set"))?,
                    )
                }
                other => anyhow::bail!("unknown embedder option {other}"),
            }
        }
        url.set_query(None);
        Ok(Embedder {
            url: url.as_str().trim_end_matches('/').to_string(),
            api: Api::OpenAi {
                model: model.ok_or_else(|| anyhow::anyhow!("{spec}: no model= given"))?,
                key,
            },
            http,
            query_http,
        })
    }

    pub fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.embed_with(&self.http, texts, None)
    }

    /// Search queries, in one request with no retries, given up after
    /// `timeout`.
    pub fn embed_queries(
        &self,
        texts: &[String],
        timeout: std::time::Duration,
    ) -> anyhow::Result<Vec<Vec<f32>>> {
        self.embed_with(&self.query_http, texts, Some(timeout))
    }

    fn embed_with(
        &self,
        http: &reqwest::blocking::Client,
        texts: &[String],
        // Set for queries, which fail fast instead of retrying.
        timeout: Option<std::time::Duration>,
    ) -> anyhow::Result<Vec<Vec<f32>>> {
        self.post(http, texts, timeout).map_err(|e| match timeout {
            // reqwest says only "error sending request" when it gives up.
            Some(t)
                if e.downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_timeout) =>
            {
                anyhow::anyhow!("the embedder didn't answer within {:.1} s", t.as_secs_f64())
            }
            _ => e,
        })
    }

    fn post(
        &self,
        http: &reqwest::blocking::Client,
        texts: &[String],
        timeout: Option<std::time::Duration>,
    ) -> anyhow::Result<Vec<Vec<f32>>> {
        let retry = timeout.is_none();
        let with_timeout = |r: reqwest::blocking::RequestBuilder| match timeout {
            Some(t) => r.timeout(t),
            None => r,
        };
        let vectors = match &self.api {
            Api::Tei => {
                let req = TeiRequest {
                    inputs: texts,
                    normalize: true,
                    truncate: true,
                };
                let r = with_timeout(http.post(format!("{}/embed", self.url)))
                    .json(&req)
                    .send()?
                    .error_for_status()?;
                let vectors = r.json::<Vec<Vec<f32>>>()?;
                anyhow::ensure!(
                    vectors.len() == texts.len(),
                    "got {} vectors for {} texts",
                    vectors.len(),
                    texts.len()
                );
                vectors
            }
            Api::OpenAi { model, key } => {
                let body = serde_json::json!({"model": model, "input": texts});
                let mut attempt = 0;
                let r = loop {
                    attempt += 1;
                    let mut req =
                        with_timeout(http.post(format!("{}/embeddings", self.url)).json(&body));
                    if let Some(key) = key {
                        req = req.bearer_auth(key);
                    }
                    let r = req.send()?;
                    let status = r.status().as_u16();
                    if retry && (status == 429 || status >= 500) && attempt < 6 {
                        tracing::warn!(status, attempt, "embedding request failed, retrying");
                        std::thread::sleep(std::time::Duration::from_secs(1 << attempt));
                        continue;
                    }
                    break r.error_for_status()?;
                };
                let mut data = r.json::<OpenAiResponse>()?.data;
                data.sort_by_key(|v| v.index);
                anyhow::ensure!(
                    data.len() == texts.len(),
                    "got {} vectors for {} texts",
                    data.len(),
                    texts.len()
                );
                data.into_iter().map(|v| v.embedding).collect()
            }
        };
        Ok(vectors.into_iter().map(truncate_normalize).collect())
    }

    /// The model it serves: as TEI reports it, or as configured.
    pub fn model(&self) -> anyhow::Result<String> {
        #[derive(Deserialize)]
        struct Info {
            model_id: String,
        }
        match &self.api {
            Api::Tei => Ok(self
                .http
                .get(format!("{}/info", self.url))
                .send()?
                .error_for_status()?
                .json::<Info>()?
                .model_id),
            Api::OpenAi { model, .. } => Ok(model.clone()),
        }
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
}

#[derive(Serialize, Deserialize)]
struct VectorsHeader {
    model: String,
    dims: usize,
    count: usize,
}

impl Dense {
    pub fn new(model: String, mut entries: Vec<(u32, Vec<i8>)>) -> Dense {
        // Sorted, so `contains` can binary-search.
        entries.sort_by_key(|(id, _)| *id);
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
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn contains(&self, id: u32) -> bool {
        self.ids.binary_search(&id).is_ok()
    }

    /// Declaration `id`'s quantized vector, if it has one.
    pub fn get(&self, id: u32) -> Option<&[i8]> {
        let i = self.ids.binary_search(&id).ok()?;
        Some(&self.vectors[i * DIMS..(i + 1) * DIMS])
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
        let ids: Vec<u32> = body[..ids_len]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| u32::from_le_bytes(b))
            .collect();
        anyhow::ensure!(ids.is_sorted(), "vector ids are out of order");
        let vectors = body[ids_len..].iter().map(|&b| b as i8).collect();
        Ok(Dense {
            model: header.model,
            ids,
            vectors,
        })
    }

    /// Ids of the `n` vectors nearest to `query` (a normalized vector).
    pub fn nearest(&self, query: &[f32], n: usize) -> Vec<u32> {
        self.nearest_quantized(&quantize(query), n)
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    /// The `n` declarations nearest to declaration `id`, or `None` if it has no vector.
    pub fn neighbours(&self, id: u32, n: usize) -> Option<Vec<u32>> {
        let i = self.ids.iter().position(|&x| x == id)?;
        let q = self.vectors[i * DIMS..(i + 1) * DIMS].to_vec();
        let mut near: Vec<u32> = self
            .nearest_quantized(&q, n + 1)
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        near.retain(|&x| x != id);
        near.truncate(n);
        Some(near)
    }

    fn nearest_quantized(&self, q: &[i8], n: usize) -> Vec<(i32, u32)> {
        let scored: Vec<(i32, u32)> = self
            .vectors
            .par_chunks_exact(DIMS)
            .zip(self.ids.par_iter())
            .map(|(v, &id)| (dot(v, q), id))
            .collect();
        best(scored, n)
    }
}

/// The dot product of two quantized vectors.
fn dot(a: &[i8], b: &[i8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| i32::from(x) * i32::from(y))
        .sum()
}

/// The `n` best of `(score, id)`s, best first, ties by id.
fn best(mut scored: Vec<(i32, u32)>, n: usize) -> Vec<(i32, u32)> {
    let n = n.min(scored.len());
    if n == 0 {
        return Vec::new();
    }
    scored.select_nth_unstable_by(n - 1, |a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(n);
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
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

/// Vectors saved per cache object, so an interrupted run keeps its progress.
const SAVE_EVERY: usize = 1000;

/// Hashes of the texts a model's cache already has vectors for.
pub async fn cached_hashes(
    store: &crate::informal_store::Store,
    model: &str,
) -> anyhow::Result<HashSet<[u8; 16]>> {
    let mut out = HashSet::new();
    for bytes in store.get_all(&cache_dir(model)).await? {
        let mut batch = HashMap::new();
        decode_batch(&bytes, &mut batch)?;
        out.extend(batch.into_keys());
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
/// `SAVE_EVERY` documents so an interrupted run keeps its progress.
pub async fn embed_missing(
    store: &crate::informal_store::Store,
    embedder: &Embedder,
    model: &str,
    docs: Vec<String>,
    known: &mut HashSet<[u8; 16]>,
    batch: usize,
    concurrency: usize,
) -> anyhow::Result<usize> {
    use futures::StreamExt;
    let mut todo: Vec<String> = docs
        .into_iter()
        .filter(|t| !known.contains(&text_hash(model, t)))
        .collect();
    todo.sort();
    todo.dedup();
    let total = todo.len();
    tracing::info!(
        cached = known.len(),
        to_embed = total,
        "embedding document views"
    );
    let started = std::time::Instant::now();
    let mut done = 0;
    for chunk in todo.chunks(SAVE_EVERY) {
        let mut pending = Vec::with_capacity(chunk.len());
        // `concurrency` requests in flight at once.
        let mut results = futures::stream::iter(chunk.chunks(batch).map(|texts| {
            let texts = texts.to_vec();
            let embedder = embedder.clone();
            tokio::task::spawn_blocking(move || embedder.embed(&texts).map(|v| (texts, v)))
        }))
        .buffer_unordered(concurrency.max(1));
        while let Some(r) = results.next().await {
            let (texts, vectors) = r??;
            for (t, v) in texts.iter().zip(vectors) {
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
        known.extend(pending.iter().map(|(h, _)| *h));
        done += pending.len();
        let rate = done as f64 / started.elapsed().as_secs_f64();
        tracing::info!(done, total, rate = format!("{rate:.1}/s"), "saved vectors");
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(signature: &str) -> Decl {
        Decl {
            name: "t".into(),
            kind: "theorem".into(),
            signature: signature.into(),
            ..Default::default()
        }
    }

    fn described(statement: &str) -> pisa_protocol::search::Informal {
        pisa_protocol::search::Informal {
            informal_name: "A name".into(),
            statement_nl: statement.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_view_keeps_its_signature_and_cuts_the_description() {
        // One that fits is as it was, so its vector is kept.
        let short = document_view(&decl("t : 1 = 1"), Some(&described("One is one.")));
        assert_eq!(short, "theorem t\nA name\nOne is one.\nt : 1 = 1");
        // A long description is cut so that the whole signature fits.
        let signature = format!("t : {} = 1", "x".repeat(900));
        let view = document_view(&decl(&signature), Some(&described(&"y".repeat(3000))));
        assert_eq!(view.chars().count(), 2000);
        assert!(view.ends_with(&signature), "{view}");
        // A signature too long on its own is cut last.
        let signature = "z".repeat(5000);
        let view = document_view(&decl(&signature), Some(&described("short")));
        assert_eq!(view.chars().count(), 2000);
        assert!(view.starts_with("theorem t\nz"), "{view}");
    }

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
    fn a_query_that_times_out_says_so() {
        // Connections queue on the listener, and nothing ever answers them.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let e = Embedder::parse(&format!("http://{}", silent.local_addr().unwrap())).unwrap();
        let timeout = std::time::Duration::from_millis(300);
        let err = e
            .embed_with(&e.query_http, &["x".to_string()], Some(timeout))
            .unwrap_err();
        assert_eq!(err.to_string(), "the embedder didn't answer within 0.3 s");
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
