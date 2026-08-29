//! The virtual interface. Linux-only in this prototype; every other platform
//! reports the capability as unsupported rather than pretending.
//!
//! The Linux path is a raw `TUNSETIFF` ioctl against `/dev/net/tun` — no
//! third-party driver, no signing, just `CAP_NET_ADMIN`. That is exactly why
//! Linux is the right place to prove the packet layer first: no gatekeeper.
//! macOS (utun + a Network Extension entitlement) and Windows (WinTun + a
//! privileged service) fold in behind this same seam later.

use std::io;
use std::net::Ipv6Addr;

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::File;
    use std::io;
    use std::net::Ipv6Addr;
    use std::os::unix::io::FromRawFd;
    use std::process::Command;

    // `_IOW('T', 202, int)` — architecture-independent.
    const TUNSETIFF: u64 = 0x4004_54ca;
    const IFF_TUN: u16 = 0x0001;
    const IFF_NO_PI: u16 = 0x1000;
    const IFNAMSIZ: usize = 16;
    // `sizeof(struct ifreq)` on Linux: 16-byte name + a 24-byte union.
    const IFREQ_LEN: usize = 40;

    /// Open `/dev/net/tun` and bind it to a TUN interface named `requested`.
    /// Returns the packet file and the kernel's actual interface name.
    pub fn open(requested: &str) -> io::Result<(File, String)> {
        let name = requested.as_bytes();
        if name.is_empty() || name.len() >= IFNAMSIZ {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "interface name must be 1..=15 bytes",
            ));
        }
        // SAFETY: a constant NUL-terminated path and O_RDWR; the fd is checked.
        let fd = unsafe { libc::open(c"/dev/net/tun".as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut ifr = [0u8; IFREQ_LEN];
        ifr[..name.len()].copy_from_slice(name);
        ifr[IFNAMSIZ..IFNAMSIZ + 2].copy_from_slice(&(IFF_TUN | IFF_NO_PI).to_ne_bytes());
        // SAFETY: `ifr` is exactly sizeof(ifreq); the kernel reads the name and
        // flags and writes the resolved name back into the same buffer.
        // `ioctl`'s request arg is c_ulong on glibc and c_int on musl; `as _`
        // resolves to whichever the target libc wants (the value fits both).
        let rc = unsafe { libc::ioctl(fd, TUNSETIFF as _, ifr.as_mut_ptr()) };
        if rc < 0 {
            let error = io::Error::last_os_error();
            // SAFETY: closing the fd we just opened and are about to drop.
            unsafe { libc::close(fd) };
            return Err(error);
        }
        let end = ifr[..IFNAMSIZ]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(IFNAMSIZ);
        let actual = String::from_utf8_lossy(&ifr[..end]).into_owned();
        // SAFETY: `fd` is a valid, owned descriptor and is not used again.
        let file = unsafe { File::from_raw_fd(fd) };
        Ok((file, actual))
    }

    fn run(args: &[&str]) -> io::Result<()> {
        let status = Command::new("ip").args(args).status()?;
        if !status.success() {
            return Err(io::Error::other(format!("`ip {}` failed", args.join(" "))));
        }
        Ok(())
    }

    /// Assign `address` to `dev`, bring it up, and route each peer's address
    /// into it. Uses iproute2 (`ip`) and therefore needs `CAP_NET_ADMIN`.
    pub fn configure(dev: &str, address: Ipv6Addr, peers: &[Ipv6Addr]) -> io::Result<()> {
        run(&["-6", "addr", "add", &format!("{address}/128"), "dev", dev])?;
        run(&["link", "set", dev, "up"])?;
        for peer in peers {
            run(&["-6", "route", "add", &format!("{peer}/128"), "dev", dev])?;
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::fs::File;
    use std::io;
    use std::net::Ipv6Addr;

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "lait-net's TUN interface is Linux-only in this prototype",
        ))
    }

    pub fn open(_requested: &str) -> io::Result<(File, String)> {
        unsupported()
    }

    pub fn configure(_dev: &str, _address: Ipv6Addr, _peers: &[Ipv6Addr]) -> io::Result<()> {
        unsupported()
    }
}

/// Open a TUN interface named `requested`; returns the packet file and the
/// interface's actual name.
pub fn open(requested: &str) -> io::Result<(std::fs::File, String)> {
    imp::open(requested)
}

/// Assign `address`, bring `dev` up, and route each peer address into it.
pub fn configure(dev: &str, address: Ipv6Addr, peers: &[Ipv6Addr]) -> io::Result<()> {
    imp::configure(dev, address, peers)
}
