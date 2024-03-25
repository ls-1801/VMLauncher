use async_std::io::WriteExt;
use async_std::os::unix::net::UnixStream;
use std::fmt::{Display, Formatter};
use std::io::ErrorKind;
use std::num::ParseIntError;
use std::path::PathBuf;
use std::str::{from_utf8, Utf8Error};
use std::time::Duration;

use async_std::task;
use futures_lite::{AsyncReadExt, AsyncWriteExt};
use serde::Serialize;
use tempdir::TempDir;
use tracing::instrument;

use crate::network::TapUser;
use crate::shell;
use crate::shell::{run_command_without_output, run_shell_command, ShellError};

const FIRECRACKER_BINARY: &'static str = "/home/lukas-ldap/.local/bin/firecracker";
#[derive(Serialize, Debug)]
struct BootSource {
    kernel_image_path: String,
    boot_args: String,
}

#[derive(Serialize, Debug)]
struct NetworkInterface {
    iface_id: String,
    guest_mac: String,
    host_dev_name: String,
}

#[derive(Serialize, Debug)]
struct Drive {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Serialize, Debug)]
struct MachineConfig {
    vcpu_count: usize,
    mem_size_mib: usize,
    ht_enabled: bool,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "kebab-case")]
struct VMConfig {
    boot_source: BootSource,
    drives: Vec<Drive>,
    network_interfaces: Vec<NetworkInterface>,
    machine_config: MachineConfig,
}

impl VMConfig {
    fn new(lc: &LaunchConfiguration) -> Self {
        VMConfig {
            boot_source: BootSource {
                kernel_image_path: lc.image_path.to_str().unwrap().to_string(),
                boot_args: "".to_string(),
            },
            drives: vec![],
            network_interfaces: vec![NetworkInterface {
                iface_id: "en1".to_string(),
                guest_mac: lc.tap.mac().to_string(),
                host_dev_name: lc.tap.device().to_string(),
            }],
            machine_config: MachineConfig {
                vcpu_count: lc.num_cores.unwrap(),
                mem_size_mib: lc.memory_in_mega_bytes.unwrap(),
                ht_enabled: true,
            },
        }
    }
}

#[derive(Debug)]
pub struct LaunchConfiguration<'nc> {
    pub(crate) tap: TapUser<'nc>,
    pub(crate) image_path: PathBuf,
    pub(crate) temp_dir: TempDir,
    pub(crate) num_cores: Option<usize>,
    pub(crate) memory_in_mega_bytes: Option<usize>,
}

#[derive(Debug)]
pub struct FirecrackerProcessHandle<'nc> {
    lc: Option<LaunchConfiguration<'nc>>,
}

