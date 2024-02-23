use async_std::path::PathBuf;
use camino::Utf8PathBuf;

use crate::network::TapUser;
use crate::qemu::{LaunchConfiguration, QemuFirmwareConfig};
use crate::shell;

pub struct Args {}

pub struct UnikernelWorkerConfig {
    pub query_id: usize,
    pub node_id: usize,
    pub elf_binary: Utf8PathBuf,
    pub args: Option<String>,
}

impl UnikernelWorkerConfig {
    fn image_name(&self) -> String {
        format!("unikernel_{}_{}", self.query_id, self.node_id)
    }
}

pub(crate) async fn prepare_launch<'nc>(
    worker_configuration: UnikernelWorkerConfig,
    tap: TapUser<'nc>,
    args: &Args,
) -> LaunchConfiguration<'nc> {
    let image_name = worker_configuration.image_name();
    let temp_dir = tempdir::TempDir::new(&image_name).expect("Could not create tempdir");
    let dest_image_path = temp_dir.path().join(&image_name);
    let ip_string = tap.ip().to_string();
    let mut ops_args = vec![
        "build",
        worker_configuration.elf_binary.as_str(),
        "--ip-address",
        &ip_string,
        "-i",
        &image_name,
    ];

    if let Some(args) = worker_configuration.args.as_ref() {
        ops_args.push("--args");
        ops_args.push(args);
    }

    shell::run_shell_command(
        "ops",
        ops_args
    )
    .await
    .expect("Could not build unikernel using ops");

    let path_to_image = homedir::get_my_home()
        .unwrap()
        .unwrap()
        .join(".ops/images")
        .join(&image_name);
    async_std::fs::copy(&path_to_image, &dest_image_path)
        .await
        .expect("Could not copy image to temp dir");

    LaunchConfiguration {
        tap,
        image_path: dest_image_path,
        temp_dir,
        firmware: vec![],
    }
}
