//! The virtual interface. Linux-only in this prototype; every other platform
//! reports the capability as unsupported rather than pretending.
//!
//! The Linux path is a raw `TUNSETIFF` ioctl against `/dev/net/tun` — no
//! third-party driver, no signing, just `CAP_NET_ADMIN`. That is exactly why
//! Linux is the right place to prove the packet layer first: no gatekeeper.
//! macOS (utun + a Network Extension entitlement) and Windows (WinTun + a
//! privileged service) fold in behind this same seam later.

use std::io::{Read, Write};
use std::net::Ipv6Addr;
use std::{fs::File, io};

use tokio::sync::watch;

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::net::Ipv6Addr;
    use std::os::unix::io::{AsRawFd, FromRawFd};
    use std::process::Command;

    use tokio::sync::watch;

    /// How long one wait for a packet lasts before the stop is looked at
    /// again. Short enough that a shutdown is not noticed late, long enough
    /// that an idle interface costs four wakeups a second.
    const POLL_MS: libc::c_int = 250;

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

    /// Assign `address` to `dev` and bring it up. Uses iproute2 (`ip`) and
    /// therefore needs `CAP_NET_ADMIN`. Peer routes are not taken here: they
    /// follow the own set as it changes, through `add_route`/`del_route`.
    pub fn configure(dev: &str, address: Ipv6Addr) -> io::Result<()> {
        run(&["-6", "addr", "add", &format!("{address}/128"), "dev", dev])?;
        run(&["link", "set", dev, "up"])
    }

    pub fn add_route(dev: &str, ula: Ipv6Addr) -> io::Result<()> {
        run(&["-6", "route", "add", &format!("{ula}/128"), "dev", dev])
    }

    pub fn del_route(dev: &str, ula: Ipv6Addr) -> io::Result<()> {
        run(&["-6", "route", "del", &format!("{ula}/128"), "dev", dev])
    }

    /// A packet source that ends when the plane it belongs to does.
    ///
    /// A plain blocking read of a TUN never returns on its own — an idle
    /// interface simply has nothing to say — so the thread the packet loop
    /// runs on would still be inside `read` when the daemon's runtime is
    /// dropped, and dropping a runtime *waits* for its blocking threads. The
    /// stop would hang the process rather than end it. So the wait is a
    /// bounded `poll` and the stop is answered as end-of-file, which is the
    /// one thing the packet loop reads as "the source is gone".
    struct Stopping {
        file: File,
        stop: watch::Receiver<bool>,
    }

    impl Read for Stopping {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            loop {
                if *self.stop.borrow() {
                    return Ok(0);
                }
                let mut waiting = libc::pollfd {
                    fd: self.file.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one initialised `pollfd` over a descriptor this
                // struct owns, with a length of exactly one.
                let ready = unsafe { libc::poll(&raw mut waiting, 1, POLL_MS) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if ready == 0 {
                    continue;
                }
                return self.file.read(buf);
            }
        }
    }

    pub fn packets(
        file: File,
        stop: watch::Receiver<bool>,
    ) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        let write = file.try_clone()?;
        Ok((Box::new(Stopping { file, stop }), Box::new(write)))
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::net::Ipv6Addr;

    use tokio::sync::watch;

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "lait-net's TUN interface is Linux-only in this prototype",
        ))
    }

    pub fn open(_requested: &str) -> io::Result<(File, String)> {
        unsupported()
    }

    pub fn configure(_dev: &str, _address: Ipv6Addr) -> io::Result<()> {
        unsupported()
    }

    pub fn add_route(_dev: &str, _ula: Ipv6Addr) -> io::Result<()> {
        unsupported()
    }

    pub fn del_route(_dev: &str, _ula: Ipv6Addr) -> io::Result<()> {
        unsupported()
    }

    pub fn packets(
        _file: File,
        _stop: watch::Receiver<bool>,
    ) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
        unsupported()
    }
}

/// Open a TUN interface named `requested`; returns the packet file and the
/// interface's actual name.
pub fn open(requested: &str) -> io::Result<(std::fs::File, String)> {
    imp::open(requested)
}

/// Assign `address` and bring `dev` up.
pub fn configure(dev: &str, address: Ipv6Addr) -> io::Result<()> {
    imp::configure(dev, address)
}

/// Route one peer's tunnel address into `dev`. Called as a device joins the
/// own set, so a route exists exactly as long as the device that owns it.
pub fn add_route(dev: &str, ula: Ipv6Addr) -> io::Result<()> {
    imp::add_route(dev, ula)
}

/// The inverse of [`add_route`], for a device retired from the set.
pub fn del_route(dev: &str, ula: Ipv6Addr) -> io::Result<()> {
    imp::del_route(dev, ula)
}

/// Split an open interface into the read and write halves the carry wants,
/// the reader ending when `stop` is raised.
///
/// The carry's packet loops are blocking, because a TUN read is; the stop has
/// to reach the reader through the descriptor rather than through a task,
/// since nothing cancels a thread parked in `read`. Handing back a source
/// that reports end-of-file on stop is what lets a mount be joined instead of
/// outliving the runtime that owns it.
pub fn packets(
    file: File,
    stop: watch::Receiver<bool>,
) -> io::Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    imp::packets(file, stop)
}
