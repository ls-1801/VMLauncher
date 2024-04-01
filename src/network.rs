use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::ops::Deref;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::{cell::RefCell, sync};

use async_std::task;

use crate::network::userbridge::UserBridgeError;
use ipnet::{IpSub, Ipv4AddrRange, Ipv4Net};
use itertools::Itertools;
use macaddr::MacAddr;
use rand::random;
use tracing::{instrument, warn, Level};

use crate::shell::{run_shell_command, ShellError};
mod common;
pub(crate) mod userbridge;
pub(crate) mod usertap;

#[derive(Debug, Clone)]
struct Bridge {
    bridge: Arc<RwLock<userbridge::Bridge>>,
    ip_addr: Ipv4Net,
}

#[instrument(level = tracing::Level::DEBUG)]
pub(crate) fn network_setup(ip_net: Ipv4Net) -> NetworkConfig {
    return NetworkConfig {
        bridges: Bridge::create_bridge("tbr0", ip_net).unwrap(),
        ip_allocator: sync::Arc::new(sync::RwLock::new(IpAddressAllocator::new(
            Ipv4AddrRange::new(
                ip_net.hosts().skip(1).next().unwrap(),
                ip_net.hosts().last().unwrap(),
            ),
        ))),
    };
}

#[tracing::instrument(level = tracing::Level::DEBUG)]
pub(crate) async fn network_cleanup(nc: NetworkConfig) {}

impl Bridge {
    fn host_ip(&self) -> Ipv4Addr {
        self.ip_addr.hosts().next().unwrap()
    }
    fn register_tap_device(&self, tap: &Tap) {
        self.bridge
            .write()
            .unwrap()
            .add_tap(tap.tap.read().unwrap().deref())
            .unwrap();
    }

    fn create_bridge(name: &str, ip_net: Ipv4Net) -> Result<Bridge, UserBridgeError> {
        let bridge = Arc::new(RwLock::new(userbridge::Bridge::new(name, ip_net)?));

        Ok(Bridge {
            bridge,
            ip_addr: ip_net,
        })
    }
}

#[derive(Debug, Clone)]
struct Tap {
    pub(crate) ip_addr: Ipv4Addr,
    pub(crate) mac_addr: MacAddr,
    tap: Arc<RwLock<usertap::Tap>>,
}

impl Tap {
    fn create(name: String, ip_addr: Ipv4Addr) -> Self {
        Tap {
            ip_addr,
            mac_addr: MacAddr::from([0x0, 0x60, 0x2f, random(), random(), random()]),
            tap: Arc::new(RwLock::new(usertap::Tap::new(&name).unwrap())),
        }
    }

    fn destroy(self) {}
}

#[tracing::instrument(level = tracing::Level::DEBUG, err(level = tracing::Level::INFO))]
async fn run_ip_command(command: &str, args: Vec<&str>) -> Result<String, ShellError> {
    run_shell_command(
        "/usr/bin/ip",
        &vec![command]
            .into_iter()
            .chain(args.iter().cloned())
            .collect(),
    )
    .await
}

#[tracing::instrument(level = tracing::Level::DEBUG, err(level = tracing::Level::INFO))]
async fn run_brctl_command(command: &str, args: &Vec<&str>) -> Result<String, ShellError> {
    run_shell_command(
        "/usr/sbin/brctl",
        &vec![command]
            .into_iter()
            .chain(args.iter().cloned())
            .collect(),
    )
    .await
}

#[tracing::instrument(level = tracing::Level::DEBUG, err(level = tracing::Level::INFO))]
async fn run_tunctl_command(command: &str, args: &Vec<&str>) -> Result<String, ShellError> {
    run_shell_command(
        "/usr/bin/tunctl",
        &vec![command]
            .into_iter()
            .chain(args.iter().cloned())
            .collect(),
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

#[derive(Debug, Clone)]
pub(crate) struct NetworkConfig {
    bridges: Bridge,
    ip_allocator: std::sync::Arc<sync::RwLock<IpAddressAllocator>>,
}

#[derive(Debug)]
pub(crate) struct TapUser {
    config: NetworkConfig,
    tap: Option<Tap>,
}

impl TapUser {
    pub fn device(&self) -> String {
        self.tap.as_ref().unwrap().tap.read().unwrap().deref().name.to_string()
    }
    pub fn mac(&self) -> &MacAddr {
        &self.tap.as_ref().unwrap().mac_addr
    }
    pub fn ip(&self) -> &Ipv4Addr {
        &self.tap.as_ref().unwrap().ip_addr
    }
}

impl Drop for TapUser {
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
    pub fn get_tap(&self) -> TapUser {
        let ip = self
            .ip_allocator
            .write()
            .unwrap()
            .allocate()
            .expect("Out of ips");
        let id = self.ip_allocator.read().unwrap().to_id(ip);
        let tap = Tap::create(format!("tap{id}"), ip);
        self.bridges.register_tap_device(&tap);
        TapUser {
            config: self.clone(),
            tap: Some(tap),
        }
    }
    async fn release_tap(&self, tap: Tap) {
        self.ip_allocator.write().unwrap().free(tap.ip_addr);
    }
}
