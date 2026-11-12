use std::collections::HashSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::warn;

/// Tracks the (recursive) descendants of a process by using `kqueue` to watch for fork events, and
/// `proc_listchildpids` to list the children of a process.
#[derive(Debug)]
pub(super) struct PidTracker {
    kq: Arc<OwnedFd>,
    handle: Option<JoinHandle<HashSet<i32>>>,
}

impl PidTracker {
    pub(crate) fn new(root_pid: i32) -> Option<Self> {
        if root_pid <= 0 {
            return None;
        }

        // SAFETY: kqueue takes no pointers and returns a new owned descriptor.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            warn!(
                "failed to create sandbox PID tracker: {}",
                std::io::Error::last_os_error()
            );
            return None;
        }
        // SAFETY: fd is a newly created, valid descriptor owned by this tracker.
        let kq = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        // Register before spawning the worker so even an immediate drop can wake it.
        if !register_stop_event(kq.as_raw_fd()) {
            return None;
        }
        let worker_kq = Arc::clone(&kq);
        let handle =
            tokio::task::spawn_blocking(move || track_descendants(worker_kq.as_raw_fd(), root_pid));

        Some(Self {
            kq,
            handle: Some(handle),
        })
    }

    pub(super) async fn stop(mut self) -> HashSet<i32> {
        trigger_stop_event(self.kq.as_raw_fd());
        match self.handle.take() {
            Some(handle) => match handle.await {
                Ok(seen) => seen,
                Err(error) => {
                    warn!("sandbox PID tracker failed: {error}");
                    HashSet::new()
                }
            },
            None => HashSet::new(),
        }
    }
}

impl Drop for PidTracker {
    fn drop(&mut self) {
        if self.handle.is_some() {
            trigger_stop_event(self.kq.as_raw_fd());
        }
    }
}

unsafe extern "C" {
    fn proc_listchildpids(
        ppid: libc::c_int,
        buffer: *mut libc::c_void,
        buffersize: libc::c_int,
    ) -> libc::c_int;
}

/// Wrap proc_listchildpids.
fn list_child_pids(parent: i32) -> Vec<i32> {
    // SAFETY: each call receives a writable buffer and its exact byte capacity.
    unsafe {
        let mut capacity: usize = 16;
        loop {
            let mut buf: Vec<i32> = vec![0; capacity];
            let count = proc_listchildpids(
                parent as libc::c_int,
                buf.as_mut_ptr() as *mut libc::c_void,
                (buf.len() * std::mem::size_of::<i32>()) as libc::c_int,
            );
            if count <= 0 {
                return Vec::new();
            }
            let returned = count as usize;
            if returned < capacity {
                buf.truncate(returned);
                return buf;
            }
            capacity = capacity.saturating_mul(2).max(returned + 16);
        }
    }
}

fn pid_is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal zero only checks existence/permissions; no pointers are passed.
    let res = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if res == 0 {
        true
    } else {
        matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        )
    }
}

enum WatchPidError {
    ProcessGone,
    Other(std::io::Error),
}

/// Add `pid` to the watch list in `kq`.
fn watch_pid(kq: libc::c_int, pid: i32) -> Result<(), WatchPidError> {
    if pid <= 0 {
        return Err(WatchPidError::ProcessGone);
    }

    let kev = libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_CLEAR,
        fflags: libc::NOTE_FORK | libc::NOTE_EXEC | libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };

    // SAFETY: kev is one initialized change; no event output is requested.
    let res = unsafe { libc::kevent(kq, &kev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if res < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Err(WatchPidError::ProcessGone)
        } else {
            Err(WatchPidError::Other(err))
        }
    } else {
        Ok(())
    }
}

fn watch_children(
    kq: libc::c_int,
    parent: i32,
    seen: &mut HashSet<i32>,
    active: &mut HashSet<i32>,
) {
    for child_pid in list_child_pids(parent) {
        add_pid_watch(kq, child_pid, seen, active);
    }
}

