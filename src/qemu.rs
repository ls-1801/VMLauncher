use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use async_process::Command;
use async_std::io::{ReadExt, WriteExt};
use async_std::os::unix::net::UnixStream;
use async_std::task;
use rand::random;
use strum_macros::Display;
use tempdir::TempDir;
use tracing::{info, instrument};

use crate::network::Tap;
use crate::qemu::MachineType::Q35;
use crate::shell::run_shell_command;

#[derive(Debug)]
pub struct LaunchConfiguration {
    pub(crate) tap: Tap,
    pub(crate) image_path: PathBuf,
    pub(crate) temp_dir: TempDir,
}

const QEMU_BINARY: &str = "qemu-system-x86_64";

// qemu-system-x86_64
//     -name flatcar_production_qemu-3602-2-3
//     -m 1024
// // net
//     -netdev tap,id=eth0,ifname=tap0,script=no,downscript=no
//     -device virtio-net-pci,netdev=eth0,mac=00-60-2F-00-00-00
// // rng
//     -object rng-random,filename=/dev/urandom,id=rng0
//     -device virtio-rng-pci,rng=rng0
//
// //fs
//     -fw_cfg name=opt/org.flatcar-linux/config,file=ignition.json
//     -drive if=virtio,file=/home/ls/dima/flatcar/flatcar_production_qemu_image.img
//     -fsdev local,id=conf,security_model=none,readonly=on,path=/tmp/tmp.hM5SnnwK3x
//     -device virtio-9p-pci,fsdev=conf,mount_tag=config-2
//
//     -machine accel=kvm:tcg
//     -cpu host
//     -smp 8

trait QemuCommandLineArgs {
    fn as_args(&self) -> impl Iterator<Item = String>;
}

struct QemuFirmwareConfig {
    name: String,
    path: PathBuf,
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
    tap: Option<&'tap Tap>,
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
                        info!(interface_name = t.name, mac = %t.mac_addr, "Attaching Tap Device");
                        [
                            "-netdev".to_string(),
                            format!("tap,id=eth0,ifname={},script=no,downscript=no", t.name),
                            "-device".to_string(),
                            format!("virtio-net-pci,netdev=eth0,mac={}", t.mac_addr),
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
        memory_in_megabytes: Some(2048),
        number_of_cores: Some(2),
        rng_device: true,
        tap: Some(&lc.tap),
        firmware: vec![QemuFirmwareConfig {
            name: "opt/org.flatcar-linux/config".to_string(),
            path: lc.temp_dir.path().join("ignition.json"),
        }],
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

#[instrument]
pub async fn stop_qemu(lc: LaunchConfiguration) {
    let mut buf = vec![0; 64];
    let len = async_std::fs::File::open(lc.temp_dir.path().join("pidfile"))
        .await
        .unwrap()
        .read(&mut buf)
        .await
        .unwrap();
    assert!(len > 0);
    let pid = std::str::from_utf8(&buf[0..len - 1]).unwrap();
    info!(pid, "Fetching Pid");
    let pid = pid.parse::<usize>().unwrap();

    let monitor = lc.temp_dir.path().join("monitor.socket");
    let mut monitor_socket = UnixStream::connect(monitor)
        .await
        .expect("Could not open monitor socket");
    monitor_socket
        .write_all("q\n".as_bytes())
        .await
        .expect("Could not write to socket");

    let start_time = Instant::now();
    let timeout_duration = Duration::from_secs(2);
    loop {
        let pid_exists = Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|b| b.success())
            .unwrap_or(false);

        if !pid_exists {
            return;
        }

        let elapsed = start_time.elapsed();
        if elapsed >= timeout_duration {
            break;
        }

        let _ = task::sleep(Duration::from_millis(200)).await;
    }

    let status = Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .await
        .unwrap();

    assert!(status.success());
}

#[instrument]
pub async fn start_qemu(lc: &LaunchConfiguration) {
    run_shell_command(
        QEMU_BINARY,
        create_qemu_arguments(lc)
            .iter()
            .map(|s| s.as_ref())
            .collect(),
    )
    .await
    .expect("Could not run qemu");
}
