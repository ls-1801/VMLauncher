use async_std::io::{ReadExt, WriteExt};
use async_std::os::unix::net::UnixStream;
use async_std::{io, task};
use rand::random;
use std::fmt::{Display, Formatter};
use std::fs::Permissions;
use std::future::Future;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::ExitStatus;
use std::str::from_utf8;
use std::time::Duration;
use strum_macros::Display;
use tempdir::TempDir;
use thiserror::Error;
use tracing::{info, instrument};

use crate::network::TapUser;
use crate::qemu::MachineType::Q35;
use crate::shell::{self, ShellError};
use crate::shell::{run_command_without_output, run_shell_command};

#[derive(Debug)]
pub struct LaunchConfiguration {
    pub(crate) tap: TapUser,
    pub(crate) image_path: PathBuf,
    pub(crate) temp_dir: TempDir,
    pub(crate) firmware: Vec<QemuFirmwareConfig>,
    pub(crate) num_cores: Option<usize>,
    pub(crate) memory_in_mega_bytes: Option<usize>,
}

const QEMU_BINARY: &str = "qemu-system-x86_64";

trait QemuCommandLineArgs {
    fn as_args(&self) -> impl Iterator<Item = String>;
}

#[derive(Debug, Clone)]
pub struct QemuFirmwareConfig {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

impl QemuCommandLineArgs for QemuFirmwareConfig {
    fn as_args(&self) -> impl Iterator<Item = String> {
        [
            "-fw_cfg".to_string(),
            format!("name={},file={}", self.name, self.path.to_str().unwrap()),
        ]
        .into_iter()
    }
}

struct MountedFilesystem {
    mount_tag: String,
    readonly: bool,
    path: PathBuf,
}

impl QemuCommandLineArgs for MountedFilesystem {
    fn as_args(&self) -> impl Iterator<Item = String> {
        let id: usize = random();
        [
            "-fsdev".to_string(),
            format!(
                "local,id=f{id},security_model=none,readonly={},path={}",
                if self.readonly { "on" } else { "off" },
                self.path.to_str().unwrap()
            ),
            "-device".to_string(),
            format!("virtio-9p-pci,fsdev=f{id},mount_tag={}", self.mount_tag),
        ]
        .into_iter()
    }
}

struct QemuConfig<'tap> {
    name: Option<String>,
    memory_in_megabytes: Option<usize>,
    number_of_cores: Option<usize>,
    rng_device: bool,
    tap: Option<&'tap TapUser>,
    firmware: Vec<QemuFirmwareConfig>,
    virtio_drives: Vec<PathBuf>,
    mounted_filesystems: Vec<MountedFilesystem>,
}

fn bool_option(b: bool) -> Option<()> {
    if b {
        return Some(());
    } else {
        None
    }
}

impl QemuCommandLineArgs for QemuConfig<'_> {
    fn as_args(&self) -> impl Iterator<Item = String> {
        self.virtio_drives
            .iter()
            .flat_map(|f| {
                [
                    "-drive".to_string(),
                    format!("if=virtio,file={}", f.to_str().unwrap()),
                ]
                .into_iter()
            })
            .chain(self.mounted_filesystems.iter().flat_map(|f| f.as_args()))
            .chain(self.firmware.iter().flat_map(|f| f.as_args()))
            .chain(bool_option(self.rng_device).into_iter().flat_map(|_| {
                [
                    "-object",
                    "rng-random,filename=/dev/urandom,id=rng0",
                    "-device",
                    "virtio-rng-pci,rng=rng0",
                ]
                .into_iter()
                .map(|s| s.to_string())
            }))
            .chain(
                self.name
                    .iter()
                    .map(|n| ["-name".to_string(), n.clone()])
                    .flat_map(|a| a.into_iter()),
            )
            .chain(
                self.memory_in_megabytes
                    .iter()
                    .map(|m| ["-m".to_string(), format!("{m}m")])
                    .flat_map(|a| a.into_iter()),
            )
            .chain(
                self.number_of_cores
                    .iter()
                    .map(|c| ["-smp".to_string(), format!("{c}")])
                    .flat_map(|a| a.into_iter()),
            )
            .chain(
                self.tap
                    .iter()
                    .map(|t| {
                        info!(interface_name = t.device(), mac = %t.mac(), "Attaching Tap Device");
                        [
                            "-netdev".to_string(),
                            format!("tap,id=eth0,ifname={},script=no,downscript=no", t.device()),
                            "-device".to_string(),
                            format!("virtio-net-pci,netdev=eth0,mac={}", t.mac()),
                        ]
                    })
                    .flat_map(|a| a.into_iter()),
            )
    }
}

