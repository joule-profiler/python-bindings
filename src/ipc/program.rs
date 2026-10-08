use std::io::{self, BufRead, BufReader, PipeReader, Read as _, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command};

use crate::ipc::{IpcError, PHASE_DECLARATION_MESSAGE, Read, Received, Request, STARTED_MESSAGE};

/// The program's end of a session.
pub struct IpcSession {
    control: UnixStream,
}

impl IpcSession {
    /// Gives `command` the control socket as standard input and the results pipe as standard
    /// output.
    pub fn spawn(mut command: Command, config: &str) -> Result<(Self, Results), IpcError> {
        let (control, theirs) = UnixStream::pair()?;
        let (results, written) = io::pipe()?;
        command.stdin(OwnedFd::from(theirs)).stdout(written);
        let mut spawned = command.spawn()?;

        drop(command);

        let mut session = Self { control };
        let mut results = Results::new(BufReader::new(results));
        let started = session
            .send(&Request::Configure(config.to_owned()))
            .and_then(|()| wait(&mut spawned))
            .and_then(|()| session.answer(STARTED_MESSAGE));

        match started {
            Ok(()) => Ok((session, results)),
            Err(error) => match results.failure() {
                IpcError::Closed => Err(error),
                failure => Err(failure),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn connect(
        control: UnixStream,
        results: BufReader<PipeReader>,
    ) -> Result<(Self, Results), IpcError> {
        let mut session = Self { control };
        let mut results = Results::new(results);

        match session.answer(STARTED_MESSAGE) {
            Ok(()) => Ok((session, results)),
            Err(_) => Err(results.failure()),
        }
    }

    /// Returns once the profiler has woken its sensors.
    pub fn phase(&mut self, name: &str) -> Result<(), IpcError> {
        self.send(&Request::Phase(name.to_owned()))?;
        self.answer(PHASE_DECLARATION_MESSAGE)
    }

    pub fn finish(mut self, exit_code: i32) -> Result<(), IpcError> {
        self.send(&Request::End(exit_code))
    }

    fn send(&mut self, request: &Request) -> Result<(), IpcError> {
        let mut line = serde_json::to_vec(request).map_err(|_| IpcError::Protocol)?;
        line.push(b'\n');
        self.control.write_all(&line).map_err(|_| IpcError::Closed)
    }

    fn answer(&mut self, expected: u8) -> Result<(), IpcError> {
        let mut answer = [0];
        match self.control.read_exact(&mut answer) {
            Ok(()) if answer[0] == expected => Ok(()),
            Ok(()) => Err(IpcError::Protocol),
            Err(_) => Err(IpcError::Closed),
        }
    }
}

/// The schema, the phases, then the summary or an error.
pub struct Results {
    reader: BufReader<PipeReader>,
    line: String,
    over: bool,
}

impl Results {
    fn new(reader: BufReader<PipeReader>) -> Self {
        Self {
            reader,
            line: String::new(),
            over: false,
        }
    }

    pub fn failure(&mut self) -> IpcError {
        self.find_map(Result::err).unwrap_or(IpcError::Closed)
    }

    /// Whether `next` returns without waiting for the profiler. A line read but not taken yet
    /// stays in the buffer of the reader, not in the pipe. A line partly in the pipe is read
    /// whole: the profiler is writing its end.
    pub fn ready(&self) -> bool {
        self.over || !self.reader.buffer().is_empty() || readable(self.reader.get_ref())
    }
}

impl Iterator for Results {
    type Item = Result<Received, IpcError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.over {
            self.line.clear();
            match self.reader.read_line(&mut self.line) {
                Ok(0) => {
                    self.over = true;
                    return Some(Err(IpcError::Closed));
                }
                Ok(_) => {}
                Err(error) => {
                    self.over = true;
                    return Some(Err(error.into()));
                }
            }

            match serde_json::from_str::<Read>(&self.line) {
                Ok(Read::Schema(schema)) => return Some(Ok(Received::Schema(schema))),
                Ok(Read::Phase(phase)) => return Some(Ok(Received::Phase(phase))),
                Ok(Read::Summary(summary)) => {
                    self.over = true;
                    return Some(Ok(Received::Summary(summary)));
                }
                Ok(Read::Error(message)) => {
                    self.over = true;
                    return Some(Err(IpcError::Profiler(message)));
                }
                Err(error) => log::debug!("not a result, skipped ({error}): {:?}", self.line),
            }
        }
        None
    }
}

/// Whether reading `pipe` returns at once: it holds data, or its writer is closed.
fn readable(pipe: &PipeReader) -> bool {
    let mut request = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `request` is one valid `pollfd`, and a timeout of 0 never waits.
    unsafe { libc::poll(&raw mut request, 1, 0) > 0 }
}

/// Wait for the child to finish, if the error is ECHILD,
/// then the child has already been reaped and the profiler is detached.
fn wait(spawned: &mut Child) -> Result<(), IpcError> {
    match spawned.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(IpcError::Spawned(status)),
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use joule_profiler_core::exporter::Exporter;
    use joule_profiler_core::phase::Summary;
    use joule_profiler_core::schema::Schema;

    use super::*;
    use crate::ipc::profiler::ResultsWriter;

    #[test]
    fn only_the_results_already_written_are_ready() {
        let (pipe, written) = io::pipe().unwrap();
        let mut results = Results::new(BufReader::new(pipe));
        let mut writer = ResultsWriter::new(written);

        assert!(!results.ready(), "nothing is written yet");
        writer.begin(&Schema::default()).unwrap();
        writer.finish(&Summary::default()).unwrap();

        assert!(results.ready());
        results.next();
        assert!(results.ready(), "the summary came with the schema");
        results.next();
        assert!(results.ready(), "the results are over");
    }

    #[test]
    fn a_profiler_that_stopped_is_not_waited_for() {
        let (pipe, written) = io::pipe().unwrap();
        let results = Results::new(BufReader::new(pipe));

        drop(written);

        assert!(results.ready());
    }
}
