use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use async_std::task;
use tracing::info;
use crate::nes::Source;

use crate::network::{network_cleanup, network_setup};
use crate::templates::WorkerConfiguration;

mod flatcar;
mod network;
mod qemu;
mod shell;
mod templates;
mod nes;

fn main() {
    tracing_subscriber::fmt::init();
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let pair2 = Arc::clone(&pair);
    ctrlc::set_handler(move || {
        let (lock, cvar) = &*pair2;
        let mut stop = lock.lock().unwrap();
        *stop = true;
        cvar.notify_one();
    })
    .expect("Error settings ctrl-c handler");

    let bridges = futures_lite::future::block_on(network_setup());

    let args = flatcar::Args {
        flatcar_fresh_image: PathBuf::from("./flatcar_fresh.iso"),
    };

    let tap = bridges.get_tap();

    let worker_config = WorkerConfiguration {
        ip_addr: tap.ip_addr,
        host_ip_addr: *bridges.host_ip(),
        worker_id: 2,
        parent_id: 1,
        sources: vec![Source::tcp_source(
            "nexmark_bid".to_string(),
            "nexmark_bid_csv".to_string(),
            IpAddr::from([10, 0, 0, 1]),
            8080,
            std::time::Duration::from_millis(100),
        )]
    };

    let task = task::spawn(async {
        let wc = worker_config;
        let flatcar_args = args;
        let t = tap;
        let lc = flatcar::prepare_launch(&wc, &t, &flatcar_args).await;
        qemu::start_qemu(&lc).await
    });

    let (lock, cvar) = &*pair;
    let mut stop = lock.lock().unwrap();
    while !*stop {
        stop = cvar.wait(stop).unwrap();
    }
    info!("Stopping");
    task::block_on(task.cancel());
    task::block_on(network_cleanup(bridges));
}
