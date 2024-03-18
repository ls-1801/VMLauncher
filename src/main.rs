use async_std::future::timeout;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::stdin;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::nes::{
    Format, Source, TCPSourceConfig, TCPSourceConfigBuilder,
    WorkerQueryProcessingConfigurationBuilder,
};
use async_std::task;
use async_std::task::JoinHandle;
use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand};
use inquire::{CustomType, InquireError};
use ipnet::Ipv4Net;
use itertools::Itertools;
use serde::Deserialize;
use thiserror::Error;
use tracing::{error, info};

use crate::network::{network_cleanup, network_setup, NetworkConfig};
use crate::qemu::{serial, start_qemu, QemuError, QemuProcessHandle, SerialError, serial_with_command};
use crate::templates::WorkerConfiguration;

mod flatcar;
mod nanos;
mod nes;
mod network;
mod qemu;
mod shell;
mod templates;

#[derive(Parser)]
struct ProgramArgs {
    #[arg(short = 'k')]
    keep_bridge_alive: bool,
    #[clap(subcommand)]
    command: VMLauncherCommand,
}

#[derive(Subcommand)]
enum VMLauncherCommand {
    Interactive(InteractiveArgs),
    Script(ScriptArgs),
}

#[derive(Debug, Args)]
struct InteractiveArgs {
    #[arg(short = 'n')]
    ip_range: Option<Ipv4Net>,
}

#[derive(Debug, Args)]
struct ScriptArgs {
    #[arg(short = 'n')]
    ip_range: Ipv4Net,
    config: Option<Utf8PathBuf>,
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
    #[error("Qemu Error while doing io")]
    IO(#[source] std::io::Error),
    #[error("Could not open script file: {1}. Error: {0}")]
    ScriptFileNotFound(#[source] std::io::Error, Utf8PathBuf),
    #[error("Qemu Error while doing io")]
    Deserialization(#[source] serde_yaml::Error),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddUnikernelArgs {
    node_id: usize,
    query_id: usize,
    path_to_binary: String,
    args: Vec<String>,
    ip: Option<Ipv4Addr>,
}

impl AddUnikernelArgs {
    fn inquire() -> Result<Self, InquireError> {
        let node_id = inquire::CustomType::<usize>::new("NodeId?").prompt()?;
        let query_id = inquire::CustomType::<usize>::new("WorkerId?").prompt()?;
        let path_to_binary = inquire::CustomType::<Utf8PathBuf>::new("Binary?")
            .with_default(Utf8PathBuf::from(
                "/home/ls/dima/nes-test-queries/nexmarkq0/build/unikernel2.debug",
            ))
            .prompt()?;
        let args = inquire::Text::new("args").prompt_skippable()?;

        Ok(AddUnikernelArgs {
            node_id,
            query_id,
            path_to_binary: path_to_binary.to_string(),
            args: args
                .unwrap_or("".to_string())
                .split(' ')
                .map(|s| s.to_string())
                .collect(),
            ip: inquire::CustomType::<Ipv4Addr>::new("ip ?").prompt_skippable()?,
        })
    }
}

fn add_unikernel<'a>(
    nc: &'a NetworkConfig,
    args: AddUnikernelArgs,
) -> Result<(QemuProcessHandle<'a>, JoinHandle<Result<(), Error>>), Error> {
    let wc = nanos::UnikernelWorkerConfig {
        node_id: args.node_id,
        query_id: args.query_id,
        elf_binary: Utf8PathBuf::from(args.path_to_binary),
        args: Some(args.args.join(" ")),
        ip: args.ip,
    };

    task::block_on(async move {
        let tap = nc.get_tap().await;
        let lc = nanos::prepare_launch(
            wc,
            tap,
            &nanos::Args {
                klib_dir: None,
                kernel: None,
                klibs: vec!["shmem".to_string(), "tmpfs".to_string()],
                debugflags: vec![],
            },
        )
            .await
            .map_err(Error::Nanos)?;
        let handle = start_qemu(lc).await.map_err(Error::Qemu)?;
        let serial_socket = handle.serial_path();
        Ok((
            handle,
            task::spawn(async move {
                serial(serial_socket, args.node_id)
                    .await
                    .map_err(|e| Error::QemuSerial(e))
            }),
        ))
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddWorkerArgs {
    worker_id: usize,
    number_of_worker_threads: usize,
}

impl AddWorkerArgs {
    pub fn inquire() -> Result<Self, InquireError> {
        let worker_id = inquire::CustomType::<usize>::new("WorkerId?").prompt()?;
        let number_of_worker_threads =
            inquire::CustomType::<usize>::new("Number of Worker Threads?").prompt()?;
        Ok(Self {
            worker_id,
            number_of_worker_threads,
        })
    }
}

fn add_worker<'a>(nc: &'a NetworkConfig, args: AddWorkerArgs) -> Result<(QemuProcessHandle<'a>, JoinHandle<Result<(), Error>>), Error> {
    let tap = task::block_on(nc.get_tap());
    let worker_id = args.worker_id;
    let worker_config = WorkerConfiguration {
        host_ip_addr: IpAddr::from(nc.host_ip()),
        ip_addr: IpAddr::from(*tap.ip()),
        worker_id: args.worker_id,
        parent_id: args.worker_id - 1,
        sources: vec![TCPSourceConfigBuilder::default()
            .format(Format::NES(8))
            .socket_port(8091)
            .logical_source_name("bid".to_string())
            .build()
            .unwrap()
            .into()],
        log_level: "LOG_INFO",
        query_processing: WorkerQueryProcessingConfigurationBuilder::default()
            .number_of_worker_threads(args.number_of_worker_threads)
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
            number_of_cores: Some(args.number_of_worker_threads),
        };
        let lc = flatcar::prepare_launch(wc, tap, &args).await;
        let handle = qemu::start_qemu(lc).await.map_err(Error::Qemu)?;
        let serial_socket = handle.serial_path();
        Ok((
            handle,
            task::spawn(async move {
                task::sleep(Duration::from_secs(20)).await;
                serial_with_command("journalctl -u nesWorker -f\n", serial_socket, worker_id)
                    .await
                    .map_err(|e| Error::QemuSerial(e))
            }),
        ))
    })
}

fn interactive_main(
    args: InteractiveArgs,
    keep_bridge_alive: bool,
) -> Result<(), Error> {
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
        let mut serials = vec![];
        let mut qemu_instances = vec![];
        let mut stopped_instances = vec![];
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
                    "uk" => match AddUnikernelArgs::inquire()
                        .map_err(Error::Inquire)
                        .and_then(|args| add_unikernel(&bridges, args))
                    {
                        Ok((qh, serial)) => {
                            qemu_instances.push(qh);
                            serials.push(serial);
                        }
                        Err(e) => {
                            error!(?e, "Could not create worker");
                        }
                    },
                    "ps" => {
                        for qh in &qemu_instances {
                            println!("{qh}")
                        }
                    }
                    "restart" => match run_restart(&mut stopped_instances).as_mut() {
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
                            stopped_instances.append(removed);
                        }
                        Err((removed, err)) => {
                            stopped_instances.append(removed);
                            error!(%err, "Could not remove all instances")
                        }
                    },
                    "exit" => {
                        break;
                    }
                    "add worker" => match AddWorkerArgs::inquire()
                        .map_err(Error::Inquire)
                        .and_then(|args| add_worker(&bridges, args))
                    {
                        Ok((qh, serial)) => {
                            qemu_instances.push(qh);
                            serials.push(serial);
                        }
                        Err(e) => {
                            error!(?e, "Could not create worker");
                        }
                    },
                    _ => unreachable!(),
                },
            }
        }
        info!("Stopping");
    }

    if !keep_bridge_alive {
        task::block_on(network_cleanup(bridges));
    }

    Ok(())
}

