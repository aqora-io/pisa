//! Python bindings for `pisa-client`, exposed as `pisa_client._native`.
//!
//! Each call runs the async Rust client on a shared Tokio runtime with the
//! GIL released, so Python threads can issue requests concurrently. Results
//! become plain dicts and lists.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use pisa_client::{Error, Job, JobPoll, JobState, JobStatus};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use serde::Serialize;
use serde_json::Value;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// Raises the exception class `pisa_client.errors` picks for the error.
fn to_py_err(py: Python<'_>, e: Error) -> PyErr {
    let (status, kind, message) = match e {
        Error::Url(msg) => return PyValueError::new_err(msg),
        Error::Server {
            status,
            kind,
            message,
        } => {
            let kind = serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(String::from));
            (Some(status), kind, message)
        }
        Error::Http(err) => (None, None, err.to_string()),
    };
    let raised = py
        .import("pisa_client.errors")
        .and_then(|m| m.getattr("_from_server"))
        .and_then(|f| f.call1((message, kind, status)));
    match raised {
        Ok(exc) => PyErr::from_value(exc),
        Err(err) => err,
    }
}

fn call<'py, T, F>(py: Python<'py>, fut: F) -> PyResult<Bound<'py, PyAny>>
where
    T: Serialize + Send,
    F: Future<Output = Result<T, Error>> + Send,
{
    let result = py.detach(|| runtime().block_on(fut));
    match result {
        Ok(value) => {
            let value =
                serde_json::to_value(value).map_err(|e| PyValueError::new_err(e.to_string()))?;
            to_py(py, &value)
        }
        Err(e) => Err(to_py_err(py, e)),
    }
}

/// Converts JSON into the equivalent Python dicts, lists and scalars.
fn to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match value {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i.into_pyobject(py)?.into_any(),
            (_, Some(u), _) => u.into_pyobject(py)?.into_any(),
            (_, _, Some(f)) => f.into_pyobject(py)?.into_any(),
            _ => unreachable!("a JSON number is an i64, u64 or f64"),
        },
        Value::String(s) => PyString::new(py, s).into_any(),
        Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(to_py(py, item)?)?;
            }
            list.into_any()
        }
        Value::Object(fields) => {
            let dict = PyDict::new(py);
            for (key, item) in fields {
                dict.set_item(key, to_py(py, item)?)?;
            }
            dict.into_any()
        }
    })
}

/// Converts Python dicts, lists and scalars into JSON.
fn from_py(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    Ok(if obj.is_none() {
        Value::Null
    } else if let Ok(b) = obj.cast::<PyBool>() {
        Value::Bool(b.is_true())
    } else if obj.is_instance_of::<PyInt>() {
        Value::from(obj.extract::<i64>()?)
    } else if obj.is_instance_of::<PyFloat>() {
        Value::from(obj.extract::<f64>()?)
    } else if let Ok(s) = obj.cast::<PyString>() {
        Value::String(s.to_str()?.to_string())
    } else if let Ok(d) = obj.cast::<PyDict>() {
        let mut fields = serde_json::Map::new();
        for (k, v) in d.iter() {
            fields.insert(k.extract::<String>()?, from_py(&v)?);
        }
        Value::Object(fields)
    } else if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
        Value::Array(
            obj.try_iter()?
                .map(|x| from_py(&x?))
                .collect::<PyResult<_>>()?,
        )
    } else {
        return Err(PyValueError::new_err(format!(
            "can't send a {} to the server",
            obj.get_type().name()?
        )));
    })
}

/// A submitted job as Python sees it: the status a submit returned.
fn queued<T>(job: Job<T>) -> JobStatus {
    JobStatus {
        job: job.id,
        kind: job.kind,
        status: JobState::Queued,
    }
}

/// The job a status dict, as a submit returned it, names.
fn job_of(status: &Bound<'_, PyAny>) -> PyResult<Job<Value>> {
    let status: JobStatus = serde_json::from_value(from_py(status)?)
        .map_err(|e| PyValueError::new_err(format!("not a job a submit returned: {e}")))?;
    Ok(Job::new(status.job, status.kind))
}

