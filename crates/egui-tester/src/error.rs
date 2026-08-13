use std::{io, path::PathBuf, time::Duration};

/// Harness failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("required containment or display tool `{0}` is unavailable")]
    MissingTool(&'static str),

    #[error("containment layer `{layer}` refused the test: {detail}")]
    Containment { layer: &'static str, detail: String },

    #[error("{operation} `{path}`: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("command `{command}` failed with {status}: {stderr}")]
    Command {
        command: String,
        status: String,
        stderr: String,
    },

    #[error("application unit `{unit}` exited before {waiting}: {detail}")]
    ApplicationExited {
        unit: String,
        waiting: String,
        detail: String,
    },

    #[error("timed out after {timeout:?} waiting for {waiting}")]
    Timeout { waiting: String, timeout: Duration },

    #[error("condition remained unsatisfied after {timeout:?} while {waiting}: {last_mismatch}")]
    Condition {
        waiting: String,
        timeout: Duration,
        last_mismatch: String,
    },

    #[error("product verdict failed: {detail}")]
    Verdict { detail: String },

    #[error("performance budget breached for {operation}: observed {elapsed:?}, budget {budget:?}")]
    TooSlow {
        operation: String,
        budget: Duration,
        elapsed: Duration,
    },

    #[error("invalid performance observation for {operation}: {detail}")]
    Timing { operation: String, detail: String },

    #[error("X11 protocol failure while {operation}: {detail}")]
    X11 {
        operation: &'static str,
        detail: String,
    },

    #[error("Wayland protocol failure while {operation}: {detail}")]
    Wayland {
        operation: &'static str,
        detail: String,
    },

    #[error("invalid probe `{path}`: {detail}")]
    Probe { path: PathBuf, detail: String },

    #[error("invalid frame journal `{path}`: {detail}")]
    FrameJournal { path: PathBuf, detail: String },

    #[error("backend capability `{capability}` is unavailable: {detail}")]
    Unsupported {
        capability: &'static str,
        detail: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Error {
    Error::Io {
        operation,
        path: path.into(),
        source,
    }
}
