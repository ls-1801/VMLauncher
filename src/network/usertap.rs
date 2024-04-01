use std::ffi::{CStr, FromBytesUntilNulError};
use std::os::fd::{AsRawFd, OwnedFd};

use libc::{c_int, c_short, ifreq, IFF_TAP};
use nix::sys::ioctl::ioctl_param_type;
use nix::sys::socket::{AddressFamily, SockFlag, SockType};
use thiserror::Error;
use tracing::{error, info};
use users::get_current_uid;

use crate::network::common::{
    create_ifreq, get_if_index, tun_set_iff, tun_set_owner, tun_set_persist, CommonError,
};

#[derive(Debug)]
pub(crate) struct Tap {
    pub(crate) name: String,
}

impl Drop for Tap {
    fn drop(&mut self) {
        info!("Dropping Tap: {}", self.name);
        if let Err(e) = Self::get_tun_device(&self.name).and_then(|f| {
            unsafe { tun_set_persist(f.as_raw_fd(), 0) }
                .map_err(|e| UserTapError::CouldNotGetIndex(e, "Unpersisting"))?;
            Ok(())
        }) {
            error!("Could not close tap device: {e:?}");
        }
    }
}

#[derive(Error, Debug)]
pub(crate) enum UserTapError {
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
    #[error("Could not create Tap Device. Error Code: {0}")]
    CouldNotCreateTap(nix::Error),
    #[error("When creating Tap Device: {0}")]
    CommonError(#[from] CommonError),
    #[error("Could not get the tap interfaces index, {1}: {0}")]
    CouldNotGetIndex(nix::Error, &'static str),
}

type Result<T> = core::result::Result<T, UserTapError>;

impl Tap {
    pub fn new(name: &str) -> Result<Self> {
        Self::check_caps()?;
        let device = OwnedFd::from(Self::get_tun_device(name)?);
        println!("fd: {}", device.as_raw_fd());

        let current_user = get_current_uid();
        unsafe { tun_set_owner(device.as_raw_fd(), current_user as ioctl_param_type) }.unwrap();
        unsafe { tun_set_persist(device.as_raw_fd(), 1) }
            .map_err(|e| UserTapError::CouldNotGetIndex(e, "Persisting"))?;

        Ok(Self {
            name: name.to_string(),
        })
    }

    pub(crate) fn get_index(&self) -> Result<c_int> {
        let mut req = create_ifreq(&self.name)?;
        let fd = nix::sys::socket::socket(
            AddressFamily::Unix,
            SockType::Datagram,
            SockFlag::SOCK_CLOEXEC,
            None,
        )
        .map_err(|e| UserTapError::CouldNotGetIndex(e, "Opening Socket"))?;

        unsafe { get_if_index(fd.as_raw_fd(), &mut req as *mut ifreq as *mut c_int) }
            .map_err(|e| UserTapError::CouldNotGetIndex(e, "Ioctl"))?;

        let index = unsafe { req.ifr_ifru.ifru_ifindex };
        Ok(index)
    }

    fn get_tun_device(name: &str) -> Result<std::fs::File> {
        let device_path = std::path::PathBuf::from("/dev/net/tun");

        if !device_path.exists() {
            return Err(UserTapError::DeviceDoesNotExist);
        }

        let device = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(device_path)
            .map_err(|e| UserTapError::IO(e, "opening /dev/net/tun"))?;

        let metadata = device
            .metadata()
            .map_err(|e| UserTapError::IO(e, "checking /dev/net/tun metadata"))?;

        if metadata.permissions().readonly() {
            return Err(UserTapError::DeviceNotAccessible);
        }

        let mut req = create_ifreq(name)?;
        req.ifr_ifru.ifru_flags = IFF_TAP as c_short;

        let _ = unsafe { tun_set_iff(device.as_raw_fd(), &req as *const ifreq as *const c_int) }
            .map_err(UserTapError::CouldNotCreateTap)?;

        Ok(device)
    }
    fn check_caps() -> Result<()> {
        use caps::{CapSet, Capability};
        if !caps::has_cap(None, CapSet::Effective, Capability::CAP_NET_ADMIN)
            .map_err(UserTapError::Caps)?
        {
            Err(UserTapError::MissingCap("CAP_NET_ADMIN"))
        } else {
            Ok(())
        }
    }
}
