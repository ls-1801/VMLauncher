use crate::network::Tap;
use crate::shell::run_shell_command;
use std::path::PathBuf;
use tempdir::TempDir;

pub struct LaunchConfiguration {
    pub(crate) tap: Tap,
    pub(crate) image_path: PathBuf,
    pub(crate) temp_dir: TempDir,
    pub(crate) additional_args: Vec<String>,
}

const MACHINE_TYPE: &str = "q35";
const NUMBER_OF_CORES: u16 = 2;
const MEMORY_IN_MB: u32 = 2048;

const QEMU_BINARY: &str = "qemu-system-x86_64";

const DEVICES: [&str; 5] = [
    "pcie-root-port,port=0x10,chassis=1,id=pci.1,bus=pcie.0,multifunction=on,addr=0x3",
    "pcie-root-port,port=0x11,chassis=2,id=pci.2,bus=pcie.0,addr=0x3.0x1",
    "pcie-root-port,port=0x12,chassis=3,id=pci.3,bus=pcie.0,addr=0x3.0x2",
    "virtio-scsi-pci,bus=pci.2,addr=0x0,id=scsi0",
    "scsi-hd,bus=scsi0.0,drive=hd0",
];

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

fn create_qemu_arguments(lc: &LaunchConfiguration) -> Vec<String> {
    DEVICES
        .iter()
        .flat_map(|s| ["-device", s].into_iter())
        .chain([
            "-machine",
            MACHINE_TYPE,
            "-enable-kvm",
            "-machine",
            "accel=kvm",
            "-cpu",
            "host",
            "-display",
            "none",
            "-nographic",
            "-monitor",
            &format!(
                "unix:{},server,nowait",
                lc.temp_dir.path().join("monitor.sock").to_str().unwrap()
            ),
            "-serial",
            &format!(
                "unix:{},server,nowait",
                lc.temp_dir.path().join("console.sock").to_str().unwrap()
            ),
            "-netdev",
            &format!("tap,id=n0,ifname={},script=no,downscript=no", lc.tap.name),
            "-drive",
            &format!(
                "file={},format=raw,if=none,id=hd0",
                lc.image_path.to_str().unwrap()
            ),
            "-m",
            &format!("{MEMORY_IN_MB}m"),
            "-smp",
            &NUMBER_OF_CORES.to_string(),
            "-vga",
            "none",
            "-device",
            &format!(
                "virtio-net,bus=pci.3,addr=0x0,netdev=n0,mac={}",
                lc.tap.mac_addr
            ),
        ])
        .map(|s| s.to_string())
        .chain(lc.additional_args.iter().cloned())
        .collect()
}

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
