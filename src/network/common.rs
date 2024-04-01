use crate::network::common::CommonError::NameToLong;
use libc::{c_char, c_int, c_long, c_short, IFNAMSIZ, ifreq};
use nix::{
    ioctl_readwrite_bad, ioctl_write_int, ioctl_write_int_bad, ioctl_write_ptr, ioctl_write_ptr_bad,
};
use std::mem::MaybeUninit;
use thiserror::Error;


const TUN_IOC_MAGIC: u8 = b'T';
const TUN_IOC_SET_IFF: u8 = 202;
const TUN_IOC_SET_PERSIST: u8 = 203;
const TUN_IOC_SET_OWNER: u8 = 204;
ioctl_write_ptr!(tun_set_iff, TUN_IOC_MAGIC, TUN_IOC_SET_IFF, c_int);
ioctl_write_int!(tun_set_persist, TUN_IOC_MAGIC, TUN_IOC_SET_PERSIST);
ioctl_write_int!(tun_set_owner, TUN_IOC_MAGIC, TUN_IOC_SET_OWNER);
ioctl_write_ptr_bad!(add_br, 0x89a0, c_long);
ioctl_write_ptr_bad!(del_br, 0x89a1, c_long);
ioctl_write_ptr_bad!(add_if, 0x89a2, c_int);
ioctl_readwrite_bad!(get_if_index, 0x8933, c_int);
ioctl_write_ptr_bad!(set_if_addr, 0x8916, c_int);
ioctl_write_ptr_bad!(set_if_flags, 0x8914, c_int);
ioctl_readwrite_bad!(get_if_flags, 0x8913, c_int);
#[derive(Error, Debug)]
pub enum CommonError {
    #[error("Name is to long")]
    NameToLong,
}
pub fn create_ifreq(name: &str) -> Result<ifreq, CommonError> {
    if name.len() > IFNAMSIZ - 1 {
        return Err(CommonError::NameToLong);
    }

    let mut req = unsafe { MaybeUninit::<ifreq>::zeroed().assume_init() };
    let mut name_bytes_iter = name.as_bytes().iter();
    req.ifr_name[0..name.len()].fill_with(|| *(name_bytes_iter.next().unwrap()) as c_char);

    Ok(req)
}
