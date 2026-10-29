use super::super::tests::output;
use super::*;
use std::io;
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[test]
fn accounting_names_require_exact_full_executable_basenames() {
    let probe = output(concat!(
        "0 0.0 kernel\n",
        "90 123.4 locate.code\n",
        "12 0.0 locate.bigram\n",
        "13 1.2 localsearch-extract\n",
        "14 0.0 baloo_file_extracto\n",
        "15 0.0 recollindex\n",
        "16 0.0 tracker-miner-fs-3\n",
        "17 0.0 updatedb\n",
        "18 0.0 locate.code\n",
        "19 0.0 localsearch-extract\n",
        "80 99.0 not-locate.code\n",
        "81 99.0 locate.code-helper\n",
        "82 99.0 grep locate.code\n",
        "83 99.0 sh\n",
        "84 99.0 find\n",
        "85 99.0 sort\n",
        "86 99.0 mds\n",
        "87 99.0 localsearch-extrac\n",
    ));
    let value = processes(
        &probe,
        |pid| {
            Ok(PathBuf::from(match pid {
                90 => "/usr/libexec/locate.code",
                12 => "/usr/libexec/locate.bigram",
                13 => "/usr/local/libexec/localsearch-extractor-3",
                14 => "/usr/local/a directory/baloo_file_extractor",
                15 => "/usr/local/bin/recollindex",
                16 => "/usr/local/libexec/tracker-miner-fs-3",
                17 => "/bin/sh",
                18 => "/tmp/not-locate.code",
                19 => "/tmp/localsearch-extractor-helper",
                _ => panic!("unrelated process resolved: {pid}"),
            }))
        },
        deadline(),
    )
    .expect("processes");
    assert_eq!(
        value
            .iter()
            .map(|process| (process.pid, process.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (12, "locate.bigram"),
            (13, "localsearch-extractor-3"),
            (14, "baloo_file_extractor"),
            (15, "recollindex"),
            (16, "tracker-miner-fs-3"),
            (90, "locate.code"),
        ]
    );
    assert_eq!(value.last().unwrap().cpu_percent, 123.4);
}

#[test]
fn denied_or_failed_candidates_never_become_partial_success() {
    let probe = output("1 0.0 locate.code\n2 1.2 baloo_file\n");
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData] {
        let result = processes(
            &probe,
            |pid| match pid {
                1 => Ok("/usr/libexec/locate.code".into()),
                _ => Err(kind.into()),
            },
            deadline(),
        );
        match kind {
            io::ErrorKind::PermissionDenied => assert_eq!(result, Err(Error::PermissionDenied)),
            _ => assert!(matches!(result, Err(Error::Unavailable(_)))),
        }
    }
}

#[test]
fn exited_candidates_are_skipped_but_empty_tables_are_not_measurements() {
    for error in [
        io::Error::from(io::ErrorKind::NotFound),
        io::Error::from_raw_os_error(libc::ESRCH),
    ] {
        let mut error = Some(error);
        let value = processes(
            &output("1 0.0 locate.code\n2 0.0 recollindex\n"),
            |pid| match pid {
                1 => Ok("/usr/libexec/locate.code".into()),
                _ => Err(error.take().unwrap()),
            },
            deadline(),
        )
        .unwrap();
        assert_eq!(value.len(), 1);
        assert_eq!(value[0].pid, 1);
    }
    assert_eq!(
        processes(&output("1 0.0 init\n"), |_| unreachable!(), deadline()),
        Ok(vec![])
    );
    for text in ["", ",pcpu=,comm=\n1\n", "1 0.0 locate.code\ntruncated"] {
        assert!(matches!(
            processes(&output(text), |_| unreachable!(), deadline()),
            Err(Error::Unavailable(_))
        ));
    }
}

#[test]
fn candidate_and_time_budgets_fail_instead_of_truncating() {
    let probe = output(&"1 0.0 locate.code\n".repeat(CANDIDATE_LIMIT + 1));
    assert!(matches!(
        processes(&probe, |_| unreachable!(), deadline()),
        Err(Error::Unavailable(_))
    ));
    let probe = output("1 0.0 locate.code\n");
    assert_eq!(
        processes(&probe, |_| unreachable!(), Instant::now()),
        Err(Error::TimedOut)
    );
    assert_eq!(
        processes(
            &probe,
            |_| {
                std::thread::sleep(Duration::from_millis(20));
                Ok("/usr/libexec/locate.code".into())
            },
            Instant::now() + Duration::from_millis(5),
        ),
        Err(Error::TimedOut)
    );
}

#[cfg(target_os = "freebsd")]
#[test]
fn native_executable_lookup_is_read_only_and_rejects_invalid_pids() {
    assert_eq!(
        executable(std::process::id()).unwrap(),
        std::env::current_exe().unwrap()
    );
    for pid in [0, u32::MAX] {
        assert_eq!(
            executable(pid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[cfg(target_os = "freebsd")]
#[test]
fn native_snapshot_confirms_a_long_name_without_running_an_indexer() {
    use super::super::{IndexingObservation, inspect_indexing};
    use std::process::{Child, Command};

    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // A harmless sleeper exercises real ps columns, MAXCOMLEN truncation, and
    // KERN_PROC_PATHNAME. Never start locate/desktop indexing in a test.
    let directory = tempfile::tempdir().unwrap();
    let name = "localsearch-extractor-3";
    let program = directory.path().join(name);
    std::fs::copy("/bin/sleep", &program).unwrap();
    let child = Running(Command::new(program).arg("30").spawn().unwrap());
    let snapshot = inspect_indexing(directory.path());
    assert_eq!(snapshot.spotlight, IndexingObservation::Unsupported);
    assert!(snapshot.volume.is_none());
    let IndexingObservation::Available { value } = snapshot.processes else {
        panic!("native process probe failed: {:?}", snapshot.processes);
    };
    assert!(
        value
            .iter()
            .any(|process| process.pid == child.0.id() && process.name == name),
        "{value:?}"
    );
}
