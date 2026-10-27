use super::*;
use std::io::{Read, Write};

#[test]
fn sequence_packets_are_atomic_and_received_authority_is_cloexec() {
    let (sender, receiver) = Channel::pair().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    sender.send(b"first", &[file.as_fd()]).unwrap();
    sender.send(b"second", &[]).unwrap();
    let packet = receiver.receive().unwrap().unwrap();
    assert_eq!(packet.bytes, b"first");
    assert_eq!(packet.descriptors.len(), 1);
    let flags = unsafe { libc::fcntl(packet.descriptors[0].as_raw_fd(), libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    assert_eq!(receiver.receive().unwrap().unwrap().bytes, b"second");
    drop(sender);
    assert!(receiver.receive().unwrap().is_none());
}

#[test]
fn channel_rejects_files_stream_sockets_and_nonroot_bootstrap() {
    let file = std::fs::File::open("/dev/null").unwrap();
    assert!(Channel::from_owned(file.into()).is_err());
    let (socket, _) = std::os::unix::net::UnixStream::pair().unwrap();
    assert!(Channel::from_owned(socket.into()).is_err());
    let (sender, _) = Channel::pair().unwrap();
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(
            sender.require_root_peer().unwrap_err().raw_os_error(),
            Some(libc::EPERM)
        );
    } else {
        sender.require_root_peer().unwrap();
    }
}

#[test]
fn private_pipe_capability_transfers_without_exposing_other_descriptors() {
    let (sender, receiver) = Channel::pair().unwrap();
    let mut raw = [-1; 2];
    assert_eq!(unsafe { libc::pipe2(raw.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
    let mut reader = unsafe { std::fs::File::from_raw_fd(raw[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(raw[1]) };
    sender.send(b"pipe", &[writer.as_fd()]).unwrap();
    drop(writer);
    let mut packet = receiver.receive().unwrap().unwrap();
    let mut writer = std::fs::File::from(packet.descriptors.pop().unwrap());
    writer.write_all(b"bounded capability").unwrap();
    drop(writer);
    let mut output = String::new();
    reader.read_to_string(&mut output).unwrap();
    assert_eq!(output, "bounded capability");
}

#[test]
fn empty_oversized_and_excess_authority_packets_are_refused_before_send() {
    let (sender, receiver) = Channel::pair().unwrap();
    assert!(sender.send(&[], &[]).is_err());
    assert!(sender.send(&vec![0; MAX_PACKET + 1], &[]).is_err());
    let file = std::fs::File::open("/dev/null").unwrap();
    assert!(
        sender
            .send(b"bad", &[file.as_fd(); MAX_RIGHTS + 1])
            .is_err()
    );
    // No malformed packet was emitted.
    sender.send(b"valid", &[]).unwrap();
    assert_eq!(receiver.receive().unwrap().unwrap().bytes, b"valid");
}

#[test]
fn receiver_rejects_truncation_and_closes_delivered_rights() {
    let (sender, receiver) = Channel::pair().unwrap();
    let mut raw = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(raw.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
        0
    );
    let mut reader = unsafe { std::fs::File::from_raw_fd(raw[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(raw[1]) };
    let mut data = vec![0_u8; MAX_PACKET + 1];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    let mut control = vec![0usize; control_words(1)];
    header.msg_iov = std::ptr::addr_of_mut!(iov);
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = control_bytes(1) as _;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32);
        libc::CMSG_DATA(cmsg)
            .cast::<RawFd>()
            .write_unaligned(writer.as_raw_fd());
        assert_eq!(
            libc::sendmsg(
                sender.as_fd().as_raw_fd(),
                &header,
                libc::MSG_NOSIGNAL | libc::MSG_EOR
            ),
            data.len() as isize
        );
    }
    drop(writer);
    assert!(receiver.receive().is_err());
    assert_eq!(
        reader.read(&mut [0]).unwrap(),
        0,
        "truncated packet leaked a writer FD"
    );
}

#[test]
fn receiver_refuses_records_without_a_freebsd_end_marker() {
    let (sender, receiver) = Channel::pair().unwrap();
    let bytes = b"unterminated";
    assert_eq!(
        unsafe {
            libc::send(
                sender.as_fd().as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                libc::MSG_NOSIGNAL,
            )
        },
        bytes.len() as isize
    );
    assert!(receiver.receive().is_err());
}

#[test]
fn ancillary_truncation_closes_every_delivered_pipe_reference() {
    let (sender, receiver) = Channel::pair().unwrap();
    let mut raw = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(raw.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
        0
    );
    let mut reader = unsafe { std::fs::File::from_raw_fd(raw[0]) };
    let writer = unsafe { OwnedFd::from_raw_fd(raw[1]) };
    let count = MAX_RIGHTS + 4;
    let mut control = vec![0usize; control_words(count)];
    let mut byte = 1u8;
    let mut iov = libc::iovec {
        iov_base: std::ptr::addr_of_mut!(byte).cast(),
        iov_len: 1,
    };
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = std::ptr::addr_of_mut!(iov);
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = control_bytes(count) as _;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN((count * std::mem::size_of::<RawFd>()) as u32);
        let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
        for index in 0..count {
            data.add(index).write_unaligned(writer.as_raw_fd());
        }
        assert_eq!(
            libc::sendmsg(
                sender.as_fd().as_raw_fd(),
                &header,
                libc::MSG_EOR | libc::MSG_NOSIGNAL
            ),
            1
        );
    }
    drop(writer);
    assert!(receiver.receive().is_err());
    assert_eq!(
        reader.read(&mut [0]).unwrap(),
        0,
        "truncated rights leaked authority"
    );
}
