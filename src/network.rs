use std::cell::RefCell;
use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use async_std::task;

use ipnet::{IpSub, Ipv4AddrRange, Ipv4Net};
use itertools::Itertools;
use macaddr::MacAddr;
use rand::random;
use tracing::warn;

use crate::shell::run_shell_command;

#[derive(Debug)]
struct Bridge {
    name: String,
    ip_addr: Ipv4Net,
}

#[tracing::instrument]
pub(crate) async fn network_setup(ip_net: Ipv4Net) -> NetworkConfig {
    sudo::escalate_if_needed().unwrap();
    let network = Bridge::create_bridge("tbr0".to_string(), ip_net).await;

    return NetworkConfig {
        bridges: network,
        ip_allocator: RefCell::new(IpAddressAllocator::new(Ipv4AddrRange::new(
            "10.0.0.2".parse().unwrap(),
            "10.0.0.20".parse().unwrap(),
        ))),
    };
}

#[tracing::instrument]
pub(crate) async fn network_cleanup(nc: NetworkConfig) {
    nc.bridges.destroy().await;
}

impl Bridge {
    fn host_ip(&self) -> Ipv4Addr {
        self.ip_addr.hosts().next().unwrap()
    }
    fn parse_ip_show_output(output: &str) -> Result<Ipv4Net, String> {
        let inet_line = output.lines().find(|l| l.trim().starts_with("inet"));
        let ip = inet_line.unwrap().split_whitespace().nth(1);
        Ok(str::parse(ip.unwrap()).unwrap())
    }
    async fn find_all() -> Result<Vec<Bridge>, String> {
        let output = run_brctl_command("show", vec![]).await?;
        if output.is_empty() {
            return Ok(vec![]);
        }
        let mut bridges = vec![];
        for line in output.lines().skip(1) {
            let name = line.split_ascii_whitespace().next().unwrap();
            let ip_show_output = run_ip_command("a", vec!["show", name])
                .await
                .expect("could not fetch further info about net device");
            let ip = Self::parse_ip_show_output(&ip_show_output);
            bridges.push(Bridge {
                name: name.to_string(),
                ip_addr: ip.unwrap(),
            });
        }

        Ok(bridges)
    }
    async fn register_tap_device(&self, tap: &Tap) {
        run_brctl_command("addif", vec![&self.name, &tap.name])
            .await
            .expect("Could not register tap device at bridge");
    }

    async fn create_bridge(name: String, ip_net: Ipv4Net) -> Bridge {
        let new_bridge = Bridge {
            name,
            ip_addr: ip_net,
        };

        if let Some(existing_bridge) = Self::find_all()
            .await
            .unwrap_or(vec![])
            .into_iter()
            .find(|b| b.name == new_bridge.name)
        {
            if existing_bridge.ip_addr == new_bridge.ip_addr {
                warn!(name = new_bridge.name, ip_net = %new_bridge.ip_addr, "Bridge already exists");
                return existing_bridge;
            } else {
                warn!(name = new_bridge.name, ip_net = %new_bridge.ip_addr, other_ip = %existing_bridge.ip_addr, "Bridge already exists! Deleting old");
                Self::destroy(existing_bridge).await;
            }
        }

        run_brctl_command("addbr", vec![&new_bridge.name])
            .await
            .expect("Could not create bridge");

        run_ip_command("link", vec!["set", "dev", &new_bridge.name, "up"])
            .await
            .expect("Could not bring bridge up");

        run_ip_command(
            "addr",
            vec![
                "add",
                &new_bridge.host_ip().to_string(),
                "dev",
                &new_bridge.name,
            ],
        )
        .await
        .expect("Could not set ip addr");

        run_ip_command(
            "route",
            vec!["add", &ip_net.to_string(), "dev", &new_bridge.name],
        )
        .await
        .expect("Could not configure route");

        new_bridge
    }

    async fn destroy(self) {
        run_ip_command(
            "route",
            vec!["del", &self.ip_addr.to_string(), "dev", &self.name],
        )
        .await
        .expect("could not delete route");
        run_ip_command("link", vec!["set", "dev", &self.name, "down"])
            .await
            .expect("could not bring bridge down");
        run_brctl_command("delbr", vec![&self.name])
            .await
            .expect("could not delete bridge");
    }
}

#[derive(Debug, Clone)]
struct Tap {
    pub(crate) name: String,
    pub(crate) ip_addr: Ipv4Addr,
    pub(crate) mac_addr: MacAddr,
}

impl Tap {
    async fn create(name: String, ip_addr: Ipv4Addr) -> Self {
        run_ip_command("tuntap", vec!["add", &name, "mode", "tap"])
            .await
            .expect("could not create tap device");

        run_ip_command("link", vec!["set", "dev", &name, "up"])
            .await
            .expect("Could not start");

        Tap {
            name,
            ip_addr,
            mac_addr: MacAddr::from([0x0, 0x60, 0x2f, random(), random(), random()]),
        }
    }

    async fn destroy(self) {
        run_ip_command("tuntap", vec!["del", &self.name, "mode", "tap"])
            .await
            .expect("Could not delete tap device");
    }
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

#[tracing::instrument]
async fn run_tunctl_command(command: &str, args: Vec<&str>) -> Result<String, String> {
    run_shell_command(
        "/usr/bin/tunctl",
        vec![command].into_iter().chain(args.into_iter()).collect(),
    )
    .await
}

#[derive(Debug)]
struct IpAddressAllocator {
    ip: Ipv4AddrRange,
    free: BTreeSet<(usize, usize)>,
}

impl IpAddressAllocator {
    fn max(ip: Ipv4AddrRange) -> usize {
        ip.size_hint().0
    }

