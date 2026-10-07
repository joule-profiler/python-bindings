//! The configuration, with the keys of the command line.

use std::convert::Infallible;
use std::fs;
use std::path::{Path, PathBuf};

use joule_profiler_core::profiler::JouleProfiler;
use joule_profiler_core::source::Source;
use joule_profiler_injector_stdout::DEFAULT_PATTERN;
use joule_profiler_source_amdsmi::AmdSmi;
use joule_profiler_source_cgroup::Cgroup;
use joule_profiler_source_nvml::Nvml;
use joule_profiler_source_perf_event::PerfEvent;
use joule_profiler_source_procfs::Procfs;
use joule_profiler_source_rapl::Rapl;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

use crate::error::{Error, Result};

type Build = fn(&'static str, Option<Value>) -> Result<Option<Source>>;

const SOURCES: [(&str, Build); 6] = [
    ("rapl", |name, table| build(name, table, Rapl::build)),
    ("perf", |name, table| {
        build(name, table, |perf: PerfEvent| {
            Ok::<_, Infallible>(perf.build())
        })
    }),
    ("procfs", |name, table| build(name, table, Procfs::build)),
    ("cgroup", |name, table| build(name, table, Cgroup::build)),
    ("nvml", |name, table| build(name, table, Nvml::build)),
    ("amdsmi", |name, table| build(name, table, AmdSmi::build)),
];

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub profiler: ProfilerConfig,
    pub injector: InjectorConfig,
    pub sources: Table,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfilerConfig {
    pub defer: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InjectorConfig {
    pub token_pattern: String,
    pub stdout_file: Option<PathBuf>,
    pub use_root: bool,
}

impl Default for InjectorConfig {
    fn default() -> Self {
        Self {
            token_pattern: DEFAULT_PATTERN.to_owned(),
            stdout_file: None,
            use_root: false,
        }
    }
}

impl Config {
    fn sources(&self, named: &[String]) -> Result<Vec<(&'static str, Build, Option<Value>)>> {
        let mut sources = named
            .iter()
            .map(|name| source(name).map(|(name, build)| (name, build, None)))
            .collect::<Result<Vec<_>>>()?;

        for (key, table) in &self.sources {
            let (name, build) = source(key)?;
            match sources.iter_mut().find(|(known, ..)| *known == name) {
                Some((.., configured)) => *configured = Some(table.clone()),
                None => sources.push((name, build, Some(table.clone()))),
            }
        }
        if sources.is_empty() {
            let (name, build) = SOURCES[0];
            sources.push((name, build, None));
        }
        Ok(sources)
    }

    pub fn check_sources(&self, named: &[String]) -> Result<()> {
        self.sources(named).map(drop)
    }

    pub fn profiler(&self, named: &[String]) -> Result<JouleProfiler> {
        let mut profiler = JouleProfiler::new();
        profiler.set_defer(self.profiler.defer);
        for (name, build, table) in self.sources(named)? {
            if let Some(source) = build(name, table)? {
                profiler.add_source(source);
            }
        }
        Ok(profiler)
    }
}

fn source(name: &str) -> Result<(&'static str, Build)> {
    let wanted = if name == "perf_event" { "perf" } else { name };
    SOURCES
        .into_iter()
        .find(|(known, _)| *known == wanted)
        .ok_or_else(|| Error::UnknownSource {
            name: name.to_owned(),
            known: SOURCES.map(|(name, _)| name).join(", "),
        })
}

/// A source that fails to build is left out if its table sets `ignore_on_failure`.
fn build<C, E>(
    name: &'static str,
    mut table: Option<Value>,
    build: impl FnOnce(C) -> Result<Source, E>,
) -> Result<Option<Source>>
where
    C: DeserializeOwned,
    E: std::error::Error + Send + Sync + 'static,
{
    let ignore_on_failure = table
        .as_mut()
        .and_then(Value::as_table_mut)
        .and_then(|table| table.remove("ignore_on_failure"))
        .and_then(|ignore| ignore.as_bool())
        .unwrap_or(false);
    let config: C = table
        .unwrap_or_else(|| Value::Table(Table::new()))
        .try_into()
        .map_err(|error| Error::SourceTable(name, error))?;

    match build(config) {
        Ok(source) => Ok(Some(source)),
        Err(error) if ignore_on_failure => {
            log::warn!("source `{name}` left out: {error}");
            Ok(None)
        }
        Err(error) => Err(Error::Unavailable(name, Box::new(error))),
    }
}

#[derive(Debug, Default)]
pub struct Settings(Table);

impl Settings {
    pub fn read(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|error| Error::Read(path.into(), error))?;
        let table = toml::from_str(&text).map_err(|error| Error::Parse(path.into(), error))?;
        Ok(Self(table))
    }

    pub fn set(&mut self, key: &str, value: Value) -> Result<()> {
        let segments: Vec<&str> = key.split('.').map(str::trim).collect();
        if segments.iter().any(|segment| segment.is_empty()) {
            return Err(Error::Key(key.to_owned()));
        }
        let Some((last, parents)) = segments.split_last() else {
            return Err(Error::Key(key.to_owned()));
        };

        let mut table = &mut self.0;
        for segment in parents {
            table = table
                .entry(*segment)
                .or_insert_with(|| Value::Table(Table::new()))
                .as_table_mut()
                .ok_or_else(|| Error::NotATable((*segment).to_owned()))?;
        }
        table.insert((*last).to_owned(), value);
        Ok(())
    }

    pub fn resolve(&self) -> Result<Config> {
        self.0.clone().try_into().map_err(Error::Config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(values: &[(&str, Value)]) -> Settings {
        let mut settings = Settings::default();
        for (key, value) in values {
            settings.set(key, value.clone()).unwrap();
        }
        settings
    }

    fn names(sources: &[(&str, Build, Option<Value>)]) -> Vec<String> {
        sources
            .iter()
            .map(|(name, ..)| (*name).to_owned())
            .collect()
    }

    #[test]
    fn a_dotted_key_sets_a_nested_value() {
        let Settings(table) = settings(&[("sources.rapl.sockets", Value::Array(vec![0.into()]))]);

        assert_eq!(
            table["sources"]["rapl"]["sockets"],
            Value::Array(vec![0.into()])
        );
    }

    #[test]
    fn a_misspelled_key_or_a_key_through_a_value_is_refused() {
        let error = settings(&[("profiler.defr", true.into())])
            .resolve()
            .unwrap_err();
        assert!(error.to_string().contains("defr"), "{error}");

        let mut settings = settings(&[("injector.use_root", true.into())]);
        assert!(settings.set("injector.use_root.more", 1.into()).is_err());
        assert!(settings.set("profiler..defer", true.into()).is_err());
    }

    #[test]
    fn named_sources_come_first_then_the_configured_ones_and_rapl_by_default() {
        let config = settings(&[
            ("sources.nvml.minimal", false.into()),
            ("sources.perf_event.events", Value::Array(Vec::new())),
        ])
        .resolve()
        .unwrap();

        let sources = config.sources(&["procfs".into(), "perf".into()]).unwrap();
        assert_eq!(names(&sources), ["procfs", "perf", "nvml"]);
        assert!(sources[1].2.is_some());

        let none = Config::default().sources(&[]).unwrap();
        assert_eq!(names(&none), ["rapl"]);
    }

    #[test]
    fn an_unknown_source_is_refused() {
        let error = Config::default()
            .check_sources(&["nope".into()])
            .unwrap_err();
        assert!(error.to_string().contains("`nope`"), "{error}");

        let config = settings(&[("sources.rpl.unit", "joule".into())])
            .resolve()
            .unwrap();
        assert!(config.check_sources(&[]).is_err());
    }

    #[test]
    fn a_source_that_cannot_be_built_is_left_out_when_asked() {
        let broken = "sockets = [4294967295]";
        let table = |toml: &str| Some(Value::Table(toml::from_str(toml).unwrap()));

        let ignored = build(
            "rapl",
            table(&format!("{broken}\nignore_on_failure = true")),
            Rapl::build,
        );
        assert!(ignored.unwrap().is_none());

        let Err(error) = build("rapl", table(broken), Rapl::build) else {
            panic!("a broken source was built");
        };
        assert!(error.to_string().contains("rapl"), "{error}");
    }

    #[test]
    fn a_table_that_does_not_describe_its_source_is_refused() {
        let table = Some(Value::Table(toml::from_str("globl = true").unwrap()));

        let Err(error) = build("procfs", table, Procfs::build) else {
            panic!("a misspelled key was accepted");
        };
        assert!(error.to_string().contains("globl"), "{error}");
    }
}
