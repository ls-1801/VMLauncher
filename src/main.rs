mod flatcar;

use async_process::Command;
use rust_embed::{EmbeddedFile, RustEmbed};
use serde::Serialize;
use std::convert::AsRef;
use std::net::{IpAddr, Ipv4Addr};
use std::process::{Output, Stdio};
use std::str::FromStr;
use std::thread::sleep;
use std::time::Duration;
use tinytemplate::TinyTemplate;
use tracing::{error, info, span, warn};

const WORKER_CONFIG_TEMPLATE: &str = "worker_config";
const TEMPLATE_FILES: [&str; 1] = [WORKER_CONFIG_TEMPLATE];

fn main() {
    tracing_subscriber::fmt::init();
    let bridges = futures_lite::future::block_on(network_setup());
    sleep(Duration::from_secs(5));
    futures_lite::future::block_on(network_cleanup(bridges));
}

#[tracing::instrument]
async fn network_setup() -> NetworkConfig {
    sudo::escalate_if_needed().unwrap();
    let mut network =
        Bridge::create_bridge("tbr0".to_string(), IpAddr::from(Ipv4Addr::new(10, 0, 0, 1))).await;

    let tap0 = Tap::create("tap0".to_string(), IpAddr::from(Ipv4Addr::new(10, 0, 0, 2))).await;

    network.register_tap_device(&tap0);

    return NetworkConfig {
        bridges: network,
        taps: vec![tap0],
    };
}

#[tracing::instrument]
async fn network_cleanup(nc: NetworkConfig) {
    nc.bridges.destroy().await;

    for tap in nc.taps {
        tap.destroy().await;
    }
}

#[derive(RustEmbed)]
#[folder = "resources/"]
struct TemplateAssets;

#[derive(Debug)]
struct Bridge {
    name: String,
    ip_addr: IpAddr,
}

#[tracing::instrument]
async fn run_shell_command(command: &str, args: Vec<&str>) -> Result<String, String> {
    info!("starting");
    let mut child = Command::new(command)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let exit_status = child.status().await.unwrap();
    let output = child.output().await.unwrap();

    if !exit_status.success() {
        error!(
            status = exit_status.code().unwrap(),
            error = stderr_as_str(&output),
            "Unexpected Exit status"
        );
        return Err(stderr_as_str(&output).to_string());
    }

    let stdout = stdout_as_str(&output);
    info!(output = stdout, "done");

    return Ok(stdout.to_string());
}

pub fn stdout_as_str(output: &Output) -> &str {
    std::str::from_utf8(output.stdout.as_ref()).unwrap()
}

pub fn stderr_as_str(output: &Output) -> &str {
    std::str::from_utf8(output.stderr.as_ref()).unwrap()
}

impl Bridge {
    pub async fn find_all() -> Result<Vec<Bridge>, String> {
        let output = Self::run_brctl_command("show", vec![]).await?;
        if output.is_empty() {
            return Ok(vec![]);
        }

        let bridges = output
            .lines()
            .skip(1)
            .map(|line| {
                let name = line.split_ascii_whitespace().next().unwrap();
                Bridge {
                    name: name.to_string(),
                    ip_addr: IpAddr::from_str("0.0.0.0").unwrap(),
                }
            })
            .collect();

        Ok(bridges)
    }

    pub async fn register_tap_device(&mut self, tap: &Tap) {
        Self::run_brctl_command("addif", vec![&self.name, &tap.name])
            .await
            .expect("Could not register tap device at bridge");
    }

    pub async fn create_bridge(name: String, ip_addr: IpAddr) -> Bridge {
        if let Some(existing_bridge) = Self::find_all()
            .await
            .unwrap_or(vec![])
            .into_iter()
            .find(|b| b.name == name)
        {
            if existing_bridge.ip_addr == ip_addr {
                warn!(name, %ip_addr, "Bridge already exists");
                return Bridge { name, ip_addr };
            } else {
                warn!(name, %ip_addr, other_ip = %existing_bridge.ip_addr, "Bridge already exists! Deleting old");
                Self::destroy(existing_bridge).await;
            }
        }

        Self::run_brctl_command("addbr", vec![&name])
            .await
            .expect("Could not create bridge");

        Self::run_ip_command("link", vec!["set", "dev", &name, "up"])
            .await
            .expect("Could not bring bridge up");

        Self::run_ip_command(
            "addr",
            vec!["add", &format!("{}/24", ip_addr.to_string()), "dev", &name],
        )
        .await
        .expect("Could not set ip addr");
        Bridge { name, ip_addr }
    }

    async fn destroy(self) {
        Self::run_ip_command("link", vec!["set", "dev", &self.name, "down"])
            .await
            .expect("could not bring bridge down");
        Self::run_brctl_command("delbr", vec![&self.name])
            .await
            .expect("could not delete bridge");
    }

    #[tracing::instrument]
    async fn run_ip_command(command: &str, args: Vec<&str>) -> Result<String, String> {
        run_shell_command(
            "/usr/bin/ip",
            vec![command].into_iter().chain(args.into_iter()).collect(),
        )
        .await
    }

    #[tracing::instrument]
    async fn run_brctl_command(command: &str, args: Vec<&str>) -> Result<String, String> {
        run_shell_command(
            "/usr/sbin/brctl",
            vec![command].into_iter().chain(args.into_iter()).collect(),
        )
        .await
    }
}

#[derive(Debug)]
struct Tap {
    name: String,
    ip_addr: IpAddr,
}

impl Tap {
    pub async fn create(name: String, ip_addr: IpAddr) -> Self {
        Self::run_tunctl_command("-t", vec![&name, "-u", "ls"])
            .await
            .expect("Could not create tap device");

        Tap { name, ip_addr }
    }

    pub async fn destroy(self) {
        Self::run_tunctl_command("-d", vec![&self.name])
            .await
            .expect("Could not delete tap device");
    }

    #[tracing::instrument]
    async fn run_tunctl_command(command: &str, args: Vec<&str>) -> Result<String, String> {
        run_shell_command(
            "/usr/bin/tunctl",
            vec![command].into_iter().chain(args.into_iter()).collect(),
        )
        .await
    }
}

#[derive(Debug)]
struct NetworkConfig {
    bridges: Bridge,
    taps: Vec<Tap>,
}

struct Templates<'embedded_files> {
    tt: TinyTemplate<'embedded_files>,
}

impl<'embedded_file> Templates<'embedded_file> {
    pub fn new(files: &'embedded_file Vec<(&str, EmbeddedFile)>) -> Self {
        let mut tt = TinyTemplate::new();
        for (name, file) in files {
            let template_str = std::str::from_utf8(file.data.as_ref()).unwrap();
            tt.add_template(name, template_str).unwrap();
        }

        Templates { tt }
    }

    pub fn worker_config(&self, wc: &WorkerConfiguration) -> String {
        self.tt.render(WORKER_CONFIG_TEMPLATE, &wc).unwrap()
    }
}

#[derive(Serialize)]
struct WorkerConfiguration {
    ip_addr: String,
    host_ip_addr: String,
    worker_id: usize,
    parent_id: usize,
}