    pub fn new(address_range: Ipv4AddrRange) -> Self {
        Self {
            ip: address_range,
            free: BTreeSet::from([(Self::max(address_range) - 1, 0)]),
        }
    }
    fn to_id(&self, ip_net: Ipv4Addr) -> usize {
        let host = <Ipv4AddrRange as Iterator>::min(self.ip).unwrap();
        ip_net.saturating_sub(host) as usize
    }
    fn to_ip(&self, value: usize) -> Ipv4Addr {
        self.ip.clone().nth(value).expect("Could not assign ip")
    }
    pub fn allocate(&mut self) -> Option<Ipv4Addr> {
        if let Some((end, start)) = self.free.pop_first() {
            if start != end {
                self.free.insert((end, start + 1));
            }
            Some(self.to_ip(start))
        } else {
            None
        }
    }

    fn compact(&mut self) {
        self.free = self
            .free
            .iter()
            .cloned()
            .coalesce(|(a_end, a_start), (b_end, b_start)| {
                if a_end + 1 == b_start {
                    Ok((b_end, a_start))
                } else {
                    Err(((a_end, a_start), (b_end, b_start)))
                }
            })
            .collect();
    }
    pub fn free(&mut self, ip_net: Ipv4Addr) {
        let host = <Ipv4AddrRange as Iterator>::min(self.ip).unwrap();
        let id = ip_net.saturating_sub(host) as usize;
        assert!(id <= Self::max(self.ip));
        self.free.insert((id, id));
        self.compact();
        println!("After Compaction {:?}", self.free);
    }
}

#[test]
fn ip_allocation() {
    let mut allocator = IpAddressAllocator::new(Ipv4AddrRange::new(
        "10.0.0.2".parse().unwrap(),
        "10.0.0.6".parse().unwrap(),
    ));

    assert_eq!(allocator.allocate(), Some("10.0.0.2".parse().unwrap()));
    assert_eq!(allocator.allocate(), Some("10.0.0.3".parse().unwrap()));
    assert_eq!(allocator.allocate(), Some("10.0.0.4".parse().unwrap()));
    assert_eq!(allocator.allocate(), Some("10.0.0.5".parse().unwrap()));
    assert_eq!(allocator.allocate(), Some("10.0.0.6".parse().unwrap()));
    assert_eq!(allocator.allocate(), None);

    allocator.free("10.0.0.4".parse().unwrap());
    assert_eq!(allocator.allocate(), Some("10.0.0.4".parse().unwrap()));

    allocator.free("10.0.0.3".parse().unwrap());
    allocator.free("10.0.0.5".parse().unwrap());

    assert_eq!(allocator.allocate(), Some("10.0.0.3".parse().unwrap()));
    assert_eq!(allocator.allocate(), Some("10.0.0.5".parse().unwrap()));

    allocator.free("10.0.0.3".parse().unwrap());
    allocator.free("10.0.0.4".parse().unwrap());
    allocator.free("10.0.0.5".parse().unwrap());

    assert_eq!(allocator.allocate(), Some("10.0.0.3".parse().unwrap()));
    assert_eq!(allocator.allocate(), Some("10.0.0.4".parse().unwrap()));
    allocator.free("10.0.0.6".parse().unwrap());
    assert_eq!(allocator.allocate(), Some("10.0.0.5".parse().unwrap()));
    assert_eq!(allocator.allocate(), Some("10.0.0.6".parse().unwrap()));
}

#[derive(Debug)]
pub(crate) struct NetworkConfig {
    bridges: Bridge,
    ip_allocator: RefCell<IpAddressAllocator>,
}

#[derive(Debug)]
pub(crate) struct TapUser<'nc> {
    config: &'nc NetworkConfig,
    tap: Option<Tap>,
}

impl TapUser<'_> {
    pub fn device(&self) -> &str {
        &self.tap.as_ref().unwrap().name
    }
    pub fn mac(&self) -> &MacAddr {
        &self.tap.as_ref().unwrap().mac_addr
    }
    pub fn ip(&self) -> &Ipv4Addr {
        &self.tap.as_ref().unwrap().ip_addr
    }
}

impl<'nc> Drop for TapUser<'nc> {
    fn drop(&mut self) {
        if let Some(tap) = self.tap.take() {
            task::block_on(self.config.release_tap(tap));
        }
    }
}

impl NetworkConfig {
    pub(crate) fn host_ip(&self) -> Ipv4Addr {
        self.bridges.ip_addr.hosts().next().unwrap()
    }
    pub async fn get_tap(&self) -> TapUser {
        let ip = self
            .ip_allocator
            .borrow_mut()
            .allocate()
            .expect("Out of ips");
        let id = self.ip_allocator.borrow().to_id(ip);
        let tap = Tap::create(format!("tap{id}"), ip).await;
        self.bridges.register_tap_device(&tap).await;
        TapUser {
            config: self,
            tap: Some(tap),
        }
    }
    async fn release_tap(&self, tap: Tap) {
        self.ip_allocator.borrow_mut().free(tap.ip_addr);
        tap.destroy().await;
    }
}