struct QemuRunMode {
    monitor: Option<QemuMonitor>,
    serial: Option<QemuSerial>,
    display: bool,
    daemonize_pidfile: Option<PathBuf>,
}

impl QemuCommandLineArgs for QemuRunMode {
    fn as_args(&self) -> impl Iterator<Item = String> {
        self.monitor
            .iter()
            .map(|m| m.as_args())
            .flat_map(|s| s.into_iter())
            .chain(
                self.serial
                    .iter()
                    .map(|m| m.as_args())
                    .flat_map(|s| s.into_iter()),
            )
            .chain(
                self.daemonize_pidfile
                    .iter()
                    .map(|pid_file| ["-daemonize", "-pidfile", pid_file.to_str().unwrap()])
                    .flat_map(|s| s.into_iter().map(|s| s.to_string())),
            )
            .chain(
                bool_option(!self.display)
                    .into_iter()
                    .map(|_| ["-display", "none", "-vga", "none"])
                    .flat_map(|s| s.into_iter().map(|s| s.to_string())),
            )
    }
}

#[derive(Display)]
enum MachineType {
    #[strum(to_string = "q35")]
    Q35,
}

struct QemuVirtualizationMode {
    machine: Option<MachineType>,
    cpu: Option<String>,
    accel: Option<String>,
}

impl QemuCommandLineArgs for QemuVirtualizationMode {
    fn as_args(&self) -> impl Iterator<Item = String> {
        let mut options = vec![];
        if let Some(machine_type) = self.machine.as_ref() {
            options.push("-machine".to_string());
            options.push(machine_type.to_string());
        }

        if let Some(machine_type) = self.cpu.as_ref() {
            options.push("-cpu".to_string());
            options.push(machine_type.to_string());
        } else {
            options.push("-cpu".to_string());
            options.push("host".to_string());
        }

        if let Some(accel) = self.accel.as_ref() {
            assert_eq!(accel, "kvm");
            options.push("-enable-kvm".to_string());
            options.push("-machine".to_string());
            options.push("accel=kvm".to_string());
        }

        options.into_iter()
    }
}

struct QemuMonitor {
    monitor_socket_path: PathBuf,
}

impl QemuCommandLineArgs for QemuMonitor {
    fn as_args(&self) -> impl Iterator<Item = String> {
        [
            "-monitor".to_string(),
            format!(
                "unix:{},server,nowait",
                self.monitor_socket_path.to_str().unwrap()
            ),
        ]
        .into_iter()
    }
}

struct QemuSerial {
    serial_socket_path: PathBuf,
}

impl QemuCommandLineArgs for QemuSerial {
    fn as_args(&self) -> impl Iterator<Item = String> {
        [
            "-serial".to_string(),
            format!(
                "unix:{},server,nowait",
                self.serial_socket_path.to_str().unwrap()
            ),
        ]
        .into_iter()
    }
}

fn create_qemu_arguments(lc: &LaunchConfiguration) -> Vec<String> {
    let qr = QemuRunMode {
        monitor: Some(QemuMonitor {
            monitor_socket_path: lc.temp_dir.path().join("monitor.socket"),
        }),
        serial: Some(QemuSerial {
            serial_socket_path: lc.temp_dir.path().join("serial.socket"),
        }),
        display: false,
        daemonize_pidfile: Some(lc.temp_dir.path().join("pidfile")),
    };

    let qv = QemuVirtualizationMode {
        machine: Some(Q35),
        cpu: None,
        accel: Some("kvm".to_string()),
    };

    let qc = QemuConfig {
        name: None,
        memory_in_megabytes: Some(lc.memory_in_mega_bytes.unwrap_or(16000)),
        number_of_cores: Some(lc.num_cores.unwrap_or(8)),
        rng_device: true,
        tap: Some(&lc.tap),
        firmware: lc.firmware.clone(),
        virtio_drives: vec![lc.image_path.clone()],
        mounted_filesystems: vec![MountedFilesystem {
            mount_tag: "config-2".to_string(),
            readonly: true,
            path: lc.temp_dir.path().to_owned(),
        }],
    };

    qr.as_args()
        .chain(qv.as_args())
        .chain(qc.as_args())
        .collect()
}

