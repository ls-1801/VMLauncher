use std::ffi::{c_void, CStr, CString, FromBytesUntilNulError, NulError};
use std::mem;
use std::mem::MaybeUninit;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd, RawFd};

use bytemuck::{Pod, Zeroable};
use byteorder::ByteOrder;
use ipnet::Ipv4Net;
use libc::{
    __c_anonymous_ifr_ifru, c_char, c_int, c_long, c_short, ifreq, in_addr_t, in_port_t, memcpy,
    sa_family_t, AF_INET, IFF_BROADCAST, IFF_MULTICAST, IFF_RUNNING, IFF_UP, IPPROTO_IP,
    IPPROTO_TCP,
};
use nix::sys::ioctl;
use nix::sys::ioctl::ioctl_param_type;
use nix::sys::socket::{AddressFamily, SockFlag, SockProtocol, SockType};
use nix::unistd::close;
use nix::{
    ioctl_read, ioctl_readwrite, ioctl_write_buf, ioctl_write_int, ioctl_write_ptr, NixPath,
};
use thiserror::Error;
use tracing::error;
use users::get_current_uid;

use crate::network::common::{
    add_br, add_if, create_ifreq, del_br, get_if_flags, set_if_addr, set_if_flags, CommonError,
};
use crate::network::userbridge::UserBridgeError::{CouldNotAttachTap, CouldNotCreateBridge};
use crate::network::usertap::{Tap, UserTapError};

#[derive(Debug)]
pub(crate) struct Bridge {
    name: String,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Err(e) = self
            .is_up()
            .and_then(|up| {
                if up {
                    return self.down();
                }
                Ok(())
            })
            .and_then(|_| self.delete())
        {
            error!("Could not delete bridge {}: {}", self.name, e);
        }
    }
}

