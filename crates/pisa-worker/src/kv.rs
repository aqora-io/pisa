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

use tikv_client::{CheckLevel, TransactionClient, TransactionOptions};
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

impl From<tikv_client::Error> for KvError {
    fn from(e: tikv_client::Error) -> Self {
        if is_tikv_conflict(&e) {
            KvError::Conflict
        } else {
            KvError::Other(e.to_string())
        }
    }
}

/// Whether TiKV rejected the request because of contention: a newer write, a
/// live lock held by another transaction, or a reply asking to retry.
fn is_tikv_conflict(e: &tikv_client::Error) -> bool {
    use tikv_client::Error::{ExtractedErrors, KeyError, MultipleKeyErrors, ResolveLockError};
    match e {
        KeyError(k) => k.conflict.is_some() || !k.retryable.is_empty(),
        ResolveLockError(_) => true,
        MultipleKeyErrors(errors) | ExtractedErrors(errors) => errors.iter().any(is_tikv_conflict),
        _ => false,
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
    Tikv(TikvKv),
}

impl Kv {
    /// An empty in-memory store, shared by its clones.
    pub fn memory() -> Kv {
        Kv::Memory(MemoryKv::default())
    }

    /// Connects to the TiKV cluster behind the given PD endpoints.
    pub async fn tikv(pd_endpoints: &[String]) -> Result<Kv, KvError> {
        let client = TransactionClient::new(pd_endpoints.to_vec()).await?;
        Ok(Kv::Tikv(TikvKv(client)))
    }

    /// Starts a transaction.
    ///
    /// An in-memory transaction holds the store until it is committed or
    /// dropped, so never begin a second one while holding the first.
    pub async fn begin(&self) -> Result<Txn, KvError> {
        match self {
            Kv::Memory(kv) => Ok(Txn(TxnInner::Memory(kv.begin().await))),
            Kv::Tikv(kv) => Ok(Txn(TxnInner::Tikv(kv.begin().await?))),
        }
    }
}

pub struct Txn(TxnInner);

enum TxnInner {
    Memory(MemoryTxn),
    Tikv(TikvTxn),
}

impl Txn {
    /// Reads a key, seeing this transaction's own writes.
    pub async fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        match &mut self.0 {
            TxnInner::Memory(t) => Ok(t.get(key)),
            TxnInner::Tikv(t) => t.get(key).await,
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
            TxnInner::Tikv(t) => t.scan(range, limit).await,
        }
    }

    pub async fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), KvError> {
        match &mut self.0 {
            TxnInner::Memory(t) => {
                t.put(key, value);
                Ok(())
            }
            TxnInner::Tikv(t) => t.put(key, value).await,
        }
    }

    pub async fn delete(&mut self, key: Vec<u8>) -> Result<(), KvError> {
        match &mut self.0 {
            TxnInner::Memory(t) => {
                t.delete(key);
                Ok(())
            }
            TxnInner::Tikv(t) => t.delete(key).await,
        }
    }

    /// Makes concurrent transactions that touch `key` conflict with this one,
    /// without writing it.
    pub async fn lock(&mut self, key: Vec<u8>) -> Result<(), KvError> {
        match &mut self.0 {
            TxnInner::Memory(_) => Ok(()),
            TxnInner::Tikv(t) => t.lock(key).await,
        }
    }

    pub async fn commit(self) -> Result<(), KvError> {
        match self.0 {
            TxnInner::Memory(t) => {
                t.commit();
                Ok(())
            }
            TxnInner::Tikv(t) => t.commit().await,
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

/// Optimistic transactions against a TiKV cluster.
#[derive(Clone)]
pub struct TikvKv(TransactionClient);

impl TikvKv {
    async fn begin(&self) -> Result<TikvTxn, KvError> {
        let options = TransactionOptions::new_optimistic().drop_check(CheckLevel::None);
        Ok(TikvTxn(Box::new(self.0.begin_with_options(options).await?)))
    }
}

/// An optimistic transaction: writes are buffered client-side and sent when
/// it commits, so dropping one leaves nothing behind in the cluster.
struct TikvTxn(Box<tikv_client::Transaction>);

impl TikvTxn {
    async fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, KvError> {
        Ok(self.0.get(key.to_vec()).await?)
    }

    async fn scan(
        &mut self,
        range: Range<Vec<u8>>,
        limit: u32,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, KvError> {
        // tikv-client panics on a range that ends before it starts.
        if range.start >= range.end {
            return Ok(Vec::new());
        }
        let pairs = self.0.scan(range, limit).await?;
        Ok(pairs
            .map(|pair| {
                let (key, value) = pair.into();
                (key.into(), value)
            })
            .collect())
    }

    async fn put(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), KvError> {
        Ok(self.0.put(key, value).await?)
    }

    async fn delete(&mut self, key: Vec<u8>) -> Result<(), KvError> {
        Ok(self.0.delete(key).await?)
    }

    async fn lock(&mut self, key: Vec<u8>) -> Result<(), KvError> {
        Ok(self.0.lock_keys([key]).await?)
    }

    /// When TiKV answers a commit with a conflict, the transaction is rolled
    /// back so that the locks its prewrite left do not hold up later
    /// transactions until they expire. Any other failure, a lost reply above
    /// all, leaves it unknown whether the primary key committed, and rolling
    /// back the rest then could leave a half-applied transaction, so nothing
    /// is sent.
    async fn commit(mut self) -> Result<(), KvError> {
        match self.0.commit().await {
            Ok(_) => Ok(()),
            Err(e) => {
                if is_tikv_conflict(&e) {
                    let _ = self.0.rollback().await;
                }
                Err(e.into())
            }
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

    /// Every key it writes is under `root`, so runs can share a store.
    async fn contract(kv: Kv, root: &str) {
        let k = key(&[root, "a"]);

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
        let other = key(&[root, "b"]);
        txn.put(other.clone(), b"x".to_vec()).await.unwrap();
        drop(txn);

        let mut txn = kv.begin().await.unwrap();
        assert_eq!(txn.get(&other).await.unwrap(), None);
        txn.delete(k.clone()).await.unwrap();
        txn.commit().await.unwrap();

        let mut txn = kv.begin().await.unwrap();
        assert_eq!(txn.get(&k).await.unwrap(), None);
        drop(txn);

        let mut txn = kv.begin().await.unwrap();
        for (a, b) in [("t", "a"), ("t", "b"), ("t", "c"), ("u", "a")] {
            txn.put(key(&[root, a, b]), b.as_bytes().to_vec())
                .await
                .unwrap();
        }
        txn.commit().await.unwrap();

        let mut txn = kv.begin().await.unwrap();
        let two = txn.scan(prefix(&[root, "t"]), 2).await.unwrap();
        assert_eq!(
            two,
            vec![
                (key(&[root, "t", "a"]), b"a".to_vec()),
                (key(&[root, "t", "b"]), b"b".to_vec()),
            ]
        );
        let all = txn.scan(prefix(&[root, "t"]), 10).await.unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|(k, _)| decode(k)[..2] == [root, "t"]));
        let backwards = txn
            .scan(key(&[root, "u"])..key(&[root, "t"]), 10)
            .await
            .unwrap();
        assert!(backwards.is_empty());

        txn.delete(key(&[root, "t", "a"])).await.unwrap();
        txn.put(key(&[root, "t", "aa"]), b"aa".to_vec())
            .await
            .unwrap();
        let own = txn.scan(prefix(&[root, "t"]), 2).await.unwrap();
        assert_eq!(
            own,
            vec![
                (key(&[root, "t", "aa"]), b"aa".to_vec()),
                (key(&[root, "t", "b"]), b"b".to_vec()),
            ]
        );
    }

    #[tokio::test]
    async fn memory_contract() {
        contract(Kv::memory(), "t").await;
    }

    /// Reads the PD endpoints of the TiKV the gated tests run against.
    fn test_pd() -> Option<Vec<String>> {
        let pd = std::env::var("PISA_TEST_TIKV_PD").ok();
        if pd.is_none() {
            eprintln!("skipping: PISA_TEST_TIKV_PD unset");
        }
        pd.map(|pd| pd.split(',').map(str::to_owned).collect())
    }

    /// Runs the contract against a real TiKV. Start a throwaway one with host
    /// networking, so that the address TiKV advertises is reachable from the
    /// host, and point the test at it:
    ///
    /// ```sh
    /// docker run -d --rm --name pisa-pd --network host pingcap/pd:v8.5.1 --name=pd --data-dir=/tmp/pd \
    ///   --client-urls=http://127.0.0.1:2379 --peer-urls=http://127.0.0.1:2380
    /// docker run -d --rm --name pisa-tikv --network host --ulimit nofile=1048576 pingcap/tikv:v8.5.1 \
    ///   --addr=127.0.0.1:20160 --status-addr=127.0.0.1:20180 --data-dir=/tmp/tikv --pd=127.0.0.1:2379
    /// PISA_TEST_TIKV_PD=127.0.0.1:2379 cargo test -p pisa-worker tikv_ -- --nocapture
    /// docker stop pisa-tikv pisa-pd
    /// ```
    #[tokio::test]
    async fn tikv_contract() {
        let Some(pd) = test_pd() else { return };
        let root = uuid::Uuid::new_v4().simple().to_string();
        contract(Kv::tikv(&pd).await.unwrap(), &root).await;
    }

    #[tokio::test]
    async fn tikv_reports_write_conflicts() {
        let Some(pd) = test_pd() else { return };
        let kv = Kv::tikv(&pd).await.unwrap();
        let root = uuid::Uuid::new_v4().simple().to_string();
        let k = key(&[&root, "k"]);

        let mut first = kv.begin().await.unwrap();
        let mut second = kv.begin().await.unwrap();
        first.get(&k).await.unwrap();
        second.get(&k).await.unwrap();
        first.put(k.clone(), b"first".to_vec()).await.unwrap();
        second.put(k.clone(), b"second".to_vec()).await.unwrap();
        first.commit().await.unwrap();
        assert!(matches!(second.commit().await, Err(KvError::Conflict)));

        let mut locker = kv.begin().await.unwrap();
        let mut writer = kv.begin().await.unwrap();
        locker.lock(k.clone()).await.unwrap();
        writer.put(k.clone(), b"writer".to_vec()).await.unwrap();
        locker.commit().await.unwrap();
        assert!(matches!(writer.commit().await, Err(KvError::Conflict)));

        // Snapshot clean-ups that only lock a module's record rely on this.
        let mut first = kv.begin().await.unwrap();
        let mut second = kv.begin().await.unwrap();
        first.lock(k.clone()).await.unwrap();
        second.lock(k.clone()).await.unwrap();
        first.commit().await.unwrap();
        assert!(matches!(second.commit().await, Err(KvError::Conflict)));
    }

    #[test]
    fn tikv_errors_are_conflicts_only_when_contended() {
        use tikv_client::{Error, ProtoKeyError};

        let write_conflict = || {
            Error::KeyError(Box::new(ProtoKeyError {
                conflict: Some(Default::default()),
                ..Default::default()
            }))
        };
        let retryable = || {
            Error::KeyError(Box::new(ProtoKeyError {
                retryable: "retry".into(),
                ..Default::default()
            }))
        };
        let is_conflict = |e: Error| KvError::from(e).is_conflict();

        assert!(is_conflict(write_conflict()));
        assert!(is_conflict(retryable()));
        assert!(is_conflict(Error::ResolveLockError(vec![])));
        assert!(is_conflict(Error::ExtractedErrors(vec![
            Error::ResolveLockError(vec![])
        ])));
        assert!(is_conflict(Error::MultipleKeyErrors(vec![
            Error::StringError("boom".into()),
            write_conflict(),
        ])));

        assert!(!is_conflict(Error::KeyError(Box::default())));
        assert!(!is_conflict(Error::StringError("boom".into())));
        assert!(!is_conflict(Error::Io(std::io::Error::other("lost"))));
        assert!(!is_conflict(Error::UndeterminedError(Box::new(
            write_conflict()
        ))));
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