#[derive(Debug)]
pub struct QemuProcessHandle {
    lc: Option<LaunchConfiguration>,
}

struct PidNoLongerExists {
    pid: usize,
    ps_process_future: Option<Pin<Box<dyn Future<Output = io::Result<ExitStatus>>>>>,
}

impl QemuProcessHandle {
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
            Ok(r) => r.map_err(QemuError::Shell),
            Err(_) => {
                let killed =
                    shell::run_command_without_output("kill", vec!["-9", &pid.to_string()])
                        .await
                        .map_err(QemuError::Shell)?;
                if killed {
                    Ok(())
                } else {
                    Err(QemuError::CouldNotKill("kill failed"))
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
                .map_err(|e| QemuError::IO(e, "reading pidfile"))?,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(QemuError::NotRunning());
            }
            Err(e) => {
                return Err(QemuError::IO(e, "opening pidfile"));
            }
        };

        assert!(read_len > 0);
        let pid_slice = from_utf8(&buf[0..read_len - 1]).map_err(QemuError::PidFileNonUtf)?;
        pid_slice
            .parse::<usize>()
            .map_err(QemuError::PidFileNonNumeric)
    }
    // Test if the pid file exists
    async fn is_running(&self) -> Result<bool> {
        match self.get_pid().await {
            Ok(pid) => run_command_without_output("ps", vec!["-p", &pid.to_string()])
                .await
                .map_err(QemuError::Shell),
            Err(QemuError::NotRunning()) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

impl Display for QemuProcessHandle {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!(
            "TapDevice: {}, Ip: {}",
            self.lc.as_ref().unwrap().tap.device(),
            self.lc.as_ref().unwrap().tap.ip()
        ))
    }
}

impl Drop for QemuProcessHandle {
    fn drop(&mut self) {
        if self.lc.is_some() {
            task::block_on(self.stop());
        }
    }
}

pub async fn serial_with_command(
    command: &str,
    serial_socket: PathBuf,
    node_id: usize,
) -> core::result::Result<(), SerialError> {
    let connection = io::timeout(Duration::from_secs(1), UnixStream::connect(serial_socket)).await;
    let mut connection = connection.map_err(SerialError::Connecting)?;

    connection
        .write_all(command.as_bytes())
        .await
        .map_err(SerialError::Writing)?;

    serial_listen(connection, node_id).await
}

async fn serial_listen(
    mut connection: UnixStream,
    node_id: usize,
) -> core::result::Result<(), SerialError> {
    let mut buf = vec![0u8; 4096];
    let mut current_index = 0;
    loop {
        let result = io::timeout(
            Duration::from_secs(1),
            connection.read(&mut buf[current_index..]),
        )
        .await;

        let result = match result {
            Err(e) => {
                if e.kind() == io::ErrorKind::TimedOut {
                    continue;
                } else {
                    return Err(SerialError::Reading(e));
                }
            }
            Ok(r) => r,
        };

        (buf, current_index) = chunk_to_lines(buf, current_index + result, |line| {
            println!("[{}] {}", node_id, line);
        })?;
    }
}

pub async fn serial(
    serial_socket: PathBuf,
    node_id: usize,
) -> core::result::Result<(), SerialError> {
    let connection = io::timeout(Duration::from_secs(1), UnixStream::connect(serial_socket)).await;
    let mut connection = connection.map_err(SerialError::Connecting)?;
    serial_listen(connection, node_id).await
}

fn chunk_to_lines(
    mut buf: Vec<u8>,
    bytes_used: usize,
    f: impl Fn(&str),
) -> core::result::Result<(Vec<u8>, usize), SerialError> {
    let mut current_index = bytes_used;
    let output = from_utf8(&buf[0..bytes_used]).map_err(SerialError::UTF8)?;
    match output.rfind('\n') {
        None => {}
        Some(size) => {
            for x in output[..size].lines() {
                f(x)
            }
            current_index = bytes_used - (size + 1);
            buf.drain(0..size + 1);
            buf.resize(4096, 0);
        }
    }

    Ok((buf, current_index))
}

