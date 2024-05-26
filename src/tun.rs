use std::{fs::File, os::fd::AsRawFd};

use anyhow::Result;

pub struct TunDevice {
    fd: File,
}


#[repr(C)]
struct Ifreq {
    pub ifrn_name: [std::ffi::c_uchar; 16],
    pub ifru_flags: std::ffi::c_short,
}

nix::ioctl_write_int!(tun_set_iff, b'T', 202);

impl TunDevice {
    pub fn create<S: AsRef<str>>(ifname: S) -> Result<TunDevice> {
        let fd = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .open("/dev/net/tun")?;

        let mut ifreq = Ifreq {
            ifrn_name: std::default::Default::default(),
            ifru_flags: 0x1001,  // IFF_TUN | IFF_NO_PI
        };

        let ifname_bytes = ifname.as_ref().as_bytes();
        unsafe {
            nix::libc::memcpy(ifreq.ifrn_name.as_mut_ptr() as *mut std::ffi::c_void,
                              ifname_bytes.as_ptr() as *const std::ffi::c_void,
                              usize::min(16, ifname_bytes.len()));
            ifreq.ifrn_name[15] = 0;
            tun_set_iff(fd.as_raw_fd(), &ifreq as *const Ifreq as u64)?;
        }

        Ok(TunDevice {
            fd,
        })
    }
}

impl std::io::Read for &TunDevice {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        (&self.fd).read(buf)
    }
}

impl std::io::Write for &TunDevice {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&self.fd).write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (&self.fd).flush()
    }
}
