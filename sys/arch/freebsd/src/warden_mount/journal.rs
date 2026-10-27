//! Warden's durable mount-ownership journal.
use super::{MountId, MountRecord};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum Event {
    Header {
        version: u32,
        root: PathBuf,
    },
    Before {
        filesystem_type: String,
        target: PathBuf,
        prior_ids: Vec<MountId>,
    },
    Mounted {
        record: MountRecord,
    },
    MountFailed,
    Unmounted {
        id: MountId,
    },
}

pub(super) struct Journal {
    file: File,
    healthy: bool,
}

impl Journal {
    pub fn create(root: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join("mounts.jsonl"))?;
        let mut journal = Self {
            file,
            healthy: true,
        };
        journal.append(&Event::Header {
            version: 1,
            root: root.to_owned(),
        })?;
        File::open(root)?.sync_all()?;
        Ok(journal)
    }

    pub fn append(&mut self, event: &Event) -> io::Result<()> {
        if !self.healthy {
            return Err(io::Error::other(
                "mount journal requires recovery after a write failure",
            ));
        }
        let result = (|| {
            let mut bytes = serde_json::to_vec(event).map_err(io::Error::other)?;
            bytes.push(b'\n');
            if self
                .file
                .metadata()?
                .len()
                .saturating_add(bytes.len() as u64)
                > MAX_JOURNAL_BYTES
            {
                return Err(io::Error::other("mount journal size limit exceeded"));
            }
            self.file.write_all(&bytes)?;
            self.file.sync_all()
        })();
        // A later append must never turn a partial JSON tail into a complete
        // corrupt record or write Unmounted after an unrecorded Mounted event.
        if result.is_err() {
            self.healthy = false;
        }
        result
    }
}

pub(super) struct Replay {
    pub owned: Vec<MountRecord>,
    pub pending: Option<(String, Vec<MountId>, PathBuf)>,
}

pub(super) fn read(root: &Path) -> io::Result<Replay> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("mounts.jsonl"))?;
    let metadata = file.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o077 != 0 || metadata.len() > MAX_JOURNAL_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "untrusted mount journal",
        ));
    }
    replay(root, BufReader::new(file))
}

fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid mount ownership journal",
    )
}

pub(super) fn replay(root: &Path, mut reader: impl BufRead) -> io::Result<Replay> {
    let mut line = Vec::new();
    let mut header = false;
    let mut state = Replay {
        owned: Vec::new(),
        pending: None,
    };
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.last() != Some(&b'\n') {
            // A partial last append cannot authorize a new syscall: the
            // caller syncs a complete Before line before invoking nmount.
            break;
        }
        let event: Event = serde_json::from_slice(&line).map_err(|_| bad())?;
        if !header {
            match event {
                Event::Header {
                    version: 1,
                    root: saved,
                } if saved == root => header = true,
                _ => return Err(bad()),
            }
            continue;
        }
        match event {
            Event::Header { .. } => return Err(bad()),
            Event::Before {
                filesystem_type,
                target,
                prior_ids,
            } => {
                if state.pending.is_some() || !scoped_path(root, &target) {
                    return Err(bad());
                }
                state.pending = Some((filesystem_type, prior_ids, target));
            }
            Event::Mounted { record } => {
                let Some((kind, prior, expected)) = state.pending.take() else {
                    return Err(bad());
                };
                if record.filesystem_type != kind
                    || !scoped_path(root, &record.target)
                    || record.target != expected
                    || prior.contains(&record.id)
                    || state.owned.iter().any(|m| m.id == record.id)
                {
                    return Err(bad());
                }
                state.owned.push(record);
            }
            Event::MountFailed => {
                if state.pending.take().is_none() {
                    return Err(bad());
                }
            }
            Event::Unmounted { id } => {
                if state.owned.last().map(|m| m.id) != Some(id) {
                    return Err(bad());
                }
                state.owned.pop();
            }
        }
    }
    if !header {
        return Err(bad());
    }
    Ok(state)
}

pub(super) fn scoped_path(root: &Path, target: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    target != root
        && target.starts_with(root)
        && !target.as_os_str().as_bytes().contains(&0)
        && target
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}