/// A create's `replaces`: an entry, or `{"entry": …, "allow": "definitions"}`.
fn replaces_of(replaces: &Bound<'_, PyAny>) -> PyResult<pisa_client::Replaces> {
    serde_json::from_value(from_py(replaces)?).map_err(|_| {
        PyValueError::new_err(
            "replaces must be an entry, or {\"entry\": ..., \"allow\": \"definitions\"}",
        )
    })
}

fn parse_detail(detail: Vec<String>) -> PyResult<Vec<pisa_client::Detail>> {
    serde_json::from_value(detail.into())
        .map_err(|e| PyValueError::new_err(format!("invalid detail: {e}")))
}

/// A check's `detail` and `at`, given as `(line, column)` pairs.
fn check_options(detail: Vec<String>, at: Vec<(u64, u64)>) -> PyResult<pisa_client::CheckOptions> {
    Ok(pisa_client::CheckOptions {
        detail: parse_detail(detail)?,
        at: at
            .into_iter()
            .map(|(line, column)| pisa_client::Pos { line, column })
            .collect(),
    })
}

/// A client for one pisa server. Safe to share between threads.
#[pyclass(frozen, module = "pisa_client")]
struct Client {
    inner: pisa_client::Client,
}

#[pymethods]
impl Client {
    #[new]
    #[pyo3(signature = (url, *, version = None, timeout = 60))]
    fn new(url: &str, version: Option<String>, timeout: u64) -> PyResult<Self> {
        let mut inner = pisa_client::Client::new(url)
            .map_err(|e| PyValueError::new_err(e.to_string()))?
            .with_timeout(timeout);
        if let Some(v) = version {
            inner = inner.with_version(v);
        }
        Ok(Client { inner })
    }

    fn health(&self, py: Python<'_>) -> PyResult<()> {
        let result = py.detach(|| runtime().block_on(self.inner.health()));
        result.map_err(|e| to_py_err(py, e))
    }

