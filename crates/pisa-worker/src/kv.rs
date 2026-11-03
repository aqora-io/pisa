//! A transactional key/value store for the worker's metadata records.
//!
//! Keys are tuples of strings, encoded so that byte order is tuple order and
//! every key extending a tuple falls in one contiguous range (see [`key`] and
//! [`prefix`]). Backends are in-memory, for development and tests, and TiKV.
//! A [`Txn`] reads and writes through the backend and takes effect when
//! committed; one dropped without [`Txn::commit`] changes nothing.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedMutexGuard};

/// The first component of every key the worker stores.
pub const ROOT: &str = "pisa";

/// The most times [`retry`] runs an attempt.
const MAX_ATTEMPTS: usize = 8;

/// Encodes a tuple of strings: each component's bytes with `0x00` written as
/// `0x00 0xFF`, then a `0x00` terminator.
pub fn key(parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        for &b in part.as_bytes() {
            out.push(b);
            if b == 0 {
                out.push(0xFF);
            }
        }
        out.push(0);
    }
    out
}

/// The range of every key that extends `parts`, `parts` itself included.
pub fn prefix(parts: &[&str]) -> Range<Vec<u8>> {
    let start = key(parts);
    let end = [start.as_slice(), &[0xFF]].concat();
    start..end
}

/// The inverse of [`key`].
pub fn decode(key: &[u8]) -> Vec<String> {
    let mut parts = Vec::new();
    let mut part = Vec::new();
    let mut bytes = key.iter().copied().peekable();
    while let Some(b) = bytes.next() {
        if b != 0 {
            part.push(b);
        } else if bytes.next_if_eq(&0xFF).is_some() {
            part.push(0);
        } else {
            parts.push(String::from_utf8_lossy(&std::mem::take(&mut part)).into_owned());
        }
    }
    parts
}

#[derive(Debug, thiserror::Error)]
pub enum KvError {
    /// Another transaction changed what this one read or wrote; run it again.
    #[error("transaction conflict")]
    Conflict,
    #[error("{0}")]
    Other(String),
}

/// Whether an error means the transaction lost a race and may be retried.
pub trait IsConflict {
    fn is_conflict(&self) -> bool;
}

impl IsConflict for KvError {
    fn is_conflict(&self) -> bool {
        matches!(self, KvError::Conflict)
    }
}

/// Runs `attempt` (which begins and commits its own transaction) up to 8
/// times while it fails with a conflict.
pub async fn retry<T, E: IsConflict, F: Future<Output = Result<T, E>>>(
    mut attempt: impl FnMut() -> F,
) -> Result<T, E> {
    let mut attempts = 1;
    loop {
        match attempt().await {
            Err(e) if e.is_conflict() && attempts < MAX_ATTEMPTS => attempts += 1,
            result => return result,
        }
    }
}

#[derive(Clone)]
pub enum Kv {
    Memory(MemoryKv),
}

impl Kv {
    /// An empty in-memory store, shared by its clones.
    pub fn memory() -> Kv {
        Kv::Memory(MemoryKv::default())
    }

    /// Starts a transaction.
    ///
    /// An in-memory transaction holds the store until it is committed or
    /// dropped, so never begin a second one while holding the first.
    pub async fn begin(&self) -> Result<Txn, KvError> {
        match self {
            Kv::Memory(kv) => Ok(Txn(TxnInner::Memory(kv.begin().await))),
        }
    }
}

pub struct Txn(TxnInner);

enum TxnInner {
    Memory(MemoryTxn),
}

impl Txn {
    /// Reads a key, seeing this transaction's own writes.
    pub async fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        match &mut self.0 {
            TxnInner::Memory(t) => Ok(t.get(key)),
        }
    }

    /// Reads up to `limit` pairs in `range`, in key order.
    pub async fn scan(
        &mut self,
        range: Range<Vec<u8>>,
        limit: u32,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, KvError> {
        match &mut self.0 {
            TxnInner::Memory(t) => Ok(t.scan(range, limit)),
        }
    }

    pub async fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), KvError> {
        match &mut self.0 {
            TxnInner::Memory(t) => {
                t.put(key, value);
                Ok(())
            }
        }
    }

    pub async fn delete(&mut self, key: Vec<u8>) -> Result<(), KvError> {
        match &mut self.0 {
            TxnInner::Memory(t) => {
                t.delete(key);
                Ok(())
            }
        }
    }

    /// Makes concurrent transactions that touch `key` conflict with this one,
    /// without writing it.
    pub async fn lock(&mut self, _key: Vec<u8>) -> Result<(), KvError> {
        match &mut self.0 {
            TxnInner::Memory(_) => Ok(()),
        }
    }

    pub async fn commit(self) -> Result<(), KvError> {
        match self.0 {
            TxnInner::Memory(t) => {
                t.commit();
                Ok(())
            }
        }
    }
}

type Map = BTreeMap<Vec<u8>, Vec<u8>>;

#[derive(Clone, Default)]
pub struct MemoryKv(Arc<Mutex<Map>>);

impl MemoryKv {
    async fn begin(&self) -> MemoryTxn {
        MemoryTxn {
            map: self.0.clone().lock_owned().await,
            undo: Vec::new(),
        }
    }
}

