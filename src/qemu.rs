use crate::network::Tap;
use crate::shell::run_shell_command;
use std::path::PathBuf;

pub struct LaunchConfiguration {
    tap: Tap,
    image_path: PathBuf,
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
            "-serial",
            "stdio",
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
