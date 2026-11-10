//! Python bindings for `pisa-client`, exposed as `pisa_client._native`.
//!
//! Each call runs the async Rust client on a shared Tokio runtime with the
//! GIL released, so Python threads can issue requests concurrently. Results
//! become plain dicts and lists.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use pisa_client::{Error, Job, JobPoll, JobStatus, api};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use serde::Serialize;
use serde_json::{Value, json};

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
        Error::Url(msg) | Error::Invalid(msg) => return PyValueError::new_err(msg),
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

/// A submitted job as Python sees it: the job a submit returned.
fn queued<T>(job: Job<T>) -> api::Job {
    api::Job {
        id: job.id,
        kind: job.kind,
        status: JobStatus::Queued,
    }
}

/// The job that a dict, as a submit returned it, names.
fn job_of(job: &Bound<'_, PyAny>) -> PyResult<Job<Value>> {
    let job: api::Job = serde_json::from_value(from_py(job)?)
        .map_err(|e| PyValueError::new_err(format!("not a job a submit returned: {e}")))?;
    Ok(Job::new(job.id, job.kind))
}

/// A create's `replaces`: `{"snapshot_id": …}`, with `"allow":
/// {"definitions": [...]}` naming the definitions it may change.
fn replaces_of(replaces: &Bound<'_, PyAny>) -> PyResult<pisa_client::Replaces> {
    serde_json::from_value(from_py(replaces)?).map_err(|_| {
        PyValueError::new_err(
            "replaces must be {\"snapshot_id\": ...}, with \"allow\": {\"definitions\": [...]} to change those definitions",
        )
    })
}

