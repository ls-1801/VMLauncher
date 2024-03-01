use async_std::future::timeout;
use std::fmt::{Display, Formatter};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::nes::{
    Format, Source, TCPSourceConfig, TCPSourceConfigBuilder,
    WorkerQueryProcessingConfigurationBuilder,
};
use async_std::task;
use camino::Utf8PathBuf;
use clap::Parser;
use inquire::{CustomType, InquireError};
use ipnet::Ipv4Net;
use itertools::Itertools;
use thiserror::Error;
use tracing::{error, info};

use crate::network::{network_cleanup, network_setup, NetworkConfig};
use crate::qemu::{serial, start_qemu, QemuError, QemuProcessHandle, SerialError};
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
    #[error("Qemu Error while listening to serial")]
    QemuSerial(#[source] SerialError),
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
        let lc = nanos::prepare_launch(
            wc,
            tap,
            &nanos::Args {
                klib_dir: "/home/ls/dima/nanos/output/klib/bin".to_string(),
                kernel: "/home/ls/dima/nanos/output/platform/pc/bin/kernel.img".to_string(),
                klibs: vec!["shmem".to_string(), "tmpfs".to_string()],
            },
        )
        .await
        .map_err(Error::Nanos)?;
        let handle = start_qemu(lc).await.map_err(Error::Qemu)?;
        match timeout(Duration::from_secs(10), serial(&handle)).await {
            Err(_) => Ok(handle),
            Ok(Ok(())) => unreachable!(),
            Ok(Err(e)) => Err(Error::QemuSerial(e)),
        }
    })
}

struct ProcessOption<'a, 'b: 'a> {
    index: usize,
    qph: &'a mut QemuProcessHandle<'b>,
}

impl Display for ProcessOption<'_, '_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}", self.qph))
    }
}

fn run_stop<'a>(
    instances: &mut Vec<QemuProcessHandle<'a>>,
) -> Result<Vec<QemuProcessHandle<'a>>, (Vec<QemuProcessHandle<'a>>, Error)> {
    let options = instances
        .iter_mut()
        .enumerate()
        .map(|(i, o)| ProcessOption { index: i, qph: o })
        .collect();

    let options = inquire::MultiSelect::new("Stop machines?", options)
        .prompt()
        .map_err(|e| (vec![], Error::Inquire(e)))?;

    let mut indexes_to_remove = vec![];
    let mut first_error: Option<Error> = None;
    for option in options {
        match task::block_on(option.qph.stop()).map_err(Error::Qemu) {
            Ok(_) => {
                indexes_to_remove.push(option.index);
            }
            Err(e) => {
                first_error = Some(e);
                break;
            }
        }
    }

    let mut removed_instances = vec![];
    for x in indexes_to_remove.into_iter().sorted().rev() {
        removed_instances.push(instances.swap_remove(x));
    }

    if let Some(e) = first_error {
        Err((removed_instances, e))
    } else {
        Ok(removed_instances)
    }
}

fn add_worker(nc: &NetworkConfig) -> Result<QemuProcessHandle, Error> {
    let worker_id = inquire::CustomType::<usize>::new("WorkerId?")
        .prompt()
        .map_err(Error::Inquire)?;

    let number_of_worker_threads = inquire::CustomType::<usize>::new("Number of Worker Threads?")
        .prompt()
        .map_err(Error::Inquire)?;

    let tap = task::block_on(nc.get_tap());
    let worker_config = WorkerConfiguration {
        host_ip_addr: IpAddr::from(nc.host_ip()),
        ip_addr: IpAddr::from(*tap.ip()),
        worker_id,
        parent_id: worker_id - 1,
        sources: vec![TCPSourceConfigBuilder::default()
            .format(Format::NES(8))
            .socket_port(8080)
            .logical_source_name("bid".to_string())
            .build()
            .unwrap()
            .into()],
        log_level: "LOG_INFO",
        query_processing: WorkerQueryProcessingConfigurationBuilder::default()
            .number_of_worker_threads(number_of_worker_threads)
            .buffer_size(8192)
            .number_of_source_buffers(128)
            .total_number_of_buffers(4096)
            .number_of_buffers_per_thread(128)
            .build()
            .unwrap()
            .into(),
    };
    task::block_on(async move {
        let wc = worker_config;
        let args = flatcar::Args {
            flatcar_fresh_image: PathBuf::from("./flatcar_fresh.iso"),
            number_of_cores: Some(number_of_worker_threads * 2),
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
        let mut stoped_instances = vec![];
        loop {
            let actions = vec!["stop", "add worker", "ps", "uk", "exit", "restart"];
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
                    "restart" => match run_restart(&mut stoped_instances).as_mut() {
                        Ok(started) => {
                            qemu_instances.append(started);
                        }
                        Err((started, err)) => {
                            qemu_instances.append(started);
                            error!(%err, "Could not start all instances")
                        }
                    },
                    "stop" => match run_stop(&mut qemu_instances).as_mut() {
                        Ok(removed) => {
                            stoped_instances.append(removed);
                        }
                        Err((removed, err)) => {
                            stoped_instances.append(removed);
                            error!(%err, "Could not remove all instances")
                        }
                    },
                    "exit" => {
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

fn run_restart<'a>(
    stopped_instances: &mut Vec<QemuProcessHandle<'a>>,
) -> Result<Vec<QemuProcessHandle<'a>>, (Vec<QemuProcessHandle<'a>>, Error)> {
    let options = stopped_instances
        .iter_mut()
        .enumerate()
        .map(|(i, o)| ProcessOption { index: i, qph: o })
        .collect();

    let options = inquire::MultiSelect::new("Restart machines?", options)
        .prompt()
        .map_err(|e| (vec![], Error::Inquire(e)))?;

    let mut indexes_to_remove = vec![];
    let mut first_error: Option<Error> = None;
    for option in options {
        match task::block_on(option.qph.restart()).map_err(Error::Qemu) {
            Ok(_) => {
                indexes_to_remove.push(option.index);
            }
            Err(e) => {
                first_error = Some(e);
                break;
            }
        }
    }

    let mut removed_instances = vec![];
    for x in indexes_to_remove.into_iter().sorted().rev() {
        removed_instances.push(stopped_instances.swap_remove(x));
    }

    if let Some(e) = first_error {
        Err((removed_instances, e))
    } else {
        Ok(removed_instances)
    }
}
