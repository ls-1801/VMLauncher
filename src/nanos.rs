use camino::Utf8PathBuf;
use thiserror::Error;

use crate::network::TapUser;
use crate::qemu::LaunchConfiguration;
use crate::shell;
use crate::shell::ShellError;

#[derive(Debug)]
pub struct Args {}

#[derive(Debug)]
pub struct UnikernelWorkerConfig {
    pub query_id: usize,
    pub node_id: usize,
    pub elf_binary: Utf8PathBuf,
    pub args: Option<String>,
}

impl UnikernelWorkerConfig {
    fn image_name(&self) -> String {
        format!("unikernel_{}_{}.img", self.query_id, self.node_id)
    }
}

#[derive(Error, Debug)]
pub(crate) enum NanosError {
    #[error("Failed to run shell command")]
    Shell(#[source] ShellError),
    #[error("FileSystem error")]
    FileSystem(#[source] std::io::Error),
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
    let temp_dir = tempdir::TempDir::new(&image_name).map_err(|e| NanosError::FileSystem(e))?;
    let dest_image_path = temp_dir.path().join(&image_name);
    let ip_string = tap.ip().to_string();
    let mut ops_args = vec![
        "build".to_string(),
        worker_configuration.elf_binary.to_string(),
        "--ip-address".to_string(),
        ip_string,
        "-p".to_string(),
        "8080".to_string(),
        "-i".to_string(),
        image_name.clone(),
    ];

    if let Some(args) = worker_configuration.args.as_ref() {
        for arg in args.split(' ') {
            ops_args.push("--args".to_string());
            ops_args.push(arg.to_string());
        }
    }

    let refs = ops_args.iter().map(|s| s.as_str()).collect();
    shell::run_shell_command("ops", refs)
        .await
        .map_err(NanosError::Shell)?;

    let path_to_image = homedir::get_my_home()
        .map_err(NanosError::HomeDir)?
        .expect("Could not locate Home Directory")
        .join(".ops/images")
        .join(&image_name);
    async_std::fs::copy(&path_to_image, &dest_image_path)
        .await
        .map_err(NanosError::FileSystem)?;

    Ok(LaunchConfiguration {
        tap,
        image_path: dest_image_path,
        temp_dir,
        firmware: vec![],
    })
}
