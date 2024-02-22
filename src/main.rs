use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use crate::nes::Source;
use async_std::task;
use tracing::info;

use crate::network::{network_cleanup, network_setup};
use crate::templates::WorkerConfiguration;

mod flatcar;
mod nes;
mod network;
mod qemu;
mod shell;
mod templates;

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

    let mut bridges = futures_lite::future::block_on(network_setup());

    let vms = (2..10)
        .into_iter()
        .map(|worker_id| {
            let tap = bridges.get_tap();
            let worker_config = WorkerConfiguration {
                ip_addr: tap.ip_addr,
                host_ip_addr: *bridges.host_ip(),
                worker_id,
                parent_id: worker_id - 1,
                sources: vec![Source::tcp_source(
                    "nexmark_bid".to_string(),
                    format!("nexmark_bid_{}", worker_id),
                    IpAddr::from([10, 0, 0, 1]),
                    8080,
                    std::time::Duration::from_millis(100),
                )],
            };

            task::block_on(async move {
                let wc = worker_config;
                let args = flatcar::Args {
                    flatcar_fresh_image: PathBuf::from("./flatcar_fresh.iso"),
                };
                let lc = flatcar::prepare_launch(wc, tap, &args).await;
                qemu::start_qemu(&lc).await;
                lc
            })
        })
        .collect::<Vec<_>>();

    let (lock, cvar) = &*pair;
    let mut stop = lock.lock().unwrap();
    while !*stop {
        stop = cvar.wait(stop).unwrap();
    }
    info!("Stopping");
    vms.into_iter().for_each(|lc| {
        task::block_on(qemu::stop_qemu(lc));
    });
    task::block_on(network_cleanup(bridges));
}
