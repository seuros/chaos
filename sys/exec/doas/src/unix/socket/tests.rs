use super::*;
use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde::Serialize;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use tempfile::NamedTempFile;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
struct TestPayload {
    id: i32,
    label: String,
}

fn fd_list(count: usize) -> std::io::Result<Vec<OwnedFd>> {
    let file = NamedTempFile::new()?;
    let mut fds = Vec::new();
    for _ in 0..count {
        fds.push(file.as_fd().try_clone_to_owned()?);
    }
    Ok(fds)
}

#[tokio::test]
async fn async_socket_round_trips_payload_and_fds() -> std::io::Result<()> {
    let (server, client) = AsyncSocket::pair()?;
    let payload = TestPayload {
        id: 7,
        label: "round-trip".to_string(),
    };
    let send_fds = fd_list(1)?;

    let receive_task = tokio::spawn(async move { server.receive_with_fds::<TestPayload>().await });
    client.send_with_fds(payload.clone(), &send_fds).await?;
    drop(send_fds);

    let (received_payload, received_fds) = receive_task.await.unwrap()?;
    assert_eq!(payload, received_payload);
    assert_eq!(1, received_fds.len());
    let fd_status = unsafe { libc::fcntl(received_fds[0].as_raw_fd(), libc::F_GETFD) };
    assert!(
        fd_status >= 0,
        "expected received file descriptor to be valid, but got {fd_status}",
    );
    Ok(())
}

#[tokio::test]
async fn async_socket_handles_large_payload() -> std::io::Result<()> {
    let (server, client) = AsyncSocket::pair()?;
    let payload = vec![b'A'; 10_000];
    let receive_task = tokio::spawn(async move { server.receive::<Vec<u8>>().await });
    client.send(payload.clone()).await?;
    let received_payload = receive_task.await.unwrap()?;
    assert_eq!(payload, received_payload);
    Ok(())
}

#[tokio::test]
async fn async_datagram_sockets_round_trip_messages() -> std::io::Result<()> {
    let (server, client) = AsyncDatagramSocket::pair()?;
    let data = b"datagram payload".to_vec();
    let send_fds = fd_list(1)?;
    let receive_task = tokio::spawn(async move { server.receive_with_fds().await });

    client.send_with_fds(&data, &send_fds).await?;
    drop(send_fds);

    let (received_bytes, received_fds) = receive_task.await.unwrap()?;
    assert_eq!(data, received_bytes);
    assert_eq!(1, received_fds.len());
    Ok(())
}

#[test]
fn send_datagram_bytes_rejects_excessive_fd_counts() -> std::io::Result<()> {
    let (socket, _peer) = Socket::pair_raw(Domain::UNIX, Type::DGRAM, None)?;
    let fds = fd_list(MAX_FDS_PER_MESSAGE + 1)?;
    let err = send_datagram_bytes(&socket, b"hi", &fds).unwrap_err();
    assert_eq!(std::io::ErrorKind::InvalidInput, err.kind());
    Ok(())
}

#[test]
fn send_stream_chunk_rejects_excessive_fd_counts() -> std::io::Result<()> {
    let (socket, _peer) = Socket::pair_raw(Domain::UNIX, Type::STREAM, None)?;
    let fds = fd_list(MAX_FDS_PER_MESSAGE + 1)?;
    let err = send_stream_chunk(&socket, b"hello", &fds, true).unwrap_err();
    assert_eq!(std::io::ErrorKind::InvalidInput, err.kind());
    Ok(())
}

#[test]
fn encode_length_errors_for_oversized_messages() {
    let err = encode_length(usize::MAX).unwrap_err();
    assert_eq!(std::io::ErrorKind::InvalidInput, err.kind());
}

#[tokio::test]
async fn receive_fails_when_peer_closes_before_header() {
    let (server, client) = AsyncSocket::pair().expect("failed to create socket pair");
    drop(client);
    let err = server
        .receive::<serde_json::Value>()
        .await
        .expect_err("expected read failure");
    assert_eq!(std::io::ErrorKind::UnexpectedEof, err.kind());
}
