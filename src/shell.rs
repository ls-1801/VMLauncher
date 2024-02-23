use async_process::Command;
use futures_lite::AsyncWriteExt;
use std::fmt::{Display, Formatter};
use std::process::{ExitStatus, Output, Stdio};
use std::str::Utf8Error;
use strum_macros::Display;
use thiserror::Error;
use tracing::{error, info};
use tracing_error::SpanTrace;

#[derive(Error, Debug)]
pub(crate) struct ShellError {
    context: SpanTrace,
    enu: ShellErrorEnum,
}

impl Display for ShellError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("[{}] ShellError: {}", self.context, self.enu))
    }
}

impl ShellError {
    pub fn new(enu: ShellErrorEnum) -> Self {
        Self {
            context: SpanTrace::capture(),
            enu,
        }
    }
}

#[derive(Error, Debug)]
enum ShellErrorEnum {
    #[error("Binary not found")]
    BinaryNotFound(#[source] which::Error),
    #[error("Could not spawn command")]
    SpawnFailed(#[source] std::io::Error),
    #[error("Could not write to stdin")]
    WritingToStdinFailed(#[source] std::io::Error),
    #[error("Unexpected Exit Code")]
    UnexpectedExitCode(ExitStatus),
    #[error("General IO Error")]
    IOFailed(#[source] std::io::Error),
    #[error("General IO Error")]
    InvalidUTF8Output(#[source] Utf8Error),
}

type Result<T> = core::result::Result<T, ShellError>;

pub fn stdout_as_str(output: &Output) -> Result<&str> {
    std::str::from_utf8(output.stdout.as_ref())
        .map_err(|e| ShellError::new(ShellErrorEnum::InvalidUTF8Output(e)))
}

pub fn stderr_as_str(output: &Output) -> Result<&str> {
    std::str::from_utf8(output.stderr.as_ref())
        .map_err(|e| ShellError::new(ShellErrorEnum::InvalidUTF8Output(e)))
}

#[tracing::instrument(skip(data))]
pub async fn run_shell_command_with_stdin(
    command: &str,
    args: Vec<&str>,
    data: &[u8],
) -> Result<String> {
    info!("starting");
    let mut child = Command::new(
        which::which(command).map_err(|e| ShellError::new(ShellErrorEnum::BinaryNotFound(e)))?,
    )
    .args(args)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| ShellError::new(ShellErrorEnum::SpawnFailed(e)))?;

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(data)
        .await
        .map_err(|e| ShellError::new(ShellErrorEnum::WritingToStdinFailed(e)))?;

    let exit_status = child
        .status()
        .await
        .map_err(|e| ShellError::new(ShellErrorEnum::IOFailed(e)))?;
    let output = child
        .output()
        .await
        .map_err(|e| ShellError::new(ShellErrorEnum::IOFailed(e)))?;

    if !exit_status.success() {
        error!(
            status = ?exit_status.code(),
            error = stderr_as_str(&output)?,
            "Unexpected Exit status"
        );
        return Err(ShellError::new(ShellErrorEnum::UnexpectedExitCode(
            exit_status,
        )));
    }

    let stdout = stdout_as_str(&output)?;
    info!(output = stdout, "done");

    return Ok(stdout.to_string());
}

#[tracing::instrument]
pub async fn run_shell_command(command: &str, args: Vec<&str>) -> Result<String> {
    println!("{command} {}", args.join(" "));
    let mut child = Command::new(
        which::which(command).map_err(|e| ShellError::new(ShellErrorEnum::BinaryNotFound(e)))?,
    )
    .args(args)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| ShellError::new(ShellErrorEnum::SpawnFailed(e)))?;
    let exit_status = child
        .status()
        .await
        .map_err(|e| ShellError::new(ShellErrorEnum::IOFailed(e)))?;
    let output = child
        .output()
        .await
        .map_err(|e| ShellError::new(ShellErrorEnum::IOFailed(e)))?;

    let stdout = stdout_as_str(&output)?;
    info!(output = stdout, "done");

    return Ok(stdout.to_string());
}
