use async_process::Command;
use futures_lite::AsyncWriteExt;
use std::process::{Output, Stdio};
use tracing::{error, info};

pub fn stdout_as_str(output: &Output) -> &str {
    std::str::from_utf8(output.stdout.as_ref()).unwrap()
}

pub fn stderr_as_str(output: &Output) -> &str {
    std::str::from_utf8(output.stderr.as_ref()).unwrap()
}

#[tracing::instrument(skip(data))]
pub async fn run_shell_command_with_stdin(
    command: &str,
    args: Vec<&str>,
    data: &[u8],
) -> Result<String, String> {
    info!("starting");
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(data)
        .await
        .unwrap();

    let exit_status = child.status().await.unwrap();
    let output = child.output().await.unwrap();

    if !exit_status.success() {
        error!(
            status = exit_status.code().unwrap(),
            error = stderr_as_str(&output),
            "Unexpected Exit status"
        );
        return Err(stderr_as_str(&output).to_string());
    }

    let stdout = stdout_as_str(&output);
    info!(output = stdout, "done");

    return Ok(stdout.to_string());
}

#[tracing::instrument]
pub async fn run_shell_command(command: &str, args: Vec<&str>) -> Result<String, String> {
    info!("starting");
    let mut child = Command::new(command)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let exit_status = child.status().await.unwrap();
    let output = child.output().await.unwrap();

    if !exit_status.success() {
        error!(
            status = exit_status.code().unwrap(),
            error = stderr_as_str(&output),
            "Unexpected Exit status"
        );
        return Err(stderr_as_str(&output).to_string());
    }

    let stdout = stdout_as_str(&output);
    info!(output = stdout, "done");

    return Ok(stdout.to_string());
}
