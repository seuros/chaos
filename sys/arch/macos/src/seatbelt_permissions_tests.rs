use super::MacOsAutomationPermission;
use super::MacOsContactsPermission;
use super::MacOsPreferencesPermission;
use super::MacOsSeatbeltProfileExtensions;
use super::build_seatbelt_extensions;

fn assert_policy_contains_all(policy: &str, expected_fragments: &[&str]) {
    for expected_fragment in expected_fragments {
        assert!(
            policy.contains(expected_fragment),
            "expected policy to contain `{expected_fragment}`:\n{policy}"
        );
    }
}

fn assert_policy_contains_none(policy: &str, unexpected_fragments: &[&str]) {
    for unexpected_fragment in unexpected_fragments {
        assert!(
            !policy.contains(unexpected_fragment),
            "did not expect policy to contain `{unexpected_fragment}`:\n{policy}"
        );
    }
}

#[test]
fn all_none_extensions_emit_no_policy_or_dir_params() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_preferences: MacOsPreferencesPermission::None,
        ..Default::default()
    });

    assert_eq!("", policy.policy);
    assert!(policy.dir_params.is_empty());
}

#[test]
fn preferences_read_only_emits_read_clauses_only() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_preferences: MacOsPreferencesPermission::ReadOnly,
        ..Default::default()
    });

    assert_policy_contains_all(
        &policy.policy,
        &[
            "(allow ipc-posix-shm-read* (ipc-posix-name-prefix \"apple.cfprefs.\"))",
            "(global-name \"com.apple.cfprefsd.daemon\")",
            "(global-name \"com.apple.cfprefsd.agent\")",
            "(local-name \"com.apple.cfprefsd.agent\")",
            "(allow user-preference-read)",
        ],
    );
    assert_policy_contains_none(
        &policy.policy,
        &[
            "(allow user-preference-write)",
            "(allow ipc-posix-shm-write-data (ipc-posix-name-prefix \"apple.cfprefs.\"))",
            "(allow ipc-posix-shm-write-create (ipc-posix-name-prefix \"apple.cfprefs.\"))",
        ],
    );
    assert!(policy.dir_params.is_empty());
}

#[test]
fn preferences_read_write_emits_write_clauses() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_preferences: MacOsPreferencesPermission::ReadWrite,
        ..Default::default()
    });

    assert_policy_contains_all(
        &policy.policy,
        &[
            "(allow ipc-posix-shm-read* (ipc-posix-name-prefix \"apple.cfprefs.\"))",
            "(global-name \"com.apple.cfprefsd.daemon\")",
            "(global-name \"com.apple.cfprefsd.agent\")",
            "(local-name \"com.apple.cfprefsd.agent\")",
            "(allow user-preference-read)",
            "(allow user-preference-write)",
            "(allow ipc-posix-shm-write-data (ipc-posix-name-prefix \"apple.cfprefs.\"))",
            "(allow ipc-posix-shm-write-create (ipc-posix-name-prefix \"apple.cfprefs.\"))",
        ],
    );
    assert!(policy.dir_params.is_empty());
}

#[test]
fn automation_all_emits_unscoped_appleevents() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_automation: MacOsAutomationPermission::All,
        ..Default::default()
    });

    assert_policy_contains_all(
        &policy.policy,
        &[
            "(global-name \"com.apple.coreservices.appleevents\")",
            "(allow appleevent-send)",
        ],
    );
    assert_policy_contains_none(&policy.policy, &["(appleevent-destination"]);
}

#[test]
fn automation_bundle_ids_are_normalized_and_scoped() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_automation: MacOsAutomationPermission::BundleIds(vec![
            " com.apple.Notes ".to_string(),
            "com.apple.Calendar".to_string(),
            "bad bundle".to_string(),
            "com.apple.Notes".to_string(),
        ]),
        ..Default::default()
    });

    assert_policy_contains_all(
        &policy.policy,
        &[
            "(global-name \"com.apple.coreservices.appleevents\")",
            "(appleevent-destination \"com.apple.Calendar\")",
            "(appleevent-destination \"com.apple.Notes\")",
        ],
    );
    assert_eq!(
        2,
        policy.policy.matches("(appleevent-destination").count(),
        "expected normalized bundle IDs to deduplicate scoped destinations:\n{}",
        policy.policy
    );
    assert!(policy.policy.contains("(allow appleevent-send\n"));
    assert_policy_contains_none(&policy.policy, &["(allow appleevent-send)", "bad bundle"]);
    assert!(policy.dir_params.is_empty());
}

#[test]
fn launch_services_emit_launch_clauses() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_launch_services: true,
        ..Default::default()
    });

    assert_policy_contains_all(
        &policy.policy,
        &[
            "com.apple.coreservices.launchservicesd",
            "com.apple.lsd.mapdb",
            "com.apple.coreservices.quarantine-resolver",
            "com.apple.lsd.modifydb",
            "(allow lsopen)",
        ],
    );
    assert!(policy.dir_params.is_empty());
}