#[test]
fn test_chunk_to_lines() {
    let mut vec = vec![0u8; 4096];
    let message = "Hello".as_bytes();
    let messageWithNewline = "Hello\nHellow".as_bytes();
    vec[..message.len()].clone_from_slice(message);

    let (mut vec, current_index) = chunk_to_lines(vec, message.len(), |f| {
        assert!(false, "No line was ever finished");
    })
    .unwrap();

    assert_eq!(vec.len(), 4096);
    assert_eq!(current_index, message.len());

    vec[current_index..(current_index + message.len())].clone_from_slice(message);

    let (mut vec, current_index) = chunk_to_lines(vec, current_index + message.len(), |f| {
        assert!(false, "No line was ever finished");
    })
    .unwrap();

    assert_eq!(vec.len(), 4096);
    assert_eq!(current_index, message.len() * 2);

    vec[current_index..(current_index + messageWithNewline.len())]
        .clone_from_slice(messageWithNewline);

    let (mut vec, current_index) =
        chunk_to_lines(vec, current_index + messageWithNewline.len(), |f| {
            assert_eq!(f, "HelloHelloHello");
        })
        .unwrap();

    assert_eq!(vec.len(), 4096);
    assert_eq!(current_index, "Hellow".len());

    vec[current_index..(current_index + message.len())].clone_from_slice(message);

    let (mut vec, current_index) = chunk_to_lines(vec, current_index + message.len(), |f| {
        assert!(false, "No line was ever finished");
    })
    .unwrap();

    assert_eq!(vec.len(), 4096);
    assert_eq!(current_index, "Hellow".len() + message.len());

    vec[current_index..(current_index + messageWithNewline.len())]
        .clone_from_slice(messageWithNewline);

    let (mut vec, current_index) =
        chunk_to_lines(vec, current_index + messageWithNewline.len(), |f| {
            assert_eq!(f, "HellowHelloHello");
        })
        .unwrap();

    assert_eq!(vec.len(), 4096);
    assert_eq!(current_index, "Hellow".len());
}

type Result<T> = core::result::Result<T, QemuError>;

#[derive(Error, Debug)]
pub enum QemuError {
    #[error("When spawning qemu command")]
    Shell(#[source] shell::ShellError),
    #[error("Qemu process is not running (pid file does not exist)")]
    NotRunning(),
    #[error("IO Error when: {1}")]
    IO(#[source] async_std::io::Error, &'static str),
    #[error("Pidfile does not containe vaild UTF-8")]
    PidFileNonUtf(#[source] std::str::Utf8Error),
    #[error("Pidfile does not contain a valid pid")]
    PidFileNonNumeric(#[source] std::num::ParseIntError),
    #[error("Could not kill qemu process")]
    CouldNotKill(&'static str),
}

#[derive(Error, Debug)]
pub enum SerialError {
    #[error("While connecting")]
    Connecting(#[source] std::io::Error),
    #[error("While writing")]
    Writing(#[source] std::io::Error),
    #[error("While reading")]
    Reading(#[source] std::io::Error),
    #[error("While reading utf8")]
    UTF8(#[source] std::str::Utf8Error),
}

#[instrument]
pub async fn start_qemu(lc: LaunchConfiguration) -> Result<QemuProcessHandle> {
    run_shell_command(
        QEMU_BINARY,
        &create_qemu_arguments(&lc)
            .iter()
            .map(|s| s.as_ref())
            .collect(),
    )
    .await
    .map_err(|e| QemuError::Shell(e))?;

    let qh = QemuProcessHandle { lc: Some(lc) };
    async_std::fs::set_permissions(qh.serial_path(), Permissions::from_mode(0o666))
        .await
        .map_err(|e| QemuError::IO(e, "Changing permission of Serial"))?;
    async_std::fs::set_permissions(qh.monitor_path(), Permissions::from_mode(0o666))
        .await
        .map_err(|e| QemuError::IO(e, "Changing permission of Monitor"))?;

    Ok(qh)
}
