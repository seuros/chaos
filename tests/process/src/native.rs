//! Small native probes for behavior unavailable in Lua's standard library.

mod http;

use std::ffi::{CStr, CString};
use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use mlua::Lua;

pub fn register(lua: &Lua) -> mlua::Result<()> {
    let fixture = lua.create_table()?;
    fixture.set(
        "sleep",
        lua.create_function(|_, seconds: f64| {
            let duration = Duration::try_from_secs_f64(seconds).map_err(mlua::Error::external)?;
            std::thread::sleep(duration);
            Ok(())
        })?,
    )?;
    fixture.set("pid", lua.create_function(|_, ()| Ok(std::process::id()))?)?;
    fixture.set(
        "stdin_isatty",
        lua.create_function(|_, ()| Ok(unsafe { libc::isatty(libc::STDIN_FILENO) } == 1))?,
    )?;
    fixture.set(
        "getpwuid",
        lua.create_function(|_, ()| getpwuid().map_err(mlua::Error::external))?,
    )?;
    fixture.set(
        "openpty_roundtrip",
        lua.create_function(|_, ()| openpty_roundtrip().map_err(mlua::Error::external))?,
    )?;
    fixture.set(
        "fork_semaphore",
        lua.create_function(|_, ()| fork_semaphore().map_err(mlua::Error::external))?,
    )?;
    fixture.set(
        "spawn_detached_sleep",
        lua.create_function(|_, (seconds, path): (u64, String)| {
            spawn_detached_sleep(seconds, &path).map_err(mlua::Error::external)
        })?,
    )?;
    fixture.set(
        "http_get",
        lua.create_function(|_, (url, proxy, seconds): (String, bool, u64)| {
            http::get(&url, proxy, Duration::from_secs(seconds)).map_err(mlua::Error::external)
        })?,
    )?;
    fixture.set(
        "socket",
        lua.create_function(|_, (domain, kind, protocol): (i32, i32, i32)| {
            let fd = unsafe { libc::socket(domain, kind, protocol) };
            if fd < 0 {
                Ok((false, io::Error::last_os_error().raw_os_error()))
            } else {
                // SAFETY: fd is a fresh, exclusively owned socket descriptor.
                drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) });
                Ok((true, None))
            }
        })?,
    )?;
    fixture.set("AF_UNIX", libc::AF_UNIX)?;
    fixture.set("SOCK_STREAM", libc::SOCK_STREAM)?;
    fixture.set("SOCK_RAW", libc::SOCK_RAW)?;
    fixture.set("IPPROTO_RAW", libc::IPPROTO_RAW)?;
    fixture.set("EPERM", libc::EPERM)?;
    fixture.set("EACCES", libc::EACCES)?;
    #[cfg(target_os = "linux")]
    fixture.set("AF_PACKET", libc::AF_PACKET)?;
    #[cfg(target_os = "macos")]
    fixture.set(
        "lsopen_allowed",
        lua.create_function(|_, ()| Ok(lsopen_allowed()))?,
    )?;
    lua.globals().set("fixture", fixture)
}

fn getpwuid() -> io::Result<String> {
    // The fixture runs single-threaded and copies the static result immediately.
    let entry = unsafe { libc::getpwuid(libc::getuid()) };
    if entry.is_null() {
        return Err(io::Error::other("getpwuid returned no entry"));
    }
    Ok(unsafe { CStr::from_ptr((*entry).pw_name) }
        .to_string_lossy()
        .into_owned())
}

fn openpty_roundtrip() -> io::Result<String> {
    let mut master = -1;
    let mut slave = -1;
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openpty returned two fresh, exclusively owned descriptors.
    let mut master = unsafe { std::fs::File::from_raw_fd(master) };
    let mut slave = unsafe { std::fs::File::from_raw_fd(slave) };
    slave.write_all(b"ping")?;
    let mut bytes = [0; 4];
    master.read_exact(&mut bytes)?;
    if &bytes != b"ping" {
        return Err(io::Error::other("openpty roundtrip changed the bytes"));
    }
    Ok("ping".to_string())
}

fn fork_semaphore() -> io::Result<()> {
    let name = CString::new(format!("/chaos-test-{}", std::process::id()))?;
    let semaphore = unsafe {
        libc::sem_open(
            name.as_ptr(),
            libc::O_CREAT | libc::O_EXCL,
            0o600 as libc::c_uint,
            1 as libc::c_uint,
        )
    };
    if semaphore == libc::SEM_FAILED {
        return Err(io::Error::last_os_error());
    }
    // Unlink immediately, so even a killed fixture cannot leave a named object.
    if unsafe { libc::sem_unlink(name.as_ptr()) } < 0 {
        let err = io::Error::last_os_error();
        unsafe { libc::sem_close(semaphore) };
        return Err(err);
    }
    let result = fork_with_semaphore(semaphore);
    unsafe { libc::sem_close(semaphore) };
    result
}

fn fork_with_semaphore(semaphore: *mut libc::sem_t) -> io::Result<()> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // Only libc operations after fork; don't re-enter the Lua VM in the child.
        let ok = unsafe { libc::sem_wait(semaphore) == 0 && libc::sem_post(semaphore) == 0 };
        unsafe { libc::_exit(if ok { 0 } else { 1 }) };
    }
    let mut status = 0;
    loop {
        if unsafe { libc::waitpid(pid, &mut status, 0) } >= 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "semaphore child failed: {status}"
        )))
    }
}

fn spawn_detached_sleep(seconds: u64, path: &str) -> io::Result<u32> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["--eval", &format!("fixture.sleep({seconds})")]);
    // SAFETY: setsid is the only operation in this post-fork callback.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn()?;
    let pid = child.id();
    if let Err(err) = std::fs::write(path, pid.to_string()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err);
    }
    Ok(pid)
}

#[cfg(target_os = "macos")]
fn lsopen_allowed() -> bool {
    #[link(name = "sandbox")]
    unsafe extern "C" {
        fn sandbox_check(pid: libc::pid_t, operation: *const libc::c_char, filter: i32, ...)
        -> i32;
    }
    unsafe { sandbox_check(libc::getpid(), c"lsopen".as_ptr(), 0) == 0 }
}
