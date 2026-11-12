//! Python bindings for `pisa-client`, exposed as `pisa_client._native`.
//!
//! Each call runs the async Rust client on a shared Tokio runtime. `Client`
//! blocks with the GIL released, so Python threads can issue requests
//! concurrently, and stops at Ctrl-C. `AsyncClient` returns an awaitable
//! instead, which a cancel stops. Results become plain dicts and lists.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use pisa_client::{Error, Job, JobPoll, JobStatus, api};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use serde::Serialize;
use serde_json::{Value, json};

/// The runtime both clients' requests run on: the one awaitables run on.
fn runtime() -> &'static tokio::runtime::Runtime {
    pyo3_async_runtimes::tokio::get_runtime()
}

/// How often a blocking call looks for Ctrl-C.
const SIGNALS_EVERY: Duration = Duration::from_millis(100);

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

/// `value`, as JSON, in Python.
fn json_to_py<T: Serialize>(py: Python<'_>, value: T) -> PyResult<Py<PyAny>> {
    let value = serde_json::to_value(value).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(to_py(py, &value)?.unbind())
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

/// A dict as `T`, or a `ValueError` saying it should be `shape`.
fn dict_of<T: serde::de::DeserializeOwned>(value: &Bound<'_, PyAny>, shape: &str) -> PyResult<T> {
    serde_json::from_value(from_py(value)?).map_err(|_| PyValueError::new_err(shape.to_string()))
}

/// A create's options, as its keyword arguments give them.
#[allow(clippy::too_many_arguments)]
fn create_snapshot_options(
    verify: Option<bool>,
    labels: Option<BTreeMap<String, String>>,
    key: Option<String>,
    replaces: Option<Bound<'_, PyAny>>,
    fork: bool,
    holes: Option<Vec<String>>,
    amends: Option<Bound<'_, PyAny>>,
    refutes: Option<Bound<'_, PyAny>>,
    detail: Vec<String>,
    name: Option<String>,
) -> PyResult<pisa_client::CreateSnapshotOptions> {
    let amends = amends
        .as_ref()
        .map(|a| dict_of(a, "amends must be {\"snapshot_id\": ..., \"reason\": ...}"));
    let refutes = refutes
        .as_ref()
        .map(|r| dict_of(r, "refutes must be {\"snapshot_id\": ..., \"target\": ...}"));
    Ok(pisa_client::CreateSnapshotOptions {
        verify,
        labels: labels.unwrap_or_default(),
        key,
        replaces: replaces.as_ref().map(replaces_of).transpose()?,
        fork,
        holes: holes.unwrap_or_default(),
        amends: amends.transpose()?,
        refutes: refutes.transpose()?,
        detail: parse_detail(detail)?,
        name,
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

/// What `Client` and `AsyncClient` share: every call, and whether it
/// blocks or returns an awaitable.
#[pyclass(name = "_Client", subclass, frozen, module = "pisa_client")]
struct Base {
    inner: pisa_client::Client,
    asynchronous: bool,
}

impl Base {
    fn new(
        url: &str,
        version: Option<String>,
        timeout: u64,
        scope: Option<BTreeMap<String, String>>,
        retry: bool,
        asynchronous: bool,
    ) -> PyResult<Self> {
        let mut inner = pisa_client::Client::new(url)
            .map_err(|e| PyValueError::new_err(e.to_string()))?
            .with_timeout(timeout);
        if !retry {
            inner = inner.with_retry(pisa_client::RetryPolicy::none());
        }
        if let Some(v) = version {
            inner = inner.with_version(v);
        }
        if let Some(scope) = scope {
            inner = inner
                .with_scope(&scope)
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
        }
        Ok(Base {
            inner,
            asynchronous,
        })
    }

    /// Runs `fut` and gives its answer as JSON in Python.
    fn call<'py, T, F>(&self, py: Python<'py>, fut: F) -> PyResult<Bound<'py, PyAny>>
    where
        T: Serialize + Send + 'static,
        F: Future<Output = Result<T, Error>> + Send + 'static,
    {
        self.run(py, fut, json_to_py)
    }

    /// Runs `fut` and gives its answer in Python as `convert` makes it: an
    /// async client returns an awaitable, which starts at once and whose
    /// cancel drops the request; a sync one blocks, with the GIL released,
    /// and stops at Ctrl-C, dropping the request too. A job a worker took
    /// runs on either way.
    fn run<'py, T, F>(
        &self,
        py: Python<'py>,
        fut: F,
        convert: fn(Python<'_>, T) -> PyResult<Py<PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, Error>> + Send + 'static,
    {
        if self.asynchronous {
            return pyo3_async_runtimes::tokio::future_into_py(py, async move {
                let result = fut.await;
                Python::attach(|py| match result {
                    Ok(value) => convert(py, value),
                    Err(e) => Err(to_py_err(py, e)),
                })
            });
        }
        let mut fut = std::pin::pin!(fut);
        loop {
            // The timeout is made on the runtime, which its timer needs.
            let slice = py.detach(|| {
                runtime()
                    .block_on(async { tokio::time::timeout(SIGNALS_EVERY, fut.as_mut()).await })
            });
            match slice {
                Ok(Ok(value)) => return Ok(convert(py, value)?.into_bound(py)),
                Ok(Err(e)) => return Err(to_py_err(py, e)),
                // Ctrl-C raises KeyboardInterrupt here, and the request goes.
                Err(_) => py.check_signals()?,
            }
        }
    }
}

/// A client for one pisa server whose calls block until they are answered,
/// with the GIL released, and stop at Ctrl-C. Safe to share between threads.
#[pyclass(extends = Base, frozen, module = "pisa_client")]
struct Client {}

#[pymethods]
impl Client {
    #[new]
    #[pyo3(signature = (url, *, version = None, timeout = 60, scope = None, retry = true))]
    fn new(
        url: &str,
        version: Option<String>,
        timeout: u64,
        scope: Option<BTreeMap<String, String>>,
        retry: bool,
    ) -> PyResult<PyClassInitializer<Self>> {
        let base = Base::new(url, version, timeout, scope, retry, false)?;
        Ok(PyClassInitializer::from(base).add_subclass(Client {}))
    }
}

/// A client for one pisa server whose calls return awaitables, which start
/// at once; cancelling one drops its request.
#[pyclass(extends = Base, frozen, module = "pisa_client")]
struct AsyncClient {}

#[pymethods]
impl AsyncClient {
    #[new]
    #[pyo3(signature = (url, *, version = None, timeout = 60, scope = None, retry = true))]
    fn new(
        url: &str,
        version: Option<String>,
        timeout: u64,
        scope: Option<BTreeMap<String, String>>,
        retry: bool,
    ) -> PyResult<PyClassInitializer<Self>> {
        let base = Base::new(url, version, timeout, scope, retry, true)?;
        Ok(PyClassInitializer::from(base).add_subclass(AsyncClient {}))
    }
}

#[pymethods]
impl Base {
    fn health<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.health().await })
    }

    #[pyo3(signature = (code, *, detail = Vec::new(), at = Vec::new()))]
    fn check<'py>(
        &self,
        py: Python<'py>,
        code: String,
        detail: Vec<String>,
        at: Vec<(u64, u64)>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let options = check_options(detail, at)?;
        self.call(py, async move { inner.check_with(&code, &options).await })
    }

    #[pyo3(signature = (codes, *, detail = Vec::new(), at = Vec::new()))]
    fn check_batch<'py>(
        &self,
        py: Python<'py>,
        codes: Vec<String>,
        detail: Vec<String>,
        at: Vec<(u64, u64)>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let options = check_options(detail, at)?;
        self.call(
            py,
            async move { inner.check_batch_with(codes, &options).await },
        )
    }

    fn start_session<'py>(&self, py: Python<'py>, code: String) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.start_session(&code).await })
    }

    fn apply_tactic<'py>(
        &self,
        py: Python<'py>,
        state_id: String,
        tactic: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(
            py,
            async move { inner.apply_tactic(&state_id, &tactic).await },
        )
    }

    #[pyo3(signature = (session_id, code, *, detail = Vec::new()))]
    fn check_session<'py>(
        &self,
        py: Python<'py>,
        session_id: String,
        code: String,
        detail: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let detail = parse_detail(detail)?;
        self.call(py, async move {
            inner.check_session_with(&session_id, &code, &detail).await
        })
    }

    #[pyo3(signature = (code, *, verify = None, labels = None, key = None, replaces = None, fork = false, holes = None, amends = None, refutes = None, detail = Vec::new(), name = None))]
    #[allow(clippy::too_many_arguments)]
    fn create_snapshot<'py>(
        &self,
        py: Python<'py>,
        code: String,
        verify: Option<bool>,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        replaces: Option<Bound<'py, PyAny>>,
        fork: bool,
        holes: Option<Vec<String>>,
        amends: Option<Bound<'py, PyAny>>,
        refutes: Option<Bound<'py, PyAny>>,
        detail: Vec<String>,
        name: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = create_snapshot_options(
            verify, labels, key, replaces, fork, holes, amends, refutes, detail, name,
        )?;
        self.call(
            py,
            async move { inner.create_snapshot_with(&code, &opts).await },
        )
    }

    #[pyo3(signature = (code, *, verify = None, labels = None, key = None, replaces = None, fork = false, holes = None, amends = None, refutes = None, detail = Vec::new(), name = None))]
    #[allow(clippy::too_many_arguments)]
    fn submit_create_snapshot<'py>(
        &self,
        py: Python<'py>,
        code: String,
        verify: Option<bool>,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        replaces: Option<Bound<'py, PyAny>>,
        fork: bool,
        holes: Option<Vec<String>>,
        amends: Option<Bound<'py, PyAny>>,
        refutes: Option<Bound<'py, PyAny>>,
        detail: Vec<String>,
        name: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = create_snapshot_options(
            verify, labels, key, replaces, fork, holes, amends, refutes, detail, name,
        )?;
        self.call(py, async move {
            let job = inner.submit_create_snapshot_with(&code, &opts).await?;
            Ok(queued(job))
        })
    }

    #[pyo3(signature = (code, *, holes = None, name = None, verify = None, labels = None, key = None, detail = Vec::new()))]
    #[allow(clippy::too_many_arguments)]
    fn create_spec<'py>(
        &self,
        py: Python<'py>,
        code: String,
        holes: Option<Vec<String>>,
        name: Option<String>,
        verify: Option<bool>,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        detail: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::SpecOptions {
            holes: holes.unwrap_or_default(),
            name,
            verify,
            labels: labels.unwrap_or_default(),
            key,
            detail: parse_detail(detail)?,
        };
        self.call(py, async move { inner.create_spec(&code, &opts).await })
    }

    #[pyo3(signature = (replaces_id, code, *, allow = None, fork = false, verify = None, labels = None, key = None, detail = Vec::new()))]
    #[allow(clippy::too_many_arguments)]
    fn create_proof<'py>(
        &self,
        py: Python<'py>,
        replaces_id: String,
        code: String,
        allow: Option<Vec<String>>,
        fork: bool,
        verify: Option<bool>,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        detail: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::ProofOptions {
            allow: allow.unwrap_or_default(),
            fork,
            verify,
            labels: labels.unwrap_or_default(),
            key,
            detail: parse_detail(detail)?,
        };
        self.call(py, async move {
            inner.create_proof(&replaces_id, &code, &opts).await
        })
    }

    #[pyo3(signature = (spec_id, reason, code, *, holes = None, name = None, labels = None, key = None, detail = Vec::new()))]
    #[allow(clippy::too_many_arguments)]
    fn amend_spec<'py>(
        &self,
        py: Python<'py>,
        spec_id: String,
        reason: String,
        code: String,
        holes: Option<Vec<String>>,
        name: Option<String>,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        detail: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::AmendOptions {
            holes,
            name,
            labels: labels.unwrap_or_default(),
            key,
            detail: parse_detail(detail)?,
        };
        self.call(py, async move {
            inner.amend_spec(&spec_id, &reason, &code, &opts).await
        })
    }

    #[pyo3(signature = (spec_id, target, code, *, labels = None, key = None, detail = Vec::new()))]
    #[allow(clippy::too_many_arguments)]
    fn refute<'py>(
        &self,
        py: Python<'py>,
        spec_id: String,
        target: String,
        code: String,
        labels: Option<BTreeMap<String, String>>,
        key: Option<String>,
        detail: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::RefuteOptions {
            labels: labels.unwrap_or_default(),
            key,
            detail: parse_detail(detail)?,
        };
        self.call(py, async move {
            inner.refute(&spec_id, &target, &code, &opts).await
        })
    }

    #[pyo3(signature = (id, *, detail = Vec::new()))]
    fn get_snapshot<'py>(
        &self,
        py: Python<'py>,
        id: String,
        detail: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let detail = parse_detail(detail)?;
        self.call(
            py,
            async move { inner.get_snapshot_with(&id, &detail).await },
        )
    }

    #[pyo3(signature = (labels = None, *, limit = None, after = None, heads = false))]
    fn list_snapshots<'py>(
        &self,
        py: Python<'py>,
        labels: Option<BTreeMap<String, String>>,
        limit: Option<usize>,
        after: Option<String>,
        heads: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let labels = labels.unwrap_or_default();
        self.call(py, async move {
            inner
                .list_snapshots(&labels, limit, after.as_deref(), heads)
                .await
        })
    }

    fn update_snapshot<'py>(
        &self,
        py: Python<'py>,
        id: String,
        labels: BTreeMap<String, Option<String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.update_snapshot(&id, &labels).await })
    }

    #[pyo3(signature = (id, *, with_replacers = false))]
    fn delete_snapshot<'py>(
        &self,
        py: Python<'py>,
        id: String,
        with_replacers: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::DeleteSnapshotOptions { with_replacers };
        self.call(
            py,
            async move { inner.delete_snapshot_with(&id, &opts).await },
        )
    }

    #[pyo3(signature = (id, *, again = false))]
    fn verify_snapshot<'py>(
        &self,
        py: Python<'py>,
        id: String,
        again: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move {
            match again {
                true => inner.verify_snapshot_again(&id).await,
                false => inner.verify_snapshot(&id).await,
            }
        })
    }

    fn submit_verify_snapshot<'py>(
        &self,
        py: Python<'py>,
        id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move {
            Ok(queued(inner.submit_verify_snapshot(&id).await?))
        })
    }

    #[pyo3(signature = (id, labels = None))]
    fn list_dependents<'py>(
        &self,
        py: Python<'py>,
        id: String,
        labels: Option<BTreeMap<String, String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let labels = labels.unwrap_or_default();
        self.call(py, async move { inner.list_dependents(&id, &labels).await })
    }

    #[pyo3(signature = (labels = None, *, against = None, allow = None, review = false, review_limit = None))]
    fn audit_snapshots<'py>(
        &self,
        py: Python<'py>,
        labels: Option<BTreeMap<String, String>>,
        against: Option<Vec<String>>,
        allow: Option<Vec<String>>,
        review: bool,
        review_limit: Option<u32>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let labels = labels.unwrap_or_default();
        let allow = allow.unwrap_or_default();
        match against {
            Some(_) if !labels.is_empty() => Err(PyValueError::new_err(
                "labels and against don't combine: audit a project by its labels, or a run against its spec",
            )),
            Some(against) => self.call(py, async move {
                let opts = pisa_client::AuditOptions {
                    allow,
                    review,
                    review_limit,
                };
                inner.audit_snapshots_against_with(&against, &opts).await
            }),
            None if !allow.is_empty() || review => Err(PyValueError::new_err(
                "allow and review are for an audit against a spec: name its spec snapshots in against",
            )),
            None => self.call(py, async move { inner.audit_snapshots(&labels).await }),
        }
    }

    /// The run's export, a tar.gz, as bytes.
    #[pyo3(signature = (against, *, allow = None))]
    fn export_run<'py>(
        &self,
        py: Python<'py>,
        against: Vec<String>,
        allow: Option<Vec<String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let allow = allow.unwrap_or_default();
        let fut = async move {
            let allow: Vec<&str> = allow.iter().map(String::as_str).collect();
            inner.export_run(&against, &allow).await
        };
        self.run(py, fut, |py, bytes| {
            Ok(PyBytes::new(py, &bytes).into_any().unbind())
        })
    }

    #[pyo3(signature = (against, *, allow = None, paranoid = true, timeout = None, build_timeout = None))]
    fn accept_run<'py>(
        &self,
        py: Python<'py>,
        against: Vec<String>,
        allow: Option<Vec<String>>,
        paranoid: bool,
        timeout: Option<u64>,
        build_timeout: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::AcceptOptions {
            allow: allow.unwrap_or_default(),
            paranoid,
            timeout,
            build_timeout,
        };
        self.call(py, async move { inner.accept_run(&against, &opts).await })
    }

    #[pyo3(signature = (against, *, allow = None, paranoid = true, timeout = None, build_timeout = None))]
    fn submit_accept_run<'py>(
        &self,
        py: Python<'py>,
        against: Vec<String>,
        allow: Option<Vec<String>>,
        paranoid: bool,
        timeout: Option<u64>,
        build_timeout: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::AcceptOptions {
            allow: allow.unwrap_or_default(),
            paranoid,
            timeout,
            build_timeout,
        };
        self.call(py, async move {
            Ok(queued(inner.submit_accept_run(&against, &opts).await?))
        })
    }

    fn get_acceptance<'py>(&self, py: Python<'py>, id: String) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.get_acceptance(&id).await })
    }

    fn list_acceptances<'py>(&self, py: Python<'py>, spec: String) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.list_acceptances(&spec).await })
    }

    fn get_run<'py>(&self, py: Python<'py>, name: String) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.get_run(&name).await })
    }

    fn name_run<'py>(
        &self,
        py: Python<'py>,
        name: String,
        snapshot_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.name_run(&name, &snapshot_id).await })
    }

    fn open_document<'py>(&self, py: Python<'py>, code: String) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.open_document(&code).await })
    }

    fn edit_document<'py>(
        &self,
        py: Python<'py>,
        id: String,
        revision: u64,
        code: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move {
            inner.edit_document(&id, revision, &code).await
        })
    }

    fn close_document<'py>(&self, py: Python<'py>, id: String) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.close_document(&id).await })
    }

    /// `at` is a list of `{"line": n}` or `{"line": n, "column": m}`.
    #[pyo3(signature = (id, at, *, revision = None, mode = "before"))]
    fn document_goals<'py>(
        &self,
        py: Python<'py>,
        id: String,
        at: &Bound<'py, PyAny>,
        revision: Option<u64>,
        mode: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let at: Vec<pisa_client::LinePos> = serde_json::from_value(from_py(at)?)
            .map_err(|e| PyValueError::new_err(format!("at: {e}")))?;
        let mode = serde_json::from_value(Value::String(mode.to_string()))
            .map_err(|_| PyValueError::new_err("mode is \"before\" or \"after\""))?;
        let opts = pisa_client::GoalsOptions { revision, mode };
        self.call(
            py,
            async move { inner.document_goals(&id, &at, &opts).await },
        )
    }

    /// `at` is `{"sorry": i}` or a position as for [`Client::document_goals`].
    #[pyo3(signature = (id, at, candidates, *, revision = None))]
    fn probe_document<'py>(
        &self,
        py: Python<'py>,
        id: String,
        at: &Bound<'py, PyAny>,
        candidates: Vec<String>,
        revision: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let at: pisa_client::ProbeAt = serde_json::from_value(from_py(at)?)
            .map_err(|e| PyValueError::new_err(format!("at: {e}")))?;
        self.call(py, async move {
            let candidates: Vec<&str> = candidates.iter().map(String::as_str).collect();
            inner.probe_document(&id, at, &candidates, revision).await
        })
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
        let inner = self.inner.clone();
        let opts = pisa_client::RebuildOptions {
            dry_run,
            allow_changes,
            retire,
            verify,
        };
        self.call(py, async move {
            inner.rebuild_snapshots(&from_, &to, &labels, &opts).await
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
        let inner = self.inner.clone();
        self.call(py, async move {
            let job = inner
                .submit_rebuild_snapshots(&from_, &to, &labels, &opts)
                .await?;
            Ok(queued(job))
        })
    }

    #[pyo3(signature = (against, *, allow = None, dry_run = false, verify = None))]
    fn settle_run<'py>(
        &self,
        py: Python<'py>,
        against: Vec<String>,
        allow: Option<Vec<String>>,
        dry_run: bool,
        verify: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::SettleOptions {
            allow: allow.unwrap_or_default(),
            dry_run,
            verify,
        };
        self.call(py, async move { inner.settle_run(&against, &opts).await })
    }

    #[pyo3(signature = (against, *, allow = None, dry_run = false, verify = None))]
    fn submit_settle_run<'py>(
        &self,
        py: Python<'py>,
        against: Vec<String>,
        allow: Option<Vec<String>>,
        dry_run: bool,
        verify: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::SettleOptions {
            allow: allow.unwrap_or_default(),
            dry_run,
            verify,
        };
        self.call(py, async move {
            Ok(queued(inner.submit_settle_run(&against, &opts).await?))
        })
    }

    #[pyo3(signature = (from_, to, *, dry_run = false, labels = None, verify = None))]
    fn resubmit_snapshots<'py>(
        &self,
        py: Python<'py>,
        from_: String,
        to: String,
        dry_run: bool,
        labels: Option<BTreeMap<String, String>>,
        verify: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let opts = pisa_client::ResubmitOptions {
            dry_run,
            labels,
            verify,
        };
        self.call(py, async move {
            inner.resubmit_snapshots(&from_, &to, &opts).await
        })
    }

    #[pyo3(signature = (from_, to, *, dry_run = false, labels = None, verify = None))]
    fn submit_resubmit_snapshots<'py>(
        &self,
        py: Python<'py>,
        from_: String,
        to: String,
        dry_run: bool,
        labels: Option<BTreeMap<String, String>>,
        verify: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts = pisa_client::ResubmitOptions {
            dry_run,
            labels,
            verify,
        };
        let inner = self.inner.clone();
        self.call(py, async move {
            let job = inner.submit_resubmit_snapshots(&from_, &to, &opts).await?;
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
        let inner = self.inner.clone();
        let options = check_options(detail, at)?;
        self.call(py, async move {
            let job = inner.submit_check_with(&code, &options).await?;
            Ok(queued(job))
        })
    }

    fn submit_start_session<'py>(
        &self,
        py: Python<'py>,
        code: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move {
            Ok(queued(inner.submit_start_session(&code).await?))
        })
    }

    fn submit_apply_tactic<'py>(
        &self,
        py: Python<'py>,
        state_id: String,
        tactic: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move {
            Ok(queued(inner.submit_apply_tactic(&state_id, &tactic).await?))
        })
    }

    fn submit_check_session<'py>(
        &self,
        py: Python<'py>,
        session_id: String,
        code: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move {
            Ok(queued(
                inner.submit_check_session(&session_id, &code).await?,
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
        let inner = self.inner.clone();
        let job = job_of(job)?;
        self.call(py, async move {
            let wait = Duration::from_secs(wait);
            Ok(match inner.poll_job(&job, wait).await? {
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
        let inner = self.inner.clone();
        let job = job_of(job)?;
        self.call(py, async move { inner.wait_job(&job).await })
    }

    fn cancel_job<'py>(
        &self,
        py: Python<'py>,
        job: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let job = job_of(job)?;
        self.call(py, async move { inner.cancel_job(&job).await })
    }

    #[pyo3(signature = (
        text = None, *, description = None, latex = None, pattern = None, goal = None,
        name = None, consts = None, libraries = None, kinds = None, limit = 5,
        include_sorried = None, snapshots = None, format = None
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
        consts: Option<Vec<String>>,
        libraries: Option<Vec<String>>,
        kinds: Option<Vec<String>>,
        limit: usize,
        include_sorried: Option<bool>,
        snapshots: Option<Bound<'py, PyAny>>,
        format: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let snapshots = match snapshots {
            Some(s) => serde_json::from_value(from_py(&s)?)
                .map_err(|e| PyValueError::new_err(format!("invalid snapshots: {e}")))?,
            None => Default::default(),
        };
        let format = match format {
            Some(f) => serde_json::from_value(serde_json::Value::String(f))
                .map_err(|e| PyValueError::new_err(format!("invalid format: {e}")))?,
            None => Default::default(),
        };
        let query = pisa_client::Query {
            text,
            description,
            latex,
            pattern,
            goal,
            name,
            consts: consts.unwrap_or_default(),
            libraries: libraries.unwrap_or_default(),
            kinds: kinds.unwrap_or_default(),
            limit,
            include_sorried,
            snapshots,
            format,
        };
        self.call(py, async move { inner.search(&query).await })
    }

    fn search_batch<'py>(
        &self,
        py: Python<'py>,
        queries: Vec<Bound<'py, PyDict>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        let queries = queries
            .iter()
            .map(|q| {
                serde_json::from_value(from_py(q.as_any())?)
                    .map_err(|e| PyValueError::new_err(format!("invalid query: {e}")))
            })
            .collect::<PyResult<Vec<pisa_client::Query>>>()?;
        self.call(py, async move { inner.search_batch(queries).await })
    }

    fn resolve<'py>(&self, py: Python<'py>, names: Vec<String>) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move { inner.resolve(names).await })
    }

    #[pyo3(signature = (name, *, module = None))]
    fn get_declaration<'py>(
        &self,
        py: Python<'py>,
        name: String,
        module: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let inner = self.inner.clone();
        self.call(py, async move {
            inner.get_declaration(&name, module.as_deref()).await
        })
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Client>()?;
    m.add_class::<AsyncClient>()
}
