//! The configuration, with the keys of the command line.

use joule_profiler_config::{Map, ProfilerConfig, SourceName, Value, configured_sources};
use joule_profiler_core::JouleProfiler;
use joule_profiler_injector_stdout::StdoutInjectorConfig;
use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub profiler: ProfilerConfig,
    pub injector: StdoutInjectorConfig,
    pub sources: Map<String, Value>,
}

impl Config {
    fn sources(&self, named: &[String]) -> Result<Vec<(SourceName, Option<Value>)>> {
        let named = named
            .iter()
            .map(|name| name.parse())
            .collect::<Result<Vec<SourceName>, _>>()?;

        Ok(configured_sources(&named, &self.sources)?)
    }

    pub fn check_sources(&self, named: &[String]) -> Result<()> {
        self.sources(named).map(drop)
    }

    pub fn profiler(&self, named: &[String]) -> Result<JouleProfiler> {
        let mut profiler = JouleProfiler::new();
        profiler.set_defer(self.profiler.defer);
        for (name, table) in self.sources(named)? {
            if let Some(source) = name.build(table)? {
                profiler.add_source(source);
            }
        }
        Ok(profiler)
    }
}