/// Writes go straight into the map; `undo` holds each overwritten value, and
/// dropping the transaction replays it unless the transaction was committed.
struct MemoryTxn {
    map: OwnedMutexGuard<Map>,
    undo: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

impl MemoryTxn {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.map.get(key).cloned()
    }

    fn scan(&self, range: Range<Vec<u8>>, limit: u32) -> Vec<(Vec<u8>, Vec<u8>)> {
        if range.start > range.end {
            return Vec::new();
        }
        self.map
            .range(range)
            .take(limit as usize)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        let before = self.map.insert(key.clone(), value);
        self.undo.push((key, before));
    }

    fn delete(&mut self, key: Vec<u8>) {
        let before = self.map.remove(&key);
        self.undo.push((key, before));
    }

    fn commit(mut self) {
        self.undo.clear();
    }
}

impl Drop for MemoryTxn {
    fn drop(&mut self) {
        for (key, before) in self.undo.drain(..).rev() {
            match before {
                Some(value) => self.map.insert(key, value),
                None => self.map.remove(&key),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sort_like_their_tuples() {
        assert!(key(&["a"]) < key(&["a", ""]));
        assert!(key(&["a", ""]) < key(&["a", "b"]));
        assert!(key(&["a", "b"]) < key(&["ab"]));

        let under_a = prefix(&["a"]);
        assert!(under_a.contains(&key(&["a"])));
        assert!(under_a.contains(&key(&["a", "\0b"])));
        assert!(!under_a.contains(&key(&["a\0x"])));
        assert!(!under_a.contains(&key(&["ab"])));

        let parts = ["v", "a\0b", ""];
        assert_eq!(decode(&key(&parts)), parts);

        let nine = key(&["p", &format!("{:016x}", 9)]);
        let ten = key(&["p", &format!("{:016x}", 10)]);
        assert!(nine < ten);
    }

    #[tokio::test]
    async fn memory_transactions_commit_or_roll_back() {
        let kv = Kv::memory();
        let k = key(&["t", "a"]);

        let mut txn = kv.begin().await.unwrap();
        txn.put(k.clone(), b"one".to_vec()).await.unwrap();
        assert_eq!(txn.get(&k).await.unwrap(), Some(b"one".to_vec()));
        txn.commit().await.unwrap();

        let mut txn = kv.begin().await.unwrap();
        assert_eq!(txn.get(&k).await.unwrap(), Some(b"one".to_vec()));
        txn.put(k.clone(), b"two".to_vec()).await.unwrap();
        assert_eq!(txn.get(&k).await.unwrap(), Some(b"two".to_vec()));
        txn.delete(k.clone()).await.unwrap();
        assert_eq!(txn.get(&k).await.unwrap(), None);
        drop(txn);

        let mut txn = kv.begin().await.unwrap();
        assert_eq!(txn.get(&k).await.unwrap(), Some(b"one".to_vec()));
        let other = key(&["t", "b"]);
        txn.put(other.clone(), b"x".to_vec()).await.unwrap();
        drop(txn);

        let mut txn = kv.begin().await.unwrap();
        assert_eq!(txn.get(&other).await.unwrap(), None);
        txn.delete(k.clone()).await.unwrap();
        txn.commit().await.unwrap();

        let mut txn = kv.begin().await.unwrap();
        assert_eq!(txn.get(&k).await.unwrap(), None);
    }

    #[tokio::test]
    async fn scans_respect_ranges_and_limits() {
        let kv = Kv::memory();
        let mut txn = kv.begin().await.unwrap();
        for (a, b) in [("t", "a"), ("t", "b"), ("t", "c"), ("u", "a")] {
            txn.put(key(&[a, b]), b.as_bytes().to_vec()).await.unwrap();
        }
        txn.commit().await.unwrap();

        let mut txn = kv.begin().await.unwrap();
        let two = txn.scan(prefix(&["t"]), 2).await.unwrap();
        assert_eq!(
            two,
            vec![
                (key(&["t", "a"]), b"a".to_vec()),
                (key(&["t", "b"]), b"b".to_vec()),
            ]
        );
        let all = txn.scan(prefix(&["t"]), 10).await.unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|(k, _)| decode(k)[0] == "t"));
        let backwards = txn.scan(key(&["u"])..key(&["t"]), 10).await.unwrap();
        assert!(backwards.is_empty());
    }

    #[tokio::test]
    async fn retry_reruns_on_conflict() {
        let mut calls = 0;
        let result = retry(|| {
            calls += 1;
            let n = calls;
            async move {
                if n <= 2 {
                    Err(KvError::Conflict)
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert!(matches!(result, Ok(7)));
        assert_eq!(calls, 3);

        let mut calls = 0;
        let result: Result<(), KvError> = retry(|| {
            calls += 1;
            async { Err(KvError::Conflict) }
        })
        .await;
        assert!(matches!(result, Err(KvError::Conflict)));
        assert_eq!(calls, 8);

        let mut calls = 0;
        let result: Result<(), KvError> = retry(|| {
            calls += 1;
            async { Err(KvError::Other("boom".into())) }
        })
        .await;
        assert!(matches!(result, Err(KvError::Other(_))));
        assert_eq!(calls, 1);
    }
}
