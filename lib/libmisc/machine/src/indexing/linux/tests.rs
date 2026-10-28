use super::super::tests::output;
use super::*;
use std::io;

#[test]
fn truncated_candidates_require_exact_executable_basenames() {
    let probe = output(concat!(
        "90 1.2 tracker-extract\n",
        "12 0.0 baloo_file_extr\n",
        "13 0.0 localsearch-ext\n",
        "14 0.0 tracker-miner-f\n",
        "15 0.0 updatedb\n",
        "80 2.0 updatedb\n",
        "91 9.0 tracker-miner-f\n",
        "92 9.0 grep tracker-extract\n",
        "93 9.0 not-baloo_file\n",
        "94 9.0 baloo_file_helper\n",
        "95 9.0 mds\n",
        "96 9.0 tracker-miner-\n",
    ));
    let value = processes(&probe, |pid| {
        Ok(PathBuf::from(match pid {
            90 => "/usr/lib/tracker-extract",
            12 => "/usr/lib/a directory/baloo_file_extractor",
            13 => "/usr/lib/localsearch-extractor-3",
            14 => "/usr/lib/tracker-miner-fs-3",
            15 => "/usr/bin/updatedb (deleted)",
            80 => "/tmp/not-updatedb",
            91 => "/tmp/tracker-miner-fs-helper",
            _ => panic!("unrelated process resolved: {pid}"),
        }))
    })
    .expect("processes");
    assert_eq!(
        value
            .iter()
            .map(|process| (process.pid, process.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (12, "baloo_file_extractor"),
            (13, "localsearch-extractor-3"),
            (14, "tracker-miner-fs-3"),
            (15, "updatedb"),
            (90, "tracker-extract"),
        ]
    );
}

#[test]
fn inaccessible_candidates_fail_but_exited_candidates_are_skipped() {
    let probe = output("1 0.0 baloo_file\n2 1.2 updatedb\n");
    assert_eq!(
        processes(&probe, |pid| match pid {
            1 => Ok("/usr/bin/baloo_file".into()),
            _ => Err(io::ErrorKind::PermissionDenied.into()),
        }),
        Err(Error::PermissionDenied)
    );
    assert_eq!(
        processes(&probe, |pid| match pid {
            1 => Ok("/usr/bin/baloo_file".into()),
            _ => Err(io::ErrorKind::NotFound.into()),
        }),
        Ok(vec![IndexerProcess {
            pid: 1,
            name: "baloo_file".into(),
            cpu_percent: 0.0,
        }])
    );
    assert!(matches!(
        processes(&probe, |_| Err(io::ErrorKind::InvalidData.into())),
        Err(Error::Unavailable(_))
    ));
}
