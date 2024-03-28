use libc::{__c_anonymous_ifr_ifru, c_char, c_int, c_short, ifreq, memcpy};
use nix::sys::ioctl;
use nix::sys::ioctl::ioctl_param_type;
use nix::unistd::close;
use nix::{
    ioctl_read, ioctl_readwrite, ioctl_write_buf, ioctl_write_int, ioctl_write_ptr, NixPath,
};
use std::ffi::{c_void, CStr, CString, FromBytesUntilNulError};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use thiserror::Error;
use users::get_current_uid;

const IFNAMSIZ: usize = 16;
const IFF_TAP: c_short = 2;
const IFF_TUN: c_short = 1;
const IFF_NO_PI: c_short = 0x1000;
const IFF_NAPI: c_short = 0x0010;

const TUN_IOC_MAGIC: u8 = b'T';
const TUN_IOC_SET_IFF: u8 = 202;
const TUN_IOC_SET_PERSIST: u8 = 203;
const TUN_IOC_SET_OWNER: u8 = 204;
ioctl_write_ptr!(tun_set_iff, TUN_IOC_MAGIC, TUN_IOC_SET_IFF, c_int);
ioctl_write_int!(tun_set_persist, TUN_IOC_MAGIC, TUN_IOC_SET_PERSIST);
ioctl_write_int!(tun_set_owner, TUN_IOC_MAGIC, TUN_IOC_SET_OWNER);

pub(crate) struct Tap {
    name: String,
    fd: RawFd,
}

impl Drop for Tap {
    fn drop(&mut self) {
        close(self.fd).expect("Could not close tap device");
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
}

type Result<T> = core::result::Result<T, UserTapError>;

impl Tap {
    pub async fn new(name: &str) -> Result<Self> {
        Self::check_caps()?;
        let device = Self::get_tun_device().await?.into_raw_fd();
        println!("fd: {}", device);

        let mut req = unsafe { MaybeUninit::<ifreq>::zeroed().assume_init() };
        let mut name_bytes_iter = name.as_bytes().iter();
        req.ifr_name[0..name.len()].fill_with(|| *(name_bytes_iter.next().unwrap()) as c_char);
        req.ifr_ifru.ifru_flags = IFF_TAP;

        let result = unsafe { tun_set_iff(device, &req as *const ifreq as *const c_int) }
            .map_err(UserTapError::CouldNotCreateTap)?;
        if result < 0 {
            panic!("Something fishy {}", result);
        }

        let name = unsafe { CStr::from_ptr(req.ifr_name.as_ptr()) }
            .to_str()
            .unwrap()
            .to_string();
        let current_user = get_current_uid();
        unsafe { tun_set_owner(device, current_user as ioctl_param_type) }.unwrap();

        Ok(Self {
            name: name.to_string(),
            fd: device,
        })
    }

    async fn get_tun_device() -> Result<std::fs::File> {
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
