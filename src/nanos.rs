use camino::Utf8PathBuf;
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::net::Ipv4Addr;
use thiserror::Error;

use crate::network::TapUser;
use crate::qemu::LaunchConfiguration;
use crate::shell;
use crate::shell::ShellError;

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Args {
    pub(crate) klibs: Vec<String>,
    pub(crate) kernel: String,
    pub(crate) klib_dir: String,
    pub(crate) debugflags: Vec<String>,
}

#[derive(Debug)]
pub struct UnikernelWorkerConfig {
    pub query_id: usize,
    pub node_id: usize,
    pub elf_binary: Utf8PathBuf,
    pub args: Option<String>,
    pub ip: Option<Ipv4Addr>
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
    #[error("Homedir error")]
    HomeDir(#[source] homedir::GetHomeError),
}

#[tracing::instrument]
pub(crate) async fn prepare_launch<'nc>(
    worker_configuration: UnikernelWorkerConfig,
    tap: TapUser<'nc>,
    args: &Args,
) -> Result<LaunchConfiguration<'nc>, NanosError> {
    let image_name = worker_configuration.image_name();
    let temp_dir = tempdir::TempDir::new(&image_name)
        .map_err(|e| NanosError::FileSystem(e, "Creating Tempdir"))?;
    let dest_image_path = temp_dir.path().join(&image_name);
    let ip_string = worker_configuration.ip.as_ref().unwrap_or(tap.ip()).to_string();
    let nanos_config_file = temp_dir.path().join("nanos_config.json");

    let mut file = fs::File::create(&nanos_config_file)
        .map_err(|e| NanosError::FileSystem(e, "Creating Config"))?;
    serde_json::to_string(args).unwrap();

    file.write_all(serde_json::to_string(args).unwrap().as_bytes())
        .map_err(|e| NanosError::FileSystem(e, "Writing Config"))?;

    let mut ops_args = vec![
        "build".to_string(),
        worker_configuration.elf_binary.to_string(),
        "--ip-address".to_string(),
        ip_string,
        "-c".to_string(),
        nanos_config_file.to_str().as_ref().unwrap().to_string(),
        "-i".to_string(),
        image_name.clone(),
    ];

    if let Some(args) = worker_configuration.args.as_ref() {
        for arg in args.split(' ') {
            ops_args.push("--args".to_string());
            ops_args.push(arg.to_string());
        }
    }

    let path_to_image = homedir::get_my_home()
        .map_err(NanosError::HomeDir)?
        .expect("Could not locate Home Directory")
        .join(".ops/images")
        .join(&image_name);

    let _ = fs::remove_file(&path_to_image);

    let refs = ops_args.iter().map(|s| s.as_str()).collect();
    shell::run_shell_command("ops", refs)
        .await
        .map_err(NanosError::Shell)?;

    async_std::fs::copy(&path_to_image, &dest_image_path)
        .await
        .map_err(|e| NanosError::FileSystem(e, "Copy Image"))?;

    Ok(LaunchConfiguration {
        tap,
        image_path: dest_image_path,
        temp_dir,
        firmware: vec![],
        num_cores: Some(1),
        memory_in_mega_bytes: Some(1024),
    })
}
