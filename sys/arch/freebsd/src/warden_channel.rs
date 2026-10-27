//! Private AF_LOCAL/SOCK_SEQPACKET capabilities for Warden. No socket path,
//! bind/listen, ambient service discovery, or automatic privilege escalation.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

pub const MAX_PACKET: usize = 48 * 1024;
const MAX_RIGHTS: usize = 8;

pub struct Channel(OwnedFd);

pub struct Packet {
    pub bytes: Vec<u8>,
    pub descriptors: Vec<OwnedFd>,
}

impl Channel {
    pub fn pair() -> io::Result<(Self, Self)> {
        let mut fds = [-1; 2];
        if unsafe {
            libc::socketpair(
                libc::AF_LOCAL,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // Both freshly allocated descriptors are uniquely owned here.
        Ok(unsafe {
            (
                Self(OwnedFd::from_raw_fd(fds[0])),
                Self(OwnedFd::from_raw_fd(fds[1])),
            )
        })
    }

    pub fn from_owned(fd: OwnedFd) -> io::Result<Self> {
        let channel = Self(fd);
        channel.validate()?;
        Ok(channel)
    }

    pub fn into_owned(self) -> OwnedFd {
        self.0
    }

    pub fn validate(&self) -> io::Result<()> {
        let mut kind: libc::c_int = 0;
        let mut size = std::mem::size_of_val(&kind) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                self.0.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                std::ptr::addr_of_mut!(kind).cast(),
                &mut size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut address = std::mem::MaybeUninit::<libc::sockaddr_storage>::zeroed();
        let mut address_size = std::mem::size_of_val(&address) as libc::socklen_t;
        if unsafe {
            libc::getsockname(
                self.0.as_raw_fd(),
                address.as_mut_ptr().cast(),
                &mut address_size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if size as usize != std::mem::size_of_val(&kind)
            || kind != libc::SOCK_SEQPACKET
            || unsafe { address.assume_init() }.ss_family as i32 != libc::AF_LOCAL
        {
            return Err(invalid(
                "Warden requires an AF_LOCAL sequence-packet channel",
            ));
        }
        Ok(())
    }

    /// The root launcher created both endpoints before dropping the harness
    /// identity. These credentials authenticate that bootstrap, not the UID
    /// of whichever process later inherits a capability.
    pub fn require_root_peer(&self) -> io::Result<()> {
        self.validate()?;
        let mut credentials = std::mem::MaybeUninit::<libc::xucred>::zeroed();
        let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                self.0.as_raw_fd(),
                0,
                libc::LOCAL_PEERCRED,
                credentials.as_mut_ptr().cast(),
                &mut size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if size as usize != std::mem::size_of::<libc::xucred>() {
            return Err(invalid("invalid Warden peer credential size"));
        }
        let credentials = unsafe { credentials.assume_init() };
        if credentials.cr_version != 0 || credentials.cr_uid != 0 {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        Ok(())
    }

    pub fn send(&self, bytes: &[u8], descriptors: &[BorrowedFd<'_>]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > MAX_PACKET || descriptors.len() > MAX_RIGHTS {
            return Err(invalid("invalid Warden packet size or descriptor count"));
        }
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr().cast_mut().cast(),
            iov_len: bytes.len(),
        };
        let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
        header.msg_iov = std::ptr::addr_of_mut!(iov);
        header.msg_iovlen = 1;
        // usize storage provides cmsghdr alignment. Lengths remain byte counts.
        let mut control = vec![0usize; control_words(descriptors.len())];
        if !descriptors.is_empty() {
            header.msg_control = control.as_mut_ptr().cast();
            header.msg_controllen = control_bytes(descriptors.len()) as _;
            let cmsg = unsafe { libc::CMSG_FIRSTHDR(&header) };
            if cmsg.is_null() {
                return Err(invalid("invalid ancillary buffer"));
            }
            unsafe {
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len =
                    libc::CMSG_LEN((descriptors.len() * std::mem::size_of::<RawFd>()) as u32);
                let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                for (index, fd) in descriptors.iter().enumerate() {
                    data.add(index).write_unaligned(fd.as_raw_fd());
                }
            }
        }
        loop {
            // FreeBSD local seqpacket requires explicit EOR; without it,
            // separate sendmsg calls may be received as one record.
            let count = unsafe {
                libc::sendmsg(
                    self.0.as_raw_fd(),
                    &header,
                    libc::MSG_NOSIGNAL | libc::MSG_EOR,
                )
            };
            if count >= 0 {
                return if count as usize == bytes.len() {
                    Ok(())
                } else {
                    Err(invalid("partial sequence packet"))
                };
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub fn receive(&self) -> io::Result<Option<Packet>> {
        let mut bytes = vec![0; MAX_PACKET];
        let mut control = vec![0usize; control_words(MAX_RIGHTS)];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
        header.msg_iov = std::ptr::addr_of_mut!(iov);
        header.msg_iovlen = 1;
        header.msg_control = control.as_mut_ptr().cast();
        header.msg_controllen = control_bytes(MAX_RIGHTS) as _;
        let count = loop {
            let count =
                unsafe { libc::recvmsg(self.0.as_raw_fd(), &mut header, libc::MSG_CMSG_CLOEXEC) };
            if count >= 0 {
                break count as usize;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        // Adopt ALL delivered rights before returning an error, including
        // truncation. The kernel closes rights which did not fit in control.
        let mut descriptors = Vec::new();
        let mut malformed = false;
        let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&header) };
        while !cmsg.is_null() {
            let fixed = unsafe { libc::CMSG_LEN(0) } as usize;
            let length = unsafe { (*cmsg).cmsg_len } as usize;
            let offset = cmsg as usize - control.as_ptr() as usize;
            if length < fixed || length > (header.msg_controllen as usize).saturating_sub(offset) {
                return Err(invalid("invalid ancillary header"));
            }
            if unsafe {
                (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS
            } {
                let payload = length - fixed;
                malformed |= !payload.is_multiple_of(std::mem::size_of::<RawFd>());
                let data = unsafe { libc::CMSG_DATA(cmsg).cast::<RawFd>() };
                for index in 0..payload / std::mem::size_of::<RawFd>() {
                    let fd = unsafe { data.add(index).read_unaligned() };
                    descriptors.push(unsafe { OwnedFd::from_raw_fd(fd) });
                }
            } else {
                malformed = true;
            }
            cmsg = unsafe { libc::CMSG_NXTHDR(&header, cmsg) };
        }
        if malformed
            || header.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
            || (count != 0 && header.msg_flags & libc::MSG_EOR == 0)
        {
            return Err(invalid("truncated or invalid Warden packet"));
        }
        bytes.truncate(count);
        if count == 0 {
            return if descriptors.is_empty() {
                Ok(None)
            } else {
                Err(invalid("empty packet with authority"))
            };
        }
        Ok(Some(Packet { bytes, descriptors }))
    }
}

impl AsFd for Channel {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

fn control_bytes(count: usize) -> usize {
    unsafe { libc::CMSG_SPACE((count * std::mem::size_of::<RawFd>()) as u32) as usize }
}
fn control_words(count: usize) -> usize {
    control_bytes(count).div_ceil(std::mem::size_of::<usize>())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
