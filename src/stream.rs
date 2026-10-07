use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::mpsc::Sender;

use joule_profiler_core::exporter::Exporter;
use joule_profiler_core::phase::{PhaseInfo, SourceMetrics, Summary};
use joule_profiler_core::schema::Schema;

use crate::error::{Error, Result};
use crate::ipc::{IpcError, PhaseValues, Received};

pub type Sent = Result<Received>;

pub struct ToPython(Sender<Sent>);

impl ToPython {
    pub fn new(sender: Sender<Sent>) -> Self {
        Self(sender)
    }

    fn send(&self, received: Received) {
        let _ = self.0.send(Ok(received));
    }
}

impl Exporter for ToPython {
    type Error = Infallible;

    fn begin(&mut self, schema: &Schema) -> std::result::Result<(), Infallible> {
        self.send(Received::Schema(schema.clone()));
        Ok(())
    }

    fn export(
        &mut self,
        phase: &PhaseInfo,
        sources: &[SourceMetrics<'_>],
    ) -> std::result::Result<(), Infallible> {
        self.send(Received::Phase(PhaseValues::new(phase, sources)));
        Ok(())
    }

    fn finish(&mut self, summary: &Summary) -> std::result::Result<(), Infallible> {
        self.send(Received::Summary(*summary));
        Ok(())
    }
}

/// Phases read by `summary` stay queued until taken.
pub struct Stream {
    results: Box<dyn Iterator<Item = Result<Received>> + Send>,
    phases: VecDeque<PhaseValues>,
    summary: Option<Summary>,
}

impl Stream {
    pub fn open(
        results: impl Iterator<Item = Result<Received>> + Send + 'static,
    ) -> Result<(Schema, Self)> {
        let mut stream = Self {
            results: Box::new(results),
            phases: VecDeque::new(),
            summary: None,
        };
        match stream.next()? {
            Received::Schema(schema) => Ok((schema, stream)),
            Received::Phase(_) | Received::Summary(_) => Err(IpcError::Protocol.into()),
        }
    }

    pub fn next_phase(&mut self) -> Result<Option<PhaseValues>> {
        while self.phases.is_empty() && self.summary.is_none() {
            self.read()?;
        }
        Ok(self.phases.pop_front())
    }

    pub fn summary(&mut self) -> Result<Summary> {
        loop {
            if let Some(summary) = self.summary {
                return Ok(summary);
            }
            self.read()?;
        }
    }

    fn read(&mut self) -> Result<()> {
        match self.next()? {
            Received::Phase(phase) => self.phases.push_back(phase),
            Received::Summary(summary) => self.summary = Some(summary),
            Received::Schema(_) => return Err(IpcError::Protocol.into()),
        }
        Ok(())
    }

    fn next(&mut self) -> Result<Received> {
        self.results.next().unwrap_or(Err(Error::Stopped))
    }
}

#[cfg(test)]
mod tests {
    use joule_profiler_core::error::Error as ProfilerError;
    use joule_profiler_core::metric::MetricValue;

    use super::*;

    fn phase(index: usize) -> PhaseValues {
        PhaseValues {
            info: PhaseInfo {
                index,
                start_token: "START".to_owned(),
                end_token: "END".to_owned(),
                start_line: None,
                end_line: None,
                timestamp_us: 0,
                duration_ms: 0,
            },
            values: vec![vec![MetricValue::U64(1)]],
        }
    }

    fn open(items: Vec<Sent>) -> Result<(Schema, Stream)> {
        Stream::open(items.into_iter())
    }

    #[test]
    fn the_phases_come_in_order_then_the_run_is_over() {
        let (_, mut stream) = open(vec![
            Ok(Received::Schema(Schema::default())),
            Ok(Received::Phase(phase(0))),
            Ok(Received::Phase(phase(1))),
            Ok(Received::Summary(Summary::default())),
        ])
        .unwrap();

        assert_eq!(stream.next_phase().unwrap(), Some(phase(0)));
        assert_eq!(stream.next_phase().unwrap(), Some(phase(1)));
        assert_eq!(stream.next_phase().unwrap(), None);
        assert_eq!(stream.summary().unwrap(), Summary::default());
    }

    #[test]
    fn the_phases_not_taken_before_the_summary_are_kept() {
        let (_, mut stream) = open(vec![
            Ok(Received::Schema(Schema::default())),
            Ok(Received::Phase(phase(0))),
            Ok(Received::Summary(Summary::default())),
        ])
        .unwrap();

        stream.summary().unwrap();

        assert_eq!(stream.next_phase().unwrap(), Some(phase(0)));
        assert_eq!(stream.next_phase().unwrap(), None);
    }

    #[test]
    fn a_profiler_that_fails_says_why() {
        let error = open(vec![Err(ProfilerError::NoSource.into())])
            .err()
            .unwrap();

        assert_eq!(error.to_string(), ProfilerError::NoSource.to_string());
    }
}
