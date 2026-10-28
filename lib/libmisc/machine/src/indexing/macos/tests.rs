use super::*;

fn output(text: &str) -> Output {
    Output {
        success: true,
        stdout: text.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn spotlight_requires_exact_unambiguous_states() {
    for (text, value) in [
        ("/:\n\tIndexing enabled.\n", SpotlightState::Enabled),
        (
            "/System/Volumes/Data:\n\tIndexing disabled.\n",
            SpotlightState::Disabled,
        ),
        (
            "/Volumes/External Drive:\n\tIndexing and searching disabled.\n",
            SpotlightState::Disabled,
        ),
    ] {
        assert_eq!(
            observation(spotlight(&output(text))),
            IndexingObservation::Available { value }
        );
    }
    for text in [
        "",
        "/Volumes/disabled:\nError: unknown indexing state.\n",
        "/:\nSpotlight server is disabled.\n",
        "Indexing disabled unexpectedly",
        "/:\nIndexing enabled.\n/Volumes/Other:\nIndexing disabled.\n",
    ] {
        let Err(Error::Unavailable(_)) = spotlight(&output(text)) else {
            panic!("unrecognized indexing state must be unavailable: {text:?}");
        };
    }
}

#[test]
fn validation_rejects_failures_before_parsing_measurements() {
    for (success, stdout, stderr, expected) in [
        (
            false,
            "Indexing enabled.",
            "Operation not permitted",
            Error::PermissionDenied,
        ),
        (true, "Permission denied", "", Error::PermissionDenied),
        (true, "", "Not authorized", Error::PermissionDenied),
        (
            false,
            "Indexing disabled.",
            "",
            Error::Unavailable("probe exited unsuccessfully".into()),
        ),
    ] {
        let mut probe = output(stdout);
        probe.success = success;
        probe.stderr = stderr.as_bytes().to_vec();
        assert_eq!(spotlight(&probe).as_ref(), Err(&expected));
        assert_eq!(processes(&probe).as_ref(), Err(&expected));
    }
    let mut invalid = output("");
    invalid.stdout = vec![0xff];
    let Err(Error::Unavailable(_)) = spotlight(&invalid) else {
        panic!("invalid indexing encoding must be unavailable");
    };
    let Err(Error::Unavailable(_)) = processes(&invalid) else {
        panic!("invalid process encoding must be unavailable");
    };
}

#[test]
fn process_matching_uses_basename_not_substring_or_arguments() {
    let probe = output(concat!(
        "  0 0.0 kernel_task\n",
        "  1 0.0 /sbin/launchd\n",
        "  80 123.4 /System/Library/Frameworks/CoreServices.framework/Support/mds_stores\n",
        "  12 0.0 /System/Library/Frameworks/CoreServices.framework/Support/mds\n",
        "  90 1.2 /System/Library/Frameworks/CoreSpotlight.framework/corespotlightd\n",
        "  99 99.0 /tmp/not-mdworker\n",
        " 100 99.0 /tmp/mdworker-helper\n",
        " 101 99.0 /Applications/My mds App.app/Contents/MacOS/My mds App\n",
        " 102 99.0 /usr/bin/grep mdworker\n",
        " 103 0.0 mdworker_shared\n",
    ));
    let value = processes(&probe).expect("valid process table");
    assert_eq!(
        value.iter().map(|process| process.pid).collect::<Vec<_>>(),
        vec![12, 80, 90, 103]
    );
    assert_eq!(value[1].name, "mds_stores");
    assert_eq!(
        value[1].cpu_percent, 123.4,
        "multicore values may exceed 100"
    );
    assert_eq!(
        processes(&output("1 0.0 /sbin/launchd\n2 0.0 /usr/bin/unrelated\n")),
        Ok(vec![])
    );
    for text in [
        "",
        "\n",
        "not a process table",
        "1 0.0 /sbin/launchd\ntruncated row",
        "5 NaN mdworker",
        "5 inf mdworker",
        "5 -1.0 mdworker",
        "5 0,1 mdworker",
        "5 0.1 ",
        "oops 0.1 mdworker",
    ] {
        let Err(Error::Unavailable(_)) = processes(&output(text)) else {
            panic!("malformed process table must be unavailable: {text:?}");
        };
    }
}

#[cfg(target_os = "macos")]
#[test]
fn workspace_resolution_uses_the_actual_filesystem_and_rejects_bad_paths() {
    let temporary = tempfile::tempdir().expect("workspace");
    let link = temporary.path().join("symlink");
    std::os::unix::fs::symlink(temporary.path(), &link).expect("workspace symlink");
    let volume = workspace_volume(temporary.path()).expect("workspace volume");
    assert!(volume.is_absolute());
    assert_eq!(workspace_volume(&link).expect("symlink volume"), volume);
    assert!(workspace_volume(std::path::Path::new("relative")).is_err());
    assert!(workspace_volume(&temporary.path().join("missing")).is_err());
}