#[derive(Deserialize)]
struct Script {
    commands: Vec<ScriptCommands>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ScriptCommands {
    AddWorker(AddWorkerArgs),
    AddUnikernel(AddUnikernelArgs),
}

fn run_commands_stop_at_first_error<'nc>(
    bridges: &'nc NetworkConfig,
    qemu_instances: &mut Vec<QemuProcessHandle<'nc>>,
    serials: &mut Vec<JoinHandle<Result<(), Error>>>,
    commands: Vec<ScriptCommands>,
) -> Result<(), Error> {
    for command in commands {
        match command {
            ScriptCommands::AddWorker(args) => {
                let (qh, serial) = add_worker(&bridges, args)?;
                qemu_instances.push(qh);
                serials.push(serial);
            }
            ScriptCommands::AddUnikernel(args) => {
                let (qh, serial) = add_unikernel(&bridges, args)?;
                qemu_instances.push(qh);
                serials.push(serial);
            }
        }
    }

    Ok(())
}

fn script_main(
    args: ScriptArgs,
    keep_bridge_alive: bool,
) -> Result<(), Error> {
    let file: Box<dyn std::io::Read + 'static> = if let Some(ref path) = args.config {
        Box::from(File::open(path).map_err(|e| Error::ScriptFileNotFound(e, path.clone()))?)
    } else {
        Box::from(stdin())
    };

    let script: Script = serde_yaml::from_reader(file).map_err(Error::Deserialization)?;

    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let pair2 = Arc::clone(&pair);
    ctrlc::set_handler(move || {
        let (lock, cvar) = &*pair2;
        let mut stop = lock.lock().unwrap();
        *stop = true;
        cvar.notify_one();
    })
        .expect("Error settings ctrl-c handler");

    let bridges = futures_lite::future::block_on(network_setup(args.ip_range));
    {
        let mut qemu_instances = vec![];
        let mut serials = vec![];
        let result = run_commands_stop_at_first_error(
            &bridges,
            &mut qemu_instances,
            &mut serials,
            script.commands,
        );

        match result {
            Ok(_) => {
                info!("Commands run successful, waiting for Ctr-C");
                let (lock, cvar) = &*pair;
                let mut started = lock.lock().unwrap();
                // As long as the value inside the `Mutex<bool>` is `false`, we wait.
                while !*started {
                    started = cvar.wait(started).unwrap();
                }
            }
            Err(e) => {
                error!("Commands failed: {:?}", e)
            }
        }
    }

    if !keep_bridge_alive {
        task::block_on(network_cleanup(bridges));
    }

    Ok(())
}

fn main() {
    let args = ProgramArgs::parse();
    tracing_subscriber::fmt::init();

    match args.command {
        VMLauncherCommand::Interactive(ia) => {
            interactive_main(ia, args.keep_bridge_alive)
                .expect("Interactive Failed")
        }
        VMLauncherCommand::Script(sa) => {
            script_main(sa, args.keep_bridge_alive).expect("Script Failed")
        }
    };
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