/// A create's options, as its keyword arguments give them.
fn create_snapshot_options(
    verify: Option<bool>,
    labels: Option<BTreeMap<String, String>>,
    key: Option<String>,
    replaces: Option<Bound<'_, PyAny>>,
    fork: bool,
) -> PyResult<pisa_client::CreateSnapshotOptions> {
    Ok(pisa_client::CreateSnapshotOptions {
        verify,
        labels: labels.unwrap_or_default(),
        key,
        replaces: replaces.as_ref().map(replaces_of).transpose()?,
        fork,
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

    fn health<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        call(py, self.inner.health())
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

    #[pyo3(signature = (codes, *, detail = Vec::new(), at = Vec::new()))]
    fn check_batch<'py>(
        &self,
        py: Python<'py>,
        codes: Vec<String>,
        detail: Vec<String>,
        at: Vec<(u64, u64)>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let options = check_options(detail, at)?;
        call(py, async move {
            self.inner.check_batch_with(codes, &options).await
        })
    }

    fn start_session<'py>(&self, py: Python<'py>, code: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move { self.inner.start_session(&code).await })
    }

    fn apply_tactic<'py>(
        &self,
        py: Python<'py>,
        state_id: String,
        tactic: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            self.inner.apply_tactic(&state_id, &tactic).await
        })
    }

    fn check_session<'py>(
        &self,
        py: Python<'py>,
        session_id: String,
        code: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            self.inner.check_session(&session_id, &code).await
        })
    }

    #[pyo3(signature = (code, *, verify = None, labels = None, key = None, replaces = None, fork = false))]
    fn create_snapshot<'py>(
        &self,
        py: Python<'py>,
        code: String,
        verify: Option<bool>,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        replaces: Option<Bound<'py, PyAny>>,
        fork: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = create_snapshot_options(verify, labels, key, replaces, fork)?;
        call(py, async move {
            self.inner.create_snapshot_with(&code, &opts).await
        })
    }

    #[pyo3(signature = (code, *, verify = None, labels = None, key = None, replaces = None, fork = false))]
    fn submit_create_snapshot<'py>(
        &self,
        py: Python<'py>,
        code: String,
        verify: Option<bool>,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        replaces: Option<Bound<'py, PyAny>>,
        fork: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = create_snapshot_options(verify, labels, key, replaces, fork)?;
        call(py, async move {
            let job = self.inner.submit_create_snapshot_with(&code, &opts).await?;
            Ok(queued(job))
        })
    }

    fn get_snapshot<'py>(&self, py: Python<'py>, id: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move { self.inner.get_snapshot(&id).await })
    }

    #[pyo3(signature = (labels = None, *, limit = None, after = None))]
    fn list_snapshots<'py>(
        &self,
        py: Python<'py>,
        labels: Option<BTreeMap<String, String>>,
        limit: Option<usize>,
        after: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let labels = labels.unwrap_or_default();
        call(py, async move {
            self.inner
                .list_snapshots(&labels, limit, after.as_deref())
                .await
        })
    }

    fn update_snapshot<'py>(
        &self,
        py: Python<'py>,
        id: String,
        labels: BTreeMap<String, Option<String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(
            py,
            async move { self.inner.update_snapshot(&id, &labels).await },
        )
    }

    #[pyo3(signature = (id, *, with_replacers = false))]
    fn delete_snapshot<'py>(
        &self,
        py: Python<'py>,
        id: String,
        with_replacers: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = pisa_client::DeleteSnapshotOptions { with_replacers };
        call(py, async move {
            self.inner.delete_snapshot_with(&id, &opts).await
        })
    }

    fn verify_snapshot<'py>(&self, py: Python<'py>, id: String) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move { self.inner.verify_snapshot(&id).await })
    }

    fn submit_verify_snapshot<'py>(
        &self,
        py: Python<'py>,
        id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            Ok(queued(self.inner.submit_verify_snapshot(&id).await?))
        })
    }

    #[pyo3(signature = (id, labels = None))]
    fn list_dependents<'py>(
        &self,
        py: Python<'py>,
        id: String,
        labels: Option<BTreeMap<String, String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let labels = labels.unwrap_or_default();
        call(
            py,
            async move { self.inner.list_dependents(&id, &labels).await },
        )
    }

    #[pyo3(signature = (labels = None, *, against = None))]
    fn audit_snapshots<'py>(
        &self,
        py: Python<'py>,
        labels: Option<BTreeMap<String, String>>,
        against: Option<Vec<String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let labels = labels.unwrap_or_default();
        match against {
            Some(_) if !labels.is_empty() => Err(PyValueError::new_err(
                "labels and against don't combine: audit a project by its labels, or a run against its spec",
            )),
            Some(against) => call(py, async move {
                self.inner.audit_snapshots_against(&against).await
            }),
            None => call(py, async move { self.inner.audit_snapshots(&labels).await }),
        }
    }

    #[pyo3(signature = (from_, to, labels, *, dry_run = false, allow_changes = false, retire = false, verify = None))]
    #[allow(clippy::too_many_arguments)]
    fn rebuild_snapshots<'py>(
        &self,
        py: Python<'py>,
        from_: String,
        to: String,
        labels: BTreeMap<String, String>,
        dry_run: bool,
        allow_changes: bool,
        retire: bool,
        verify: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = pisa_client::RebuildOptions {
            dry_run,
            allow_changes,
            retire,
            verify,
        };
        call(py, async move {
            self.inner
                .rebuild_snapshots(&from_, &to, &labels, &opts)
                .await
        })
    }

    #[pyo3(signature = (from_, to, labels, *, dry_run = false, allow_changes = false, retire = false, verify = None))]
    #[allow(clippy::too_many_arguments)]
    fn submit_rebuild_snapshots<'py>(
        &self,
        py: Python<'py>,
        from_: String,
        to: String,
        labels: BTreeMap<String, String>,
        dry_run: bool,
        allow_changes: bool,
        retire: bool,
        verify: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = pisa_client::RebuildOptions {
            dry_run,
            allow_changes,
            retire,
            verify,
        };
        call(py, async move {
            let job = self
                .inner
                .submit_rebuild_snapshots(&from_, &to, &labels, &opts)
                .await?;
            Ok(queued(job))
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

    fn submit_start_session<'py>(
        &self,
        py: Python<'py>,
        code: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            Ok(queued(self.inner.submit_start_session(&code).await?))
        })
    }

    fn submit_apply_tactic<'py>(
        &self,
        py: Python<'py>,
        state_id: String,
        tactic: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            Ok(queued(
                self.inner.submit_apply_tactic(&state_id, &tactic).await?,
            ))
        })
    }

    fn submit_check_session<'py>(
        &self,
        py: Python<'py>,
        session_id: String,
        code: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            Ok(queued(
                self.inner.submit_check_session(&session_id, &code).await?,
            ))
        })
    }

    /// Where the job stands, after waiting up to `wait` seconds: `{"done":
    /// True, "answer": …}`, or `{"done": False, "job": …}` if it isn't done
    /// yet.
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
                JobPoll::Done(answer) => json!({"done": true, "answer": answer}),
                JobPoll::Pending(job) => json!({"done": false, "job": job}),
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

    #[pyo3(signature = (
        text = None, *, description = None, latex = None, pattern = None, goal = None,
        name = None, libraries = None, kinds = None, limit = 5, include_sorried = false,
        snapshots = None
    ))]
    #[allow(clippy::too_many_arguments)]
    fn search<'py>(
        &self,
        py: Python<'py>,
        text: Option<String>,
        description: Option<String>,
        latex: Option<String>,
        pattern: Option<String>,
        goal: Option<String>,
        name: Option<String>,
        libraries: Option<Vec<String>>,
        kinds: Option<Vec<String>>,
        limit: usize,
        include_sorried: bool,
        snapshots: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let snapshots = match snapshots {
            Some(s) => serde_json::from_value(from_py(&s)?)
                .map_err(|e| PyValueError::new_err(format!("invalid snapshots: {e}")))?,
            None => Default::default(),
        };
        let query = pisa_client::Query {
            text,
            description,
            latex,
            pattern,
            goal,
            name,
            libraries: libraries.unwrap_or_default(),
            kinds: kinds.unwrap_or_default(),
            limit,
            include_sorried,
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

    #[pyo3(signature = (name, *, module = None))]
    fn get_declaration<'py>(
        &self,
        py: Python<'py>,
        name: String,
        module: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        call(py, async move {
            self.inner.get_declaration(&name, module.as_deref()).await
        })
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Client>()
}