    #[pyo3(signature = (code, *, detail = Vec::new(), at = Vec::new()))]
    fn check<'py>(
        &self,
        py: Python<'py>,
        code: String,
        detail: Vec<String>,
        at: Vec<(u64, u64)>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let options = check_options(detail, at)?;
        call(
            py,
            async move { self.inner.check_with(&code, &options).await },
        )
    }

    #[pyo3(signature = (items, *, detail = Vec::new(), at = Vec::new()))]
    fn check_batch<'py>(
        &self,
        py: Python<'py>,
        items: Vec<(String, String)>,
        detail: Vec<String>,
        at: Vec<(u64, u64)>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let options = check_options(detail, at)?;
        call(py, async move {
            self.inner.check_batch_with(items, &options).await
        })
    }

    fn start_session<'py>(&self, py: Python<'py>, code: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move { self.inner.start_session(&code).await })
    }

    fn tactic<'py>(
        &self,
        py: Python<'py>,
        state: String,
        tactic: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move { self.inner.tactic(&state, &tactic).await })
    }

    fn check_session<'py>(
        &self,
        py: Python<'py>,
        session: String,
        code: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            self.inner.check_session(&session, &code).await
        })
    }

    #[pyo3(signature = (code, *, verify = None, labels = None, key = None, replaces = None))]
    fn create_snapshot<'py>(
        &self,
        py: Python<'py>,
        code: String,
        verify: Option<bool>,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        replaces: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = pisa_client::SnapshotOptions {
            verify,
            labels: labels.unwrap_or_default(),
            key,
            replaces: replaces.as_ref().map(replaces_of).transpose()?,
        };
        call(py, async move {
            self.inner.create_snapshot_with(&code, &opts).await
        })
    }

    fn verify_snapshot<'py>(&self, py: Python<'py>, entry: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move { self.inner.verify_snapshot(&entry).await })
    }

    fn snapshot<'py>(&self, py: Python<'py>, entry: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move { self.inner.snapshot(&entry).await })
    }

    fn delete_snapshot<'py>(&self, py: Python<'py>, entry: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move { self.inner.delete_snapshot(&entry).await })
    }

    #[pyo3(signature = (labels = None, *, limit = None, after = None))]
    fn list_snapshots<'py>(
        &self,
        py: Python<'py>,
        labels: Option<Bound<'py, PyDict>>,
        limit: Option<usize>,
        after: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // In the dict's order: the first label is the one the server scans.
        let labels = labels.map_or(Ok(Vec::new()), |d| {
            d.iter()
                .map(|(k, v)| Ok((k.extract::<String>()?, v.extract::<String>()?)))
                .collect::<PyResult<Vec<_>>>()
        })?;
        call(py, async move {
            let labels: Vec<(&str, &str)> = labels
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            self.inner
                .list_snapshots(&labels, limit, after.as_deref())
                .await
        })
    }

    #[pyo3(signature = (labels = None))]
    fn audit<'py>(
        &self,
        py: Python<'py>,
        labels: Option<BTreeMap<String, String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let labels = labels.unwrap_or_default();
        call(py, async move {
            let labels: Vec<(&str, &str)> = labels
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            self.inner.audit(&labels).await
        })
    }

    #[pyo3(signature = (entry, labels = None))]
    fn dependents<'py>(
        &self,
        py: Python<'py>,
        entry: String,
        labels: Option<BTreeMap<String, String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let labels = labels.unwrap_or_default();
        call(py, async move {
            let labels: Vec<(&str, &str)> = labels
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            self.inner.dependents(&entry, &labels).await
        })
    }

    #[pyo3(signature = (from_entry, to_entry, labels, *, allow_changes = false, retire = false, verify = None))]
    #[allow(clippy::too_many_arguments)]
    fn rebuild<'py>(
        &self,
        py: Python<'py>,
        from_entry: String,
        to_entry: String,
        labels: BTreeMap<String, String>,
        allow_changes: bool,
        retire: bool,
        verify: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = pisa_client::RebuildOptions {
            allow_changes,
            retire,
            verify,
        };
        call(py, async move {
            self.inner
                .rebuild(&from_entry, &to_entry, &labels, &opts)
                .await
        })
    }

    #[pyo3(signature = (from_entry, to_entry, labels, *, allow_changes = false, retire = false, verify = None))]
    #[allow(clippy::too_many_arguments)]
    fn submit_rebuild<'py>(
        &self,
        py: Python<'py>,
        from_entry: String,
        to_entry: String,
        labels: BTreeMap<String, String>,
        allow_changes: bool,
        retire: bool,
        verify: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = pisa_client::RebuildOptions {
            allow_changes,
            retire,
            verify,
        };
        call(py, async move {
            let job = self
                .inner
                .submit_rebuild(&from_entry, &to_entry, &labels, &opts)
                .await?;
            Ok(queued(job))
        })
    }

    #[pyo3(signature = (from_entry, to_entry, labels, *, allow_changes = false))]
    fn rebuild_plan<'py>(
        &self,
        py: Python<'py>,
        from_entry: String,
        to_entry: String,
        labels: BTreeMap<String, String>,
        allow_changes: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            self.inner
                .rebuild_plan(&from_entry, &to_entry, &labels, allow_changes)
                .await
        })
    }

    #[pyo3(signature = (code, *, detail = Vec::new(), at = Vec::new()))]
    fn submit_check<'py>(
        &self,
        py: Python<'py>,
        code: String,
        detail: Vec<String>,
        at: Vec<(u64, u64)>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let options = check_options(detail, at)?;
        call(py, async move {
            let job = self.inner.submit_check_with(&code, &options).await?;
            Ok(queued(job))
        })
    }

    fn submit_session<'py>(&self, py: Python<'py>, code: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            Ok(queued(self.inner.submit_session(&code).await?))
        })
    }

    fn submit_tactic<'py>(
        &self,
        py: Python<'py>,
        state: String,
        tactic: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            Ok(queued(self.inner.submit_tactic(&state, &tactic).await?))
        })
    }

    fn submit_session_check<'py>(
        &self,
        py: Python<'py>,
        session: String,
        code: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            Ok(queued(
                self.inner.submit_session_check(&session, &code).await?,
            ))
        })
    }

    #[pyo3(signature = (code, *, verify = None, labels = None, key = None, replaces = None))]
    fn submit_snapshot<'py>(
        &self,
        py: Python<'py>,
        code: String,
        verify: Option<bool>,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        replaces: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = pisa_client::SnapshotOptions {
            verify,
            labels: labels.unwrap_or_default(),
            key,
            replaces: replaces.as_ref().map(replaces_of).transpose()?,
        };
        call(py, async move {
            Ok(queued(self.inner.submit_snapshot_with(&code, &opts).await?))
        })
    }

    fn submit_verify<'py>(&self, py: Python<'py>, entry: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            Ok(queued(self.inner.submit_verify(&entry).await?))
        })
    }

    /// The job's answer if it is done, after waiting up to `wait` seconds;
    /// `None` if it isn't yet.
    #[pyo3(signature = (job, *, wait = 0))]
    fn poll_job<'py>(
        &self,
        py: Python<'py>,
        job: &Bound<'py, PyAny>,
        wait: u64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let job = job_of(job)?;
        call(py, async move {
            let wait = Duration::from_secs(wait);
            Ok(match self.inner.poll_job(&job, wait).await? {
                JobPoll::Done(answer) => answer,
                JobPoll::Pending(_) => Value::Null,
            })
        })
    }

    fn wait_job<'py>(
        &self,
        py: Python<'py>,
        job: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let job = job_of(job)?;
        call(py, async move { self.inner.wait_job(&job).await })
    }

    fn cancel_job<'py>(
        &self,
        py: Python<'py>,
        job: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let job = job_of(job)?;
        call(py, async move { self.inner.cancel_job(&job).await })
    }

    #[pyo3(signature = (entry, *, set = None, remove = None))]
    fn set_snapshot_labels<'py>(
        &self,
        py: Python<'py>,
        entry: String,
        set: Option<BTreeMap<String, String>>,
        remove: Option<Vec<String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (set, remove) = (set.unwrap_or_default(), remove.unwrap_or_default());
        call(py, async move {
            self.inner.set_snapshot_labels(&entry, &set, &remove).await
        })
    }
}