/// Watch `pid` and its children, updating `seen` and `active` sets.
fn add_pid_watch(kq: libc::c_int, pid: i32, seen: &mut HashSet<i32>, active: &mut HashSet<i32>) {
    if pid <= 0 {
        return;
    }

    let newly_seen = seen.insert(pid);
    let mut should_recurse = newly_seen;

    if active.insert(pid) {
        match watch_pid(kq, pid) {
            Ok(()) => {
                should_recurse = true;
            }
            Err(WatchPidError::ProcessGone) => {
                active.remove(&pid);
                return;
            }
            Err(WatchPidError::Other(err)) => {
                warn!("failed to watch pid {pid}: {err}");
                active.remove(&pid);
                return;
            }
        }
    }

    if should_recurse {
        watch_children(kq, pid, seen, active);
    }
}
const STOP_IDENT: libc::uintptr_t = 1;

fn register_stop_event(kq: libc::c_int) -> bool {
    let kev = libc::kevent {
        ident: STOP_IDENT,
        filter: libc::EVFILT_USER,
        flags: libc::EV_ADD | libc::EV_CLEAR,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    };

    // SAFETY: kev is one initialized change; no event output is requested.
    let res = unsafe { libc::kevent(kq, &kev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    res >= 0
}

fn trigger_stop_event(kq: libc::c_int) {
    if kq < 0 {
        return;
    }

    let kev = libc::kevent {
        ident: STOP_IDENT,
        filter: libc::EVFILT_USER,
        flags: 0,
        fflags: libc::NOTE_TRIGGER,
        data: 0,
        udata: std::ptr::null_mut(),
    };

    // SAFETY: the OwnedFd keeps kq alive; kev is one initialized user event.
    if unsafe { libc::kevent(kq, &kev, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0 {
        warn!(
            "failed to stop sandbox PID tracker: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// Put all of the above together to track all the descendants of `root_pid`.
fn track_descendants(kq: libc::c_int, root_pid: i32) -> HashSet<i32> {
    let mut seen: HashSet<i32> = HashSet::new();
    let mut active: HashSet<i32> = HashSet::new();

    add_pid_watch(kq, root_pid, &mut seen, &mut active);

    const EVENTS_CAP: usize = 32;
    // SAFETY: kevent is a plain C struct for which zeroed storage is valid.
    let mut events: [libc::kevent; EVENTS_CAP] =
        unsafe { std::mem::MaybeUninit::zeroed().assume_init() };

    let mut stop_requested = false;
    loop {
        if active.is_empty() {
            if !pid_is_alive(root_pid) {
                break;
            }
            add_pid_watch(kq, root_pid, &mut seen, &mut active);
        }

        // SAFETY: events has EVENTS_CAP writable entries; kq remains owned by the caller.
        let nev = unsafe {
            libc::kevent(
                kq,
                std::ptr::null::<libc::kevent>(),
                0,
                events.as_mut_ptr(),
                EVENTS_CAP as libc::c_int,
                std::ptr::null(),
            )
        };

        if nev < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }

        if nev == 0 {
            continue;
        }

        for ev in events.iter().take(nev as usize) {
            let pid = ev.ident as i32;

            if ev.filter == libc::EVFILT_USER && ev.ident == STOP_IDENT {
                stop_requested = true;
                break;
            }

            if (ev.flags & libc::EV_ERROR) != 0 {
                if ev.data == libc::ESRCH as isize {
                    active.remove(&pid);
                }
                continue;
            }

            if (ev.fflags & libc::NOTE_FORK) != 0 {
                watch_children(kq, pid, &mut seen, &mut active);
            }

            if (ev.fflags & libc::NOTE_EXIT) != 0 {
                active.remove(&pid);
            }
        }

        if stop_requested {
            break;
        }
    }

    seen
}

#[cfg(test)]
mod tests;
