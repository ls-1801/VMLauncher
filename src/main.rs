use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use crate::nes::Source;
use async_std::task;
use camino::{Utf8Path, Utf8PathBuf};
use clap::Parser;
use inquire::{CustomType, InquireError};
use ipnet::Ipv4Net;
use thiserror::Error;
use tracing::{error, info};

use crate::network::{network_cleanup, network_setup, NetworkConfig};
use crate::qemu::{start_qemu, QemuError, QemuProcessHandle};
use crate::templates::WorkerConfiguration;

mod flatcar;
mod nanos;
mod nes;
mod network;
mod qemu;
mod shell;
mod templates;

#[derive(Parser)]
struct Args {
    #[arg(short = 'n')]
    ip_range: Option<Ipv4Net>,
}

#[derive(Error, Debug)]
enum Error {
    #[error("Command Prompt Error")]
    Inquire(#[source] InquireError),
    #[error("Qemu Error")]
    Nanos(#[source] nanos::NanosError),
    #[error("Qemu Error")]
    Qemu(#[source] QemuError),
}

fn add_unikernel(nc: &NetworkConfig) -> Result<QemuProcessHandle, Error> {
    let node_id = inquire::CustomType::<usize>::new("NodeId?")
        .prompt()
        .map_err(Error::Inquire)?;
    let query_id = inquire::CustomType::<usize>::new("WorkerId?")
        .prompt()
        .map_err(Error::Inquire)?;
    let path_to_binary = inquire::CustomType::<Utf8PathBuf>::new("Binary?")
        .prompt()
        .map_err(Error::Inquire)?;

    let args = inquire::Text::new("args")
        .prompt_skippable()
        .map_err(Error::Inquire)?;

    let wc = nanos::UnikernelWorkerConfig {
        node_id,
        query_id,
        elf_binary: path_to_binary,
        args,
    };

    task::block_on(async move {
        let tap = nc.get_tap().await;
        let lc = nanos::prepare_launch(wc, tap, &nanos::Args {})
            .await
            .map_err(Error::Nanos)?;
        start_qemu(lc).await.map_err(Error::Qemu)
    })
}

fn add_worker(nc: &NetworkConfig) -> Result<QemuProcessHandle, Error> {
    let worker_id = inquire::CustomType::<usize>::new("WorkerId?")
        .prompt()
        .map_err(Error::Inquire)?;

    let tap = task::block_on(nc.get_tap());
    let worker_config = WorkerConfiguration {
        host_ip_addr: IpAddr::from(nc.host_ip()),
        ip_addr: IpAddr::from(*tap.ip()),
        worker_id,
        parent_id: worker_id - 1,
        sources: vec![Source::tcp_source(
            "nexmark_bid".to_string(),
            format!("nexmark_bid_{}", worker_id),
            IpAddr::from([10, 0, 0, 0]),
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
        qemu::start_qemu(lc).await.map_err(Error::Qemu)
    })
}

fn main() {
    let args = Args::parse();
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

    let gateway_ip = args
        .ip_range
        .or_else(|| {
            CustomType::<Ipv4Net>::new("Ip Address Space")
                .with_default("10.0.0.0/24".parse::<Ipv4Net>().unwrap())
                .with_error_message("Please type a valid Ipv4 cidr notation")
                .prompt()
                .ok()
        })
        .unwrap();

    let bridges = futures_lite::future::block_on(network_setup(gateway_ip));
    {
        let mut qemu_instances = vec![];
        loop {
            let actions = vec!["stop", "add worker", "ps", "uk"];
            match inquire::Select::new("", actions).prompt() {
                Err(inquire::InquireError::OperationCanceled) => continue,
                Err(inquire::InquireError::OperationInterrupted) => break,
                Err(e) => {
                    error!(e = ?e, "Inquire Error");
                    break;
                }
                Ok(action) => match action {
                    "uk" => match add_unikernel(&bridges) {
                        Ok(qh) => qemu_instances.push(qh),
                        Err(e) => {
                            error!(?e, "Could not create worker");
                            break;
                        }
                    },
                    "ps" => {
                        for qh in &qemu_instances {
                            println!("{qh}")
                        }
                    }
                    "stop" => {
                        break;
                    }
                    "add worker" => match add_worker(&bridges) {
                        Ok(qh) => qemu_instances.push(qh),
                        Err(e) => {
                            error!(?e, "Could not create worker");
                            break;
                        }
                    },
                    _ => unreachable!(),
                },
            }
        }
        info!("Stopping");
    }

    task::block_on(network_cleanup(bridges));
}