#[derive(thiserror::Error, Debug)]
pub enum FirecrackerError {
    #[error("While executing shell command")]
    Shell(#[source] ShellError),
    #[error("Could not stop VM")]
    CouldNotKill(&'static str),
    #[error("While performing IO: {1}")]
    IO(#[source] std::io::Error, &'static str),
    #[error("VM is not running")]
    NotRunning(),
    #[error("Could not locate pidfile")]
    PidFileNonUtf(#[source] Utf8Error),
    #[error("Pidfile contains garbage")]
    PidFileNonNumeric(#[source] ParseIntError),
}

type Result<T> = core::result::Result<T, FirecrackerError>;
impl<'nc> FirecrackerProcessHandle<'nc> {
    fn monitor_path(&self) -> PathBuf {
        self.lc
            .as_ref()
            .expect("invalid state")
            .temp_dir
            .path()
            .join("monitor.socket")
    }
    pub fn serial_path(&self) -> PathBuf {
        self.lc
            .as_ref()
            .expect("invalid state")
            .temp_dir
            .path()
            .join("serial.socket")
    }
    fn pid_file_path(&self) -> PathBuf {
        self.lc
            .as_ref()
            .expect("invalid state")
            .temp_dir
            .path()
            .join("pidfile")
    }

    #[instrument]
    pub(crate) async fn restart(&mut self) -> Result<()> {
        self.lc = start_qemu(self.lc.take().unwrap()).await?.lc.take();
        Ok(())
    }
    #[instrument]
    pub(crate) async fn stop(&self) -> Result<()> {
        if !self.is_running().await? {
            return Ok(());
        }

        let pid = self.get_pid().await?;
        let mut monitor_socket = UnixStream::connect(self.monitor_path())
            .await
            .expect("Could not open monitor socket");
        monitor_socket
            .write_all("q\n".as_bytes())
            .await
            .expect("Could not write to socket");

        let wait_until_pid_stops_existing = async {
            while run_command_without_output("ps", vec!["-p", &pid.to_string()]).await? {}
            Ok(())
        };

        match async_std::future::timeout(Duration::from_secs(2), wait_until_pid_stops_existing)
            .await
        {
            Ok(r) => r.map_err(FirecrackerError::Shell),
            Err(_) => {
                let killed =
                    shell::run_command_without_output("kill", vec!["-9", &pid.to_string()])
                        .await
                        .map_err(FirecrackerError::Shell)?;
                if killed {
                    Ok(())
                } else {
                    Err(FirecrackerError::CouldNotKill("kill failed"))
                }
            }
        }
    }
    async fn get_pid(&self) -> Result<usize> {
        let pid_file_path = self
            .lc
            .as_ref()
            .expect("qemu handle in invalid state")
            .temp_dir
            .path()
            .join("pidfile");

        let mut buf = vec![0; 64];
        let read_len = match async_std::fs::File::open(pid_file_path).await {
            Ok(mut f) => f
                .read(&mut buf)
                .await
                .map_err(|e| FirecrackerError::IO(e, "reading pidfile"))?,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(FirecrackerError::NotRunning());
            }
            Err(e) => {
                return Err(FirecrackerError::IO(e, "opening pidfile"));
            }
        };

        assert!(read_len > 0);
        let pid_slice =
            from_utf8(&buf[0..read_len - 1]).map_err(FirecrackerError::PidFileNonUtf)?;
        pid_slice
            .parse::<usize>()
            .map_err(FirecrackerError::PidFileNonNumeric)
    }
    // Test if the pid file exists
    async fn is_running(&self) -> Result<bool> {
        match self.get_pid().await {
            Ok(pid) => run_command_without_output("ps", vec!["-p", &pid.to_string()])
                .await
                .map_err(FirecrackerError::Shell),
            Err(FirecrackerError::NotRunning()) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

impl<'nc> Display for FirecrackerProcessHandle<'nc> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "TapDevice: {}, Ip: {}",
            self.lc.as_ref().unwrap().tap.device(),
            self.lc.as_ref().unwrap().tap.ip()
        ))
    }
}

impl<'nc> Drop for FirecrackerProcessHandle<'nc> {
    fn drop(&mut self) {
        if self.lc.is_some() {
            task::block_on(self.stop());
        }
    }
}

#[instrument]
pub async fn start_qemu<'nc>(
    lc: LaunchConfiguration<'nc>,
) -> Result<FirecrackerProcessHandle<'nc>> {
    let config = VMConfig::new(&lc);
    let fc_config_file = lc.temp_dir.path().join("firecracker-config.json");
    let fc_config_string = serde_json::to_string(&fc_config_file).unwrap();
    async_std::fs::File::create(fc_config_file)
        .await
        .map_err(|e| FirecrackerError::IO(e, "Creating Config file"))?
        .write_all(fc_config_string)
        .await
        .map_err(|e| FirecrackerError::IO(e, "Writing Config file"));

    run_shell_command(FIRECRACKER_BINARY)
        .await
        .map_err(|e| FirecrackerError::Shell(e))?;

    Ok(FirecrackerProcessHandle { lc: Some(lc) })
}