#[test]
fn accessibility_emits_axserver_lookup() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_accessibility: true,
        ..Default::default()
    });

    assert_policy_contains_all(&policy.policy, &["(local-name \"com.apple.axserver\")"]);
    assert_policy_contains_none(
        &policy.policy,
        &["com.apple.CalendarAgent", "com.apple.remindd"],
    );
}

#[test]
fn calendar_emits_calendar_agent_lookup() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_calendar: true,
        ..Default::default()
    });

    assert_policy_contains_all(
        &policy.policy,
        &["(global-name \"com.apple.CalendarAgent\")"],
    );
    assert_policy_contains_none(&policy.policy, &["com.apple.axserver", "com.apple.remindd"]);
}

#[test]
fn reminders_emit_calendar_agent_and_remindd_lookups() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_reminders: true,
        ..Default::default()
    });

    assert_policy_contains_all(
        &policy.policy,
        &[
            "(global-name \"com.apple.CalendarAgent\")",
            "(global-name \"com.apple.remindd\")",
        ],
    );
    assert_policy_contains_none(&policy.policy, &["com.apple.axserver"]);
}

#[test]
fn contacts_read_only_emit_contacts_read_clauses() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_contacts: MacOsContactsPermission::ReadOnly,
        ..Default::default()
    });

    assert_policy_contains_all(
        &policy.policy,
        &[
            "(allow file-read* file-test-existence",
            "(subpath \"/System/Library/Address Book Plug-Ins\")",
            "(subpath (param \"ADDRESSBOOK_DIR\"))",
            "(global-name \"com.apple.tccd\")",
            "(global-name \"com.apple.tccd.system\")",
            "(global-name \"com.apple.contactsd.persistence\")",
            "(global-name \"com.apple.AddressBook.ContactsAccountsService\")",
            "(global-name \"com.apple.contacts.account-caching\")",
            "(global-name \"com.apple.accountsd.accountmanager\")",
        ],
    );
    assert_policy_contains_none(
        &policy.policy,
        &[
            "(allow file-write*",
            "(subpath \"/var/folders\")",
            "(subpath \"/private/var/folders\")",
            "com.apple.securityd.xpc",
        ],
    );

    let expected_dir_params = dirs::home_dir()
        .map(|home| {
            vec![(
                "ADDRESSBOOK_DIR".to_string(),
                home.join("Library/Application Support/AddressBook"),
            )]
        })
        .unwrap_or_default();
    assert_eq!(expected_dir_params, policy.dir_params);
}

#[test]
fn contacts_read_write_emit_write_clauses() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions {
        macos_contacts: MacOsContactsPermission::ReadWrite,
        ..Default::default()
    });

    assert_policy_contains_all(
        &policy.policy,
        &[
            "(allow file-read* file-write*",
            "(subpath \"/System/Library/Address Book Plug-Ins\")",
            "(subpath (param \"ADDRESSBOOK_DIR\"))",
            "(subpath \"/var/folders\")",
            "(subpath \"/private/var/folders\")",
            "(global-name \"com.apple.tccd\")",
            "(global-name \"com.apple.tccd.system\")",
            "(global-name \"com.apple.contactsd.persistence\")",
            "(global-name \"com.apple.AddressBook.ContactsAccountsService\")",
            "(global-name \"com.apple.contacts.account-caching\")",
            "(global-name \"com.apple.accountsd.accountmanager\")",
            "(global-name \"com.apple.securityd.xpc\")",
        ],
    );

    let expected_dir_params = dirs::home_dir()
        .map(|home| {
            vec![(
                "ADDRESSBOOK_DIR".to_string(),
                home.join("Library/Application Support/AddressBook"),
            )]
        })
        .unwrap_or_default();
    assert_eq!(expected_dir_params, policy.dir_params);
}

#[test]
fn default_extensions_emit_preferences_read_only_policy() {
    let policy = build_seatbelt_extensions(&MacOsSeatbeltProfileExtensions::default());

    assert_policy_contains_all(
        &policy.policy,
        &[
            "(allow ipc-posix-shm-read* (ipc-posix-name-prefix \"apple.cfprefs.\"))",
            "(global-name \"com.apple.cfprefsd.daemon\")",
            "(global-name \"com.apple.cfprefsd.agent\")",
            "(local-name \"com.apple.cfprefsd.agent\")",
            "(allow user-preference-read)",
        ],
    );
    assert_policy_contains_none(
        &policy.policy,
        &[
            "(allow user-preference-write)",
            "(allow appleevent-send)",
            "(allow lsopen)",
            "com.apple.axserver",
            "com.apple.CalendarAgent",
            "com.apple.remindd",
            "ADDRESSBOOK_DIR",
        ],
    );
    assert!(policy.dir_params.is_empty());
}
