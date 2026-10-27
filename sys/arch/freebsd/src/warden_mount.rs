//! Mount ownership for the privileged, single-threaded Warden worker.
//!
//! Targets and sources are pinned with descriptors. A private `fdescfs,nodup`
//! mount lets nmount resolve those descriptors to their actual vnodes rather
//! than reopening caller-controlled pathname strings. The private fdesc view
//! must never be exposed to commands.
//!
//! The journal is synced BEFORE nmount and after recording ownership. Cleanup
//! is by filesystem ID, in reverse order, without FORCE or path-based unmount.
//! Failure leaves a root-owned journal for explicit recovery.
//!
//! These primitives do not establish that a whole VFS policy is enforced.

mod devices;
mod journal;
mod native;

pub use native::ScopedMounts;
pub(crate) use native::snapshot;

use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MountId(pub [i32; 2]);

impl MountId {
    fn kernel_name(self) -> String {
        format!("FSID:{}:{}", self.0[0], self.0[1])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountRecord {
    pub id: MountId,
    pub target: PathBuf,
    pub filesystem_type: String,
    pub mounted_from: PathBuf,
    pub flags: u64,
}

/// Stop at the first failed child. Never proceed to its parent or forget a
/// failed resource. Popped entries can be safely retried after interruption.
fn rollback<T>(owned: &mut Vec<T>, mut remove: impl FnMut(&T) -> io::Result<()>) -> io::Result<()> {
    while let Some(last) = owned.last() {
        remove(last)?;
        owned.pop();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