#[derive(Error, Debug)]
pub(crate) enum UserBridgeError {
    #[error("Testing Capabilities failed")]
    Caps(#[source] caps::errors::CapsError),
    #[error("Missing {0} Capability")]
    MissingCap(&'static str),
    #[error("/dev/net/tun does not exist")]
    DeviceDoesNotExist,
    #[error("/dev/net/tun cannot be written to")]
    DeviceNotAccessible,
    #[error("IO Error when {1}: {0}")]
    IO(#[source] std::io::Error, &'static str),
    #[error("FFI Error")]
    FFI(#[source] FromBytesUntilNulError),
    #[error("FFI Error")]
    FFINullError(#[source] NulError),
    #[error("Could not create Bridge Device: {1}. Error Code: {0}")]
    CouldNotCreateBridge(nix::Error, &'static str),
    #[error("Could not attach Tap Device: {1}. Error Code: {0}")]
    CouldNotAttachTap(nix::Error, &'static str),
    #[error("UserTapError while: {1}")]
    UserTap(#[source] UserTapError, &'static str),
    #[error("When creating Tap Device: {0}")]
    CommonError(#[from] CommonError),
    #[error("Socket error when {1}: {0}")]
    Socket(nix::Error, &'static str),
    #[error("Ioctl error when {1}: {0}")]
    Ioctl(nix::Error, &'static str),
}
#[repr(C)]
#[derive(bytemuck::NoUninit, Clone, Copy)]
struct InAddr {
    pub s_addr: libc::in_addr_t,
}
#[repr(C)]
#[derive(bytemuck::NoUninit, Clone, Copy)]
struct SockaddrIn {
    pub sin_family: libc::sa_family_t,
    pub sin_port: libc::in_port_t,
    pub sin_addr: InAddr,
    pub sin_zero: [u8; 8],
}

type Result<T> = core::result::Result<T, crate::network::userbridge::UserBridgeError>;
impl Bridge {
    pub fn down(&self) -> Result<()> {
        let flags = self.get_flags()?;
        let flags = flags & !(IFF_UP as c_short);
        self.set_flags(flags)
    }

    pub fn delete(&mut self) -> Result<()> {
        let cstring = CString::new(self.name.clone()).unwrap();
        let bridge_fd = nix::sys::socket::socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .map_err(|e| UserBridgeError::CouldNotCreateBridge(e, "Creating Unix Socket"))?;
        unsafe { del_br(bridge_fd.as_raw_fd(), cstring.as_ptr() as *const c_long) }
            .map_err(|e| UserBridgeError::Ioctl(e, "Deleting Bridge"))?;

        Ok(())
    }

    fn set_flags(&self, flags: c_short) -> Result<()> {
        let fd = nix::sys::socket::socket(
            AddressFamily::Inet,
            SockType::Datagram,
            SockFlag::empty(),
            None,
        )
        .map_err(|e| UserBridgeError::Socket(e, "Opening Unix Socket"))?;

        let mut req = create_ifreq(&self.name)?;
        req.ifr_ifru.ifru_flags = flags;

        unsafe { set_if_flags(fd.as_raw_fd(), &req as *const ifreq as *const c_int) }
            .map_err(|e| UserBridgeError::Ioctl(e, "Set IF Flags Ioctl"))?;

        Ok(())
    }
    fn get_flags(&self) -> Result<c_short> {
        let fd = nix::sys::socket::socket(
            AddressFamily::Inet,
            SockType::Datagram,
            SockFlag::empty(),
            None,
        )
        .map_err(|e| UserBridgeError::Socket(e, "Opening Unix Socket"))?;

        let mut req = create_ifreq(&self.name)?;

        unsafe { get_if_flags(fd.as_raw_fd(), &mut req as *mut ifreq as *mut c_int) }
            .map_err(|e| UserBridgeError::Ioctl(e, "Get IF Flags Ioctl"))?;

        Ok(unsafe { req.ifr_ifru.ifru_flags })
    }
    pub fn is_up(&self) -> Result<bool> {
        Ok(self.get_flags()? & IFF_UP as c_short == IFF_UP as c_short)
    }

    fn set_ip(&self, ipv4addr: Ipv4Addr) -> Result<()> {
        let mut req = create_ifreq(&self.name)?;
        let sai = SockaddrIn {
            sin_family: AF_INET as sa_family_t,
            sin_port: 0,
            sin_addr: InAddr {
                s_addr: byteorder::LittleEndian::read_u32(&ipv4addr.octets()),
            },
            sin_zero: [0u8; 8],
        };

        unsafe {
            req.ifr_ifru.ifru_addr = mem::transmute(sai);
        }

        let fd = nix::sys::socket::socket(
            AddressFamily::Inet,
            SockType::Datagram,
            SockFlag::empty(),
            None,
        )
        .map_err(|e| UserBridgeError::CouldNotCreateBridge(e, "Opening Unix Socket"))?;

        unsafe { set_if_addr(fd.as_raw_fd(), &req as *const ifreq as *const c_int) }
            .map_err(|e| CouldNotCreateBridge(e, "Set IF ADDR Ioctl"))?;

        Ok(())
    }
    pub fn new(name: &str, ipv4addr: Ipv4Net) -> Result<Self> {
        Self::check_caps()?;
        let bridge_fd = nix::sys::socket::socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .map_err(|e| UserBridgeError::CouldNotCreateBridge(e, "Creating Unix Socket"))?;

        let name = name.to_string();
        let cstring = CString::new(name.clone()).map_err(UserBridgeError::FFINullError)?;

        unsafe { add_br(bridge_fd.as_raw_fd(), cstring.as_ptr() as *const c_long) }
            .map_err(|e| UserBridgeError::CouldNotCreateBridge(e, "AddBridge IOCTL"))?;

        let bridge = Bridge { name };

        bridge.set_ip(ipv4addr.network())?;

        bridge.set_flags((IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST) as c_short)?;

        Ok(bridge)
    }

    pub fn add_tap(&self, tap: &Tap) -> Result<()> {
        let index = tap
            .get_index()
            .map_err(|e| UserBridgeError::UserTap(e, "Requesting Index"))?;
        let mut request = create_ifreq(&self.name)?;
        request.ifr_ifru.ifru_ifindex = index;
        let bridge_fd = nix::sys::socket::socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .map_err(|e| UserBridgeError::CouldNotCreateBridge(e, "Creating Unix Socket"))?;
        unsafe {
            add_if(
                bridge_fd.as_raw_fd(),
                &request as *const ifreq as *const c_int,
            )
        }
        .map_err(|e| CouldNotAttachTap(e, "IOCTL"))?;
        Ok(())
    }

    fn check_caps() -> Result<()> {
        use caps::{CapSet, Capability};
        if !caps::has_cap(None, CapSet::Effective, Capability::CAP_NET_ADMIN)
            .map_err(UserBridgeError::Caps)?
        {
            Err(UserBridgeError::MissingCap("CAP_NET_ADMIN"))
        } else {
            Ok(())
        }
    }
}
