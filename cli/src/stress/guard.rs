//! Process isolation with ordinary CLI stack limits and reaped deadlines.

use super::{FilePair, Guarded, Inspection};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) enum Mode {
    Document,
    Islands,
}

#[derive(Serialize, Deserialize)]
struct Request {
    path: String,
    before: String,
    after: String,
    mode: Mode,
}

/// Worker process reaped on every exit from its owner.
struct Worker {
    process: Child,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

pub(super) fn worker() -> Result<()> {
    let input = std::io::stdin();
    let request: Request = serde_json::from_reader(input.lock())?;
    let inspection = match request.mode {
        Mode::Document => super::inspect_document(&request.path, &request.before, &request.after),
        Mode::Islands => super::inspect_islands(&request.path, &request.before, &request.after),
    };
    let inspection = inspection?;
    let output = std::io::stdout();
    serde_json::to_writer(output.lock(), &inspection)?;
    Ok(())
}

pub(super) fn inspect(pair: &FilePair, timeout: Duration, mode: Mode) -> Guarded<Inspection> {
    let result = run(pair, timeout, mode);
    match result {
        Err(error) => Guarded::Error(format!("{error:#}")),
        Ok(outcome) => outcome,
    }
}

fn run(pair: &FilePair, timeout: Duration, mode: Mode) -> Result<Guarded<Inspection>> {
    let directory = tempfile::tempdir()?;
    let input_path = directory.path().join("request.json");
    let output_path = directory.path().join("response.json");
    let error_path = directory.path().join("stderr.txt");
    let input = File::create(&input_path)?;
    let request = Request {
        path: pair.path.clone(),
        before: pair.before.clone(),
        after: pair.after.clone(),
        mode,
    };
    serde_json::to_writer(input, &request)?;
    let input = File::open(&input_path)?;
    let output = File::create(&output_path)?;
    let errors = File::create(&error_path)?;
    let executable = std::env::current_exe()?;
    let process = Command::new(executable)
        .arg("--worker")
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(errors))
        .spawn()
        .context("failed to start stress worker")?;
    let mut worker = Worker { process };
    let status = wait(&mut worker.process, timeout)?;
    let Some(status) = status else {
        return Ok(Guarded::Timeout);
    };
    if !status.success() {
        let stderr = std::fs::read_to_string(&error_path)?;
        let detail = format!("worker {status}: {}", stderr.trim());
        let outcome = if status.code() == Some(1) {
            Guarded::Error(detail)
        } else {
            Guarded::Panic(detail)
        };
        return Ok(outcome);
    }
    let output = File::open(output_path)?;
    let inspection = serde_json::from_reader(output).context("invalid stress worker response")?;
    Ok(Guarded::Done(inspection))
}

/// Wait until exit or terminate and reap the process at its deadline.
fn wait(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>> {
    let started = Instant::now();
    loop {
        let status = child.try_wait()?;
        if let Some(status) = status {
            return Ok(Some(status));
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let killed = child.kill();
            let reaped = child.wait();
            reaped?;
            killed?;
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_terminates_and_reaps_the_worker() {
        let mut child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start sleeping process");
        let result = wait(&mut child, Duration::from_millis(20)).expect("enforce deadline");
        assert!(result.is_none());
        let status = child.try_wait().expect("inspect reaped process");
        assert!(status.is_some_and(|status| !status.success()));
    }
}
