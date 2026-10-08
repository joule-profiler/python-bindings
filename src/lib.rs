mod config;
mod error;
mod ipc;
mod stream;

use pyo3::prelude::*;

#[pymodule]
mod _core {
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError, mpsc};
    use std::thread;

    use joule_profiler_core::metric::MetricValue;
    use joule_profiler_core::schema::Schema;
    use joule_profiler_injector_stdout::StdoutInjector;
    use pyo3::prelude::*;
    use pyo3::types::PyTuple;
    use toml::Value;

    use crate::config::{Config, ConfigTable};
    use crate::error::{Error, Result};
    use crate::ipc::PhaseValues;
    use crate::ipc::profiler::serve_spawned;
    use crate::ipc::program::IpcSession;
    use crate::stream::{Stream, ToPython};

    #[pyclass(frozen, name = "_Profiler", module = "joule_profiler._core")]
    struct Profiler {
        config: Config,
        sources: Vec<String>,
    }

    #[pymethods]
    impl Profiler {
        #[new]
        fn new(sources: Vec<String>, config: Option<PathBuf>, values: &str) -> PyResult<Self> {
            let config =
                config_table(config, values).and_then(|config_table| config_table.resolve())?;
            config.check_sources(&sources)?;

            Ok(Self { config, sources })
        }

        fn list_sensors(&self, py: Python<'_>) -> PyResult<String> {
            let schema = py.detach(|| {
                self.config
                    .profiler(&self.sources)
                    .map(|profiler| profiler.schema())
            })?;

            let json: serde_json::Map<String, serde_json::Value> = schema
                .sources
                .into_iter()
                .map(|source| {
                    let metrics = source
                        .metrics
                        .into_iter()
                        .map(|metric| (metric.name, metric.unit.to_string().into()))
                        .collect();
                    (source.name, serde_json::Value::Object(metrics))
                })
                .collect();
            Ok(serde_json::Value::Object(json).to_string())
        }

        fn info(&self, py: Python<'_>) -> PyResult<String> {
            let sections = py.detach(|| {
                self.config
                    .profiler(&self.sources)
                    .map(|profiler| profiler.info())
            })?;

            let json: serde_json::Map<String, serde_json::Value> = sections
                .into_iter()
                .map(|(name, info)| {
                    let info = serde_json::to_value(info).expect("an info is valid JSON");
                    (name, info)
                })
                .collect();
            Ok(serde_json::Value::Object(json).to_string())
        }

        #[pyo3(signature = (command, token_pattern = None, stdout_file = None, use_root = None))]
        fn profile(
            &self,
            py: Python<'_>,
            command: Vec<String>,
            token_pattern: Option<&str>,
            stdout_file: Option<PathBuf>,
            use_root: Option<bool>,
        ) -> PyResult<Run> {
            let injector = &self.config.injector;
            let opened = py.detach(|| -> Result<_> {
                let mut profiler = self.config.profiler(&self.sources)?;
                let pattern = token_pattern.unwrap_or(&injector.token_pattern);

                profiler.set_injector(
                    StdoutInjector::new(command, pattern)?
                        .use_root(use_root.unwrap_or(injector.use_root))
                        .output_file(stdout_file.or_else(|| injector.stdout_file.clone())),
                );

                let (sender, received) = mpsc::channel();
                profiler.set_exporter(ToPython::new(sender.clone()));
                thread::spawn(move || {
                    if let Err(error) = profiler.profile() {
                        let _ = sender.send(Err(error.into()));
                    }
                });

                Stream::open(received)
            })?;

            Ok(Run::new(opened))
        }

        fn session(&self, py: Python<'_>, command: Vec<String>) -> PyResult<Session> {
            let config = serde_json::to_string(&(&self.sources, &self.config))
                .expect("a configuration is valid JSON");
            let mut arguments = command.into_iter();
            let program = arguments.next().ok_or(Error::NoCommand)?;
            let mut command = Command::new(program);
            command.args(arguments);

            let (control, opened) = py.detach(|| -> Result<_> {
                let (control, results) = IpcSession::spawn(command, &config)?;
                let opened = Stream::open(results)?;
                Ok((control, opened))
            })?;

            Ok(Session {
                control: Mutex::new(Some(control)),
                run: Py::new(py, Run::new(opened))?,
            })
        }
    }

    #[pyfunction]
    #[pyo3(name = "_serve")]
    fn serve(py: Python<'_>) -> PyResult<()> {
        py.detach(|| {
            serve_spawned(|config| {
                let (sources, config): (Vec<String>, Config) = serde_json::from_str(config)?;
                Ok(config.profiler(&sources)?)
            })
        })
        .map_err(Error::from)?;
        Ok(())
    }

    fn config_table(file: Option<PathBuf>, values: &str) -> Result<ConfigTable> {
        let mut config_table = match file {
            Some(path) => ConfigTable::read(&path)?,
            None => ConfigTable::default(),
        };
        let values: Vec<(String, Value)> = serde_json::from_str(values).map_err(Error::Values)?;
        for (key, value) in values {
            config_table.set(&key, value)?;
        }
        Ok(config_table)
    }

    #[pyclass(frozen, name = "Run", module = "joule_profiler")]
    struct Run {
        schema: Schema,
        stream: Mutex<Stream>,
    }

    impl Run {
        fn new((schema, stream): (Schema, Stream)) -> Self {
            Self {
                schema,
                stream: Mutex::new(stream),
            }
        }

        /// Take it with the GIL released.
        fn stream(&self) -> MutexGuard<'_, Stream> {
            self.stream.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn phase(&self, py: Python<'_>, phase: Option<PhaseValues>) -> PyResult<Option<Phase>> {
            phase
                .map(|phase| Phase::new(py, &self.schema, phase))
                .transpose()
        }
    }

    #[pymethods]
    impl Run {
        fn __iter__(this: PyRef<'_, Self>) -> PyRef<'_, Self> {
            this
        }

        fn __next__(&self, py: Python<'_>) -> PyResult<Option<Phase>> {
            let phase = py.detach(|| self.stream().next_phase())?;
            self.phase(py, phase)
        }

        /// A phase that already ended, or `None` without waiting.
        fn poll(&self, py: Python<'_>) -> PyResult<Option<Phase>> {
            let phase = match self.stream.try_lock() {
                Ok(mut stream) => stream.try_next_phase()?,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().try_next_phase()?,
                Err(TryLockError::WouldBlock) => None,
            };
            self.phase(py, phase)
        }

        #[getter]
        fn summary(&self, py: Python<'_>) -> PyResult<Summary> {
            let summary = py.detach(|| self.stream().summary())?;
            Ok(Summary {
                timestamp_us: summary.timestamp_us,
                duration_ms: summary.duration_ms,
                phases: summary.phases,
                exit_code: summary.exit_code,
            })
        }
    }

    #[pyclass(frozen, name = "_Session", module = "joule_profiler._core")]
    struct Session {
        control: Mutex<Option<IpcSession>>,
        run: Py<Run>,
    }

    #[pymethods]
    impl Session {
        #[getter]
        fn run(&self, py: Python<'_>) -> Py<Run> {
            self.run.clone_ref(py)
        }

        fn phase(&self, py: Python<'_>, name: &str) -> PyResult<()> {
            let run = self.run.get();
            py.detach(|| -> Result<()> {
                let mut control = self.control();
                let control = control.as_mut().ok_or(Error::SessionOver)?;
                control
                    .phase(name)
                    .map_err(|error| match run.stream().summary() {
                        Err(failure) => failure,
                        Ok(_) => error.into(),
                    })
            })
            .map_err(Into::into)
        }

        fn finish(&self, py: Python<'_>, exit_code: i32) -> PyResult<()> {
            let run = self.run.get();
            py.detach(|| -> Result<()> {
                let control = self.control().take().ok_or(Error::SessionOver)?;
                let ended = control.finish(exit_code);
                run.stream().summary()?;
                Ok(ended?)
            })
            .map_err(Into::into)
        }
    }

    impl Session {
        fn control(&self) -> MutexGuard<'_, Option<IpcSession>> {
            self.control.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    #[pyclass(frozen, get_all, name = "Summary", module = "joule_profiler")]
    struct Summary {
        timestamp_us: u128,
        duration_ms: u64,
        phases: usize,
        exit_code: Option<i32>,
    }

    #[pymethods]
    impl Summary {
        fn __repr__(&self) -> String {
            let exit_code = self
                .exit_code
                .map_or_else(|| "None".to_owned(), |code| code.to_string());
            format!(
                "Summary(phases={}, duration_ms={}, exit_code={exit_code})",
                self.phases, self.duration_ms,
            )
        }
    }

    #[pyclass(frozen, get_all, name = "Phase", module = "joule_profiler")]
    struct Phase {
        index: usize,
        name: String,
        start_token: String,
        end_token: String,
        start_line: Option<usize>,
        end_line: Option<usize>,
        timestamp_us: u128,
        duration_ms: u64,
        sources: Py<PyTuple>,
    }

    impl Phase {
        fn new(py: Python<'_>, schema: &Schema, phase: PhaseValues) -> PyResult<Self> {
            let sources = schema
                .sources
                .iter()
                .zip(phase.values)
                .map(|(source, values)| {
                    let metrics = source
                        .metrics
                        .iter()
                        .zip(values)
                        .map(|(metric, value)| {
                            Py::new(
                                py,
                                Metric {
                                    name: metric.name.clone(),
                                    value,
                                    unit: metric.unit.to_string(),
                                },
                            )
                        })
                        .collect::<PyResult<Vec<_>>>()?;
                    Py::new(
                        py,
                        SourceMetrics {
                            name: source.name.clone(),
                            metrics: PyTuple::new(py, metrics)?.unbind(),
                        },
                    )
                })
                .collect::<PyResult<Vec<_>>>()?;

            let info = phase.info;
            Ok(Self {
                index: info.index,
                name: info.name(),
                start_token: info.start_token,
                end_token: info.end_token,
                start_line: info.start_line,
                end_line: info.end_line,
                timestamp_us: info.timestamp_us,
                duration_ms: info.duration_ms,
                sources: PyTuple::new(py, sources)?.unbind(),
            })
        }
    }

    #[pymethods]
    impl Phase {
        fn __repr__(&self) -> String {
            format!(
                "Phase(index={}, name={:?}, duration_ms={})",
                self.index, self.name, self.duration_ms
            )
        }
    }

    #[pyclass(frozen, get_all, name = "SourceMetrics", module = "joule_profiler")]
    struct SourceMetrics {
        name: String,
        metrics: Py<PyTuple>,
    }

    #[pymethods]
    impl SourceMetrics {
        fn __repr__(&self, py: Python<'_>) -> String {
            format!(
                "SourceMetrics(name={:?}, metrics={})",
                self.name,
                self.metrics.bind(py).len()
            )
        }
    }

    #[pyclass(frozen, name = "Metric", module = "joule_profiler")]
    struct Metric {
        #[pyo3(get)]
        name: String,
        value: MetricValue,
        #[pyo3(get)]
        unit: String,
    }

    #[pymethods]
    impl Metric {
        #[getter]
        fn value<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
            Ok(match self.value {
                MetricValue::U64(value) => value.into_pyobject(py)?.into_any(),
                MetricValue::I64(value) => value.into_pyobject(py)?.into_any(),
                MetricValue::F64(value) => value.into_pyobject(py)?.into_any(),
            })
        }

        fn __repr__(&self) -> String {
            format!(
                "Metric(name={:?}, value={}, unit={:?})",
                self.name, self.value, self.unit
            )
        }
    }
}
