use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;

use serde::Serialize;
use tempdir::TempDir;
use tracing::info;

use crate::network::TapUser;
use crate::qemu::{LaunchConfiguration, QemuFirmwareConfig};
use crate::shell::run_shell_command_with_stdin;
use crate::templates::{Templates, WorkerConfiguration};

#[derive(Debug, Serialize)]
struct Content {
    inline: String,
}

#[derive(Debug, Serialize)]
struct FlatcarStorageFileConfig {
    path: PathBuf,
    contents: Content,
}

#[derive(Debug, Serialize)]
struct FlatcarStorageConfig {
    files: Vec<FlatcarStorageFileConfig>,
}

#[derive(Debug, Serialize)]
struct FlatcarSystemdUnitConfig {
    name: String,
    enabled: bool,
    contents: String,
}

#[derive(Debug, Serialize)]
struct FlatcarSystemdConfig {
    units: Vec<FlatcarSystemdUnitConfig>,
}

#[derive(Debug, Serialize)]
struct FlatcarConfig {
    variant: String,
    version: String,
    systemd: FlatcarSystemdConfig,
    storage: FlatcarStorageConfig,
}

async fn run_butane(config: &FlatcarConfig) -> String {
    let data = serde_yaml::to_string(&config).unwrap();
    run_shell_command_with_stdin(
        "docker",
        vec!["run", "-i", "--rm", "quay.io/coreos/butane:latest"],
        data.as_bytes(),
    )
    .await
    .expect("could not run docker")
}

#[derive(Clone)]
pub struct Args {
    pub flatcar_fresh_image: PathBuf,
    pub number_of_cores: Option<usize>,
}

fn create_configuration(wc: &WorkerConfiguration) -> FlatcarConfig {
    FlatcarConfig {
        version: "1.0.0".to_string(),
        variant: "flatcar".to_string(),
        systemd: FlatcarSystemdConfig {
            units: vec![FlatcarSystemdUnitConfig {
                name: "nesWorker.service".to_string(),
                enabled: true,
                contents: Templates::docker_unit(wc),
            }],
        },
        storage: FlatcarStorageConfig {
            files: vec![
                FlatcarStorageFileConfig {
                    path: PathBuf::from("/etc/systemd/network/00-eth0.network"),
                    contents: Content {
                        inline: Templates::network_config(wc),
                    },
                },
                FlatcarStorageFileConfig {
                    path: PathBuf::from("/config/worker_config.yaml"),
                    contents: Content {
                        inline: Templates::worker_config(wc),
                    },
                },
                FlatcarStorageFileConfig {
                    path: PathBuf::from("/etc/docker/daemon.json"),
                    contents: Content {
                        inline: Templates::docker_daemon(wc),
                    },
                },
            ],
        },
    }
}

pub(crate) async fn prepare_launch<'nc>(
    wc: WorkerConfiguration,
    tap: TapUser<'nc>,
    args: &Args,
) -> LaunchConfiguration<'nc> {
    let temp_dir = TempDir::new(&format!("worker_{}", wc.worker_id)).unwrap();
    let image_path = temp_dir.path().join("flatcar_fresh.iso");
    let ignition_path = temp_dir.path().join("ignition.json");
    let flatcar_config = create_configuration(&wc);
    let butane_output = run_butane(dbg!(&flatcar_config));
    info!(src = ?args.flatcar_fresh_image, dest = ?image_path, tmp= ?temp_dir, "Copy image to tmp directory");
    std::fs::copy(&args.flatcar_fresh_image, &image_path).expect("Could not copy flatcar image");
    let butane_output = butane_output.await;

    let mut ignition_file =
        std::fs::File::create(&ignition_path).expect("Could not create ignition.json");
    ignition_file
        .write_all(butane_output.as_ref())
        .expect("Could not populate ignition.json");

    LaunchConfiguration {
        tap,
        image_path,
        firmware: vec![QemuFirmwareConfig {
            name: "opt/org.flatcar-linux/config".to_string(),
            path: temp_dir.path().join("ignition.json"),
        }],
        num_cores: args.number_of_cores,
        memory_in_mega_bytes: None,
        temp_dir,
    }
}

#[test]
fn should_serialize_properly() {
    let worker_config = WorkerConfiguration {
        ip_addr: IpAddr::from([127, 0, 0, 1]),
        host_ip_addr: IpAddr::from([127, 0, 0, 1]),
        parent_id: 0,
        worker_id: 1,
        sources: vec![],
        log_level: "LOG_INFO",
        query_processing: Default::default(),
    };

    let config = FlatcarConfig {
        version: "1.0.0".to_string(),
        variant: "flatcar".to_string(),
        systemd: FlatcarSystemdConfig {
            units: vec![FlatcarSystemdUnitConfig {
                name: "nesWorker.service".to_string(),
                enabled: true,
                contents: Templates::docker_unit(&worker_config),
            }],
        },
        storage: FlatcarStorageConfig {
            files: vec![
                FlatcarStorageFileConfig {
                    path: PathBuf::from("/etc/systemd/network/00-eth0.network"),
                    contents: Content {
                        inline: Templates::network_config(&worker_config),
                    },
                },
                FlatcarStorageFileConfig {
                    path: PathBuf::from("/config/worker_config.yaml"),
                    contents: Content {
                        inline: Templates::worker_config(&worker_config),
                    },
                },
                FlatcarStorageFileConfig {
                    path: PathBuf::from("/etc/docker/daemon.json"),
                    contents: Content {
                        inline: Templates::docker_daemon(&worker_config),
                    },
                },
            ],
        },
    };

    let output = futures_lite::future::block_on(run_butane(&config));
    println!("{output}")
}
