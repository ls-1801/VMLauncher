use crate::shell::run_shell_command;
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;
use macaddr::MacAddr;
use tracing::warn;
use rand::random;

#[derive(Debug)]
pub(crate) struct Bridge {
    name: String,
    ip_addr: IpAddr,
}

#[tracing::instrument]
pub(crate) async fn network_setup() -> NetworkConfig {
    sudo::escalate_if_needed().unwrap();
    let mut network =
        Bridge::create_bridge("tbr0".to_string(), IpAddr::from(Ipv4Addr::new(10, 0, 0, 1))).await;

    let tap0 = Tap::create("tap0".to_string(), IpAddr::from(Ipv4Addr::new(10, 0, 0, 2))).await;

    network.register_tap_device(&tap0).await;

    return NetworkConfig {
        bridges: network,
        taps: vec![tap0],
    };
}

#[tracing::instrument]
pub(crate) async fn network_cleanup(nc: NetworkConfig) {
    nc.bridges.destroy().await;

    for tap in nc.taps {
        tap.destroy().await;
    }
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
pub(crate) struct Tap {
    pub(crate) name: String,
    ip_addr: IpAddr,
    pub(crate) mac_addr: MacAddr,
}

impl Tap {
    pub async fn create(name: String, ip_addr: IpAddr) -> Self {
        Self::run_tunctl_command("-t", vec![&name, "-u", "ls"])
            .await
            .expect("Could not create tap device");

        Tap { name, ip_addr, mac_addr: MacAddr::from([0x0, 0x60, 0x2f, random(), random(), random()]) }
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
pub(crate) struct NetworkConfig {
    bridges: Bridge,
    taps: Vec<Tap>,
}
