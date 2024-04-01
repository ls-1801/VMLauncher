use crate::nanos::NanosError::HomeDir;
use camino::Utf8PathBuf;
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use tempdir::TempDir;
use thiserror::Error;
use tracing::{error, info};
use tracing_subscriber::fmt::format;
use which::which;

use crate::network::TapUser;
use crate::qemu::LaunchConfiguration;
use crate::shell;
use crate::shell::{run_shell_command, run_shell_command_with_env, ShellError};
use crate::templates::WorkerConfiguration;

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Args {
    pub(crate) klibs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kernel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) klib_dir: Option<String>,
    pub(crate) debugflags: Vec<String>,
    pub(crate) run_config: RunConfig,
    pub(crate) use_docker: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct RunConfig {
    pub(crate) gateway: Ipv4Addr,
}

#[derive(Debug)]
pub struct UnikernelWorkerConfig {
    pub query_id: usize,
    pub node_id: usize,
    pub elf_binary: Utf8PathBuf,
    pub args: Option<String>,
    pub ip: Option<Ipv4Addr>,
}

impl UnikernelWorkerConfig {
    fn image_name(&self) -> String {
        format!("unikernel_{}_{}", self.query_id, self.node_id)
    }
}

#[derive(Error, Debug)]
pub(crate) enum NanosError {
    #[error("Failed to run shell command")]
    Shell(#[source] ShellError),
    #[error("FileSystem error: {1}")]
    FileSystem(#[source] std::io::Error, &'static str),
    #[error("Ops not found error: {0}")]
    OpsNotFound(#[source] which::Error),
    #[error("Homedir error")]
    HomeDir(#[source] homedir::GetHomeError),
    #[error("Usage error: {0}")]
    UsageError(String),
}

pub(crate) async fn prepare_launch(
    worker_configuration: UnikernelWorkerConfig,
    tap: TapUser,
    args: &Args,
) -> Result<LaunchConfiguration, NanosError> {
    let image_name = worker_configuration.image_name();
    let temp_dir = tempdir::TempDir::new(&image_name)
        .map_err(|e| NanosError::FileSystem(e, "Creating Tempdir"))?;
    let dest_image_path = temp_dir.path().join(".ops/images").join(&image_name);

    async_std::fs::create_dir_all(dest_image_path.parent().unwrap())
        .await
        .map_err(|e| NanosError::FileSystem(e, "creating ops image dir"))?;

    let ip_string = worker_configuration
        .ip
        .as_ref()
        .unwrap_or(tap.ip())
        .to_string();
    let nanos_config_file = temp_dir.path().join("nanos_config.json");

    let mut file = fs::File::create(&nanos_config_file)
        .map_err(|e| NanosError::FileSystem(e, "Creating Config"))?;
    serde_json::to_string(args).unwrap();

    file.write_all(serde_json::to_string(args).unwrap().as_bytes())
        .map_err(|e| NanosError::FileSystem(e, "Writing Config"))?;

    if !worker_configuration.elf_binary.is_file() {
        return Err(NanosError::UsageError(format!(
            "{} is not a file",
            worker_configuration.elf_binary.as_str()
        )));
    }
    let (binary_name, config_file) = if args.use_docker {
        let elf_binary_filename = worker_configuration.elf_binary.file_name().unwrap();
        (
            Utf8PathBuf::from("/input/").join(elf_binary_filename),
            Utf8PathBuf::from("/config/")
                .join(nanos_config_file.file_name().unwrap().to_str().unwrap()),
        )
    } else {
        (
            worker_configuration.elf_binary.clone(),
            Utf8PathBuf::from_path_buf(nanos_config_file).unwrap(),
        )
    };

    let mut ops_args = vec![
        "build",
        binary_name.as_str(),
        "-c",
        config_file.as_str(),
        "--ip-address",
        &ip_string,
        "-i",
        &image_name,
    ];

    if let Some(args) = worker_configuration.args.as_ref() {
        if !args.is_empty() {
            for arg in args.split(' ') {
                ops_args.push("--args");
                ops_args.push(arg);
            }
        }
    }

    if args.use_docker {
        ops_build_using_docker(ops_args, &worker_configuration, &temp_dir).await?
    } else {
        ops_build_using_local(ops_args, &worker_configuration, &dest_image_path).await?
    }

    Ok(LaunchConfiguration {
        tap,
        image_path: dest_image_path,
        temp_dir,
        firmware: vec![],
        num_cores: Some(1),
        memory_in_mega_bytes: Some(512),
    })
}

async fn ops_build_using_local(
    ops_args: Vec<&str>,
    config: &UnikernelWorkerConfig,
    dest_image: &PathBuf,
) -> Result<(), NanosError> {
    run_shell_command("ops", &ops_args)
        .await
        .map_err(NanosError::Shell)?;

    let source_image = homedir::get_my_home()
        .unwrap()
        .unwrap()
        .join(format!(".ops/images/{}.img", config.image_name()));

    async_std::fs::copy(source_image, dest_image)
        .await
        .map_err(|e| NanosError::FileSystem(e, "copying image"))?;

    Ok(())
}

async fn ops_build_using_docker(
    p0: Vec<&str>,
    p1: &UnikernelWorkerConfig,
    p2: &TempDir,
) -> Result<(), NanosError> {
    todo!()
}
