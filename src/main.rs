use crate::network::{network_cleanup, network_setup};
use std::thread::sleep;
use std::time::Duration;

mod flatcar;
mod network;
mod qemu;
mod shell;
mod templates;

fn main() {
    tracing_subscriber::fmt::init();
    let bridges = futures_lite::future::block_on(network_setup());
    sleep(Duration::from_secs(5));
    futures_lite::future::block_on(network_cleanup(bridges));
}
