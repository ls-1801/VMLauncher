use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use crate::network::{network_cleanup, network_setup};
use crate::templates::WorkerConfiguration;

mod flatcar;
mod network;
mod qemu;
mod shell;
mod templates;

fn main() {
    tracing_subscriber::fmt::init();
    let bridges = futures_lite::future::block_on(network_setup());
    let tap = futures_lite::future::block_on(network::Tap::create(
        "tap0".to_string(),
        IpAddr::from([10, 0, 0, 2]),
    ));

    let args = flatcar::Args {
        flatcar_fresh_image: PathBuf::from("./flatcar_fresh.iso"),
    };

    let worker_config = WorkerConfiguration {
        ip_addr: Ipv4Addr::from([10, 0, 0, 2]),
        host_ip_addr: Ipv4Addr::from([10, 0, 0, 2]),
        worker_id: 2,
        parent_id: 1,
    };

    let lc = futures_lite::future::block_on(flatcar::prepare_launch(&worker_config, tap, &args));

    futures_lite::future::block_on(qemu::start_qemu(&lc));

    futures_lite::future::block_on(network_cleanup(bridges));
}
