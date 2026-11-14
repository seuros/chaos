//! Activity from every Chaos process of one home, gathered by its residents.
//!
//! A resident (`chaos serve`) listens on `run/activity/<pid>.sock` under the
//! Chaos home. Every other Chaos process forwards its bus to each resident it
//! finds there, as JSON lines: one `Hello` naming the process, then events.
//! The directory is private to the user, and only what the bus already
//! carries ever leaves a process. A process with no resident to talk to does
//! not subscribe to its bus at all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};

use super::ActivityEvent;

const PROTOCOL: u32 = 1;
const RESCAN: Duration = Duration::from_secs(2);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LINE: usize = 64 * 1024;
const MAX_PEERS: usize = 64;

/// The kind of Chaos process an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    Console,
    Exec,
    Mcp,
    Acp,
    Taskd,
}

/// The process an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Origin {
    pub kind: OriginKind,
    pub pid: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct Hello {
    protocol: u32,
    origin: Origin,
}

/// What a resident hears from the other processes of its home.
#[derive(Debug, Clone)]
pub enum FleetEvent {
    Joined(Origin),
    Activity(Origin, ActivityEvent),
    Left(Origin),
}

/// Where the residents of a Chaos home listen.
pub fn runtime_dir(chaos_home: &Path) -> PathBuf {
    chaos_home.join("run").join("activity")
}

/// Forward this process's activity to every resident of `chaos_home`, for as
/// long as the process lives.
pub fn forward(chaos_home: &Path, kind: OriginKind) {
    let dir = runtime_dir(chaos_home);
    let origin = Origin {
        kind,
        pid: std::process::id(),
    };
    tokio::spawn(async move {
        let mut peers: HashMap<PathBuf, UnixStream> = HashMap::new();
        let mut bus: Option<broadcast::Receiver<ActivityEvent>> = None;
        let mut rescan = tokio::time::interval(RESCAN);
        loop {
            tokio::select! {
                _ = rescan.tick() => {
                    connect_new(&dir, &origin, &mut peers).await;
                    // Subscribe only while someone listens: publishing stays
                    // free for a process with no resident to talk to.
                    match (peers.is_empty(), bus.is_some()) {
                        (false, false) => bus = Some(super::subscribe()),
                        (true, true) => bus = None,
                        _ => {}
                    }
                }
                event = async { bus.as_mut()?.recv().await.ok() }, if bus.is_some() => {
                    let Some(event) = event else { continue };
                    let Ok(mut line) = serde_json::to_vec(&event) else { continue };
                    line.push(b'\n');
                    let mut gone = Vec::new();
                    for (path, stream) in &mut peers {
                        if stream.write_all(&line).await.is_err() {
                            gone.push(path.clone());
                        }
                    }
                    for path in gone {
                        peers.remove(&path);
                    }
                }
            }
        }
    });
}

async fn connect_new(dir: &Path, origin: &Origin, peers: &mut HashMap<PathBuf, UnixStream>) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        peers.clear();
        return;
    };
    let mut present = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "sock") {
            present.push(path);
        }
    }
    peers.retain(|path, _| present.contains(path));
    for path in present {
        if peers.contains_key(&path) {
            continue;
        }
        let Ok(mut stream) = UnixStream::connect(&path).await else {
            continue;
        };
        let hello = Hello {
            protocol: PROTOCOL,
            origin: *origin,
        };
        let Ok(mut line) = serde_json::to_vec(&hello) else {
            continue;
        };
        line.push(b'\n');
        if stream.write_all(&line).await.is_ok() {
            peers.insert(path, stream);
        }
    }
}

/// A resident's socket; removed when dropped.
pub struct Listening {
    path: PathBuf,
}

impl Drop for Listening {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Listen for the other processes of `chaos_home`. Events arrive on the
/// returned channel until the `Listening` guard is dropped.
pub async fn listen(chaos_home: &Path) -> std::io::Result<(Listening, mpsc::Receiver<FleetEvent>)> {
    use std::os::unix::fs::PermissionsExt;
    let dir = runtime_dir(chaos_home);
    tokio::fs::create_dir_all(&dir).await?;
    tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
    remove_stale(&dir).await;
    let path = dir.join(format!("{}.sock", std::process::id()));
    let _ = tokio::fs::remove_file(&path).await;
    let listener = UnixListener::bind(&path)?;
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
    let (tx, rx) = mpsc::channel(1024);
    let peers = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_PEERS));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let Ok(permit) = peers.clone().try_acquire_owned() else {
                continue;
            };
            if tx.is_closed() {
                break;
            }
            let tx = tx.clone();
            tokio::spawn(async move {
                let _permit = permit;
                receive(stream, tx).await;
            });
        }
    });
    Ok((Listening { path }, rx))
}

async fn receive(stream: UnixStream, tx: mpsc::Sender<FleetEvent>) {
    let mut lines = BufReader::new(stream).take_lines();
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, lines.next()).await {
        Ok(Some(line)) => serde_json::from_slice::<Hello>(&line).ok(),
        _ => None,
    };
    let Some(hello) = hello.filter(|hello| hello.protocol == PROTOCOL) else {
        return;
    };
    let origin = hello.origin;
    if tx.send(FleetEvent::Joined(origin)).await.is_err() {
        return;
    }
    while let Some(line) = lines.next().await {
        let Ok(event) = serde_json::from_slice::<ActivityEvent>(&line) else {
            continue;
        };
        if tx.send(FleetEvent::Activity(origin, event)).await.is_err() {
            return;
        }
    }
    let _ = tx.send(FleetEvent::Left(origin)).await;
}

/// Sockets left by residents that died without cleaning up refuse
/// connections; nothing will ever answer them.
async fn remove_stale(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "sock")
            && UnixStream::connect(&path).await.is_err()
        {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

/// Newline-delimited lines with a hard length cap, so a peer cannot make a
/// resident buffer without bound.
struct Lines<R> {
    reader: R,
}

trait TakeLines: Sized {
    fn take_lines(self) -> Lines<Self>;
}

impl<R: AsyncBufReadExt + Unpin> TakeLines for R {
    fn take_lines(self) -> Lines<Self> {
        Lines { reader: self }
    }
}

impl<R: AsyncBufReadExt + Unpin> Lines<R> {
    async fn next(&mut self) -> Option<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            let available = self.reader.fill_buf().await.ok()?;
            if available.is_empty() {
                return None;
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(end) => {
                    line.extend_from_slice(&available[..end]);
                    self.reader.consume(end + 1);
                    return (line.len() <= MAX_LINE).then_some(line);
                }
                None => {
                    let taken = available.len();
                    line.extend_from_slice(available);
                    self.reader.consume(taken);
                    if line.len() > MAX_LINE {
                        return None;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