/// A client for one declaration search server. Safe to share between threads.
#[pyclass(frozen, module = "pisa_client")]
struct SearchClient {
    inner: pisa_client::SearchClient,
}

#[pymethods]
impl SearchClient {
    #[new]
    #[pyo3(signature = (url, *, env_id = None))]
    fn new(url: &str, env_id: Option<String>) -> PyResult<Self> {
        let mut inner = pisa_client::SearchClient::new(url)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        if let Some(env) = env_id {
            inner = inner.with_env(env);
        }
        Ok(SearchClient { inner })
    }

    fn health(&self, py: Python<'_>) -> PyResult<()> {
        let result = py.detach(|| runtime().block_on(self.inner.health()));
        result.map_err(|e| to_py_err(py, e))
    }

    #[pyo3(signature = (
        q = None, *, nl = None, latex = None, pattern = None, goal = None, name = None,
        libraries = Vec::new(), kinds = Vec::new(), k = 5, include_sorried = false,
        snapshots = None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn search<'py>(
        &self,
        py: Python<'py>,
        q: Option<String>,
        nl: Option<String>,
        latex: Option<String>,
        pattern: Option<String>,
        goal: Option<String>,
        name: Option<String>,
        libraries: Vec<String>,
        kinds: Vec<String>,
        k: usize,
        include_sorried: bool,
        snapshots: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let snapshots = match snapshots {
            Some(s) => serde_json::from_value(from_py(&s)?)
                .map_err(|e| PyValueError::new_err(format!("invalid snapshots: {e}")))?,
            None => Default::default(),
        };
        let query = pisa_client::Query {
            q,
            nl,
            latex,
            pattern,
            goal,
            name,
            libraries,
            kinds,
            k,
            include_sorried,
            verify: false,
            snapshots,
        };
        call(py, async move { self.inner.search(&query).await })
    }

    fn search_batch<'py>(
        &self,
        py: Python<'py>,
        queries: Vec<Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let queries = queries
            .iter()
            .map(|q| {
                serde_json::from_value(from_py(q.as_any())?)
                    .map_err(|e| PyValueError::new_err(format!("invalid query: {e}")))
            })
            .collect::<PyResult<Vec<pisa_client::Query>>>()?;
        call(py, self.inner.search_batch(queries))
    }

    fn resolve<'py>(&self, py: Python<'py>, names: Vec<String>) -> PyResult<Bound<'py, PyAny>> {
        call(py, self.inner.resolve(names))
    }

    fn decl<'py>(&self, py: Python<'py>, name: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move { self.inner.decl(&name).await })
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Client>()?;
    m.add_class::<SearchClient>()
}
