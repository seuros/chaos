use super::*;

#[test]
fn config_toml_deserializes_permission_profiles() {
    let toml = r#"
default_permissions = "workspace"

[permissions.workspace.filesystem]
":minimal" = "read"

[permissions.workspace.filesystem.":project_roots"]
"." = "write"
"docs" = "read"

[permissions.workspace.network]
enabled = true
proxy_url = "http://127.0.0.1:43128"
enable_socks5 = false
allow_upstream_proxy = false
allowed_domains = ["openai.com"]
"#;
    let cfg: ConfigToml =
        toml::from_str(toml).expect("TOML deserialization should succeed for permissions profiles");

    assert_eq!(cfg.default_permissions.as_deref(), Some("workspace"));
    assert_eq!(
        cfg.permissions.expect("[permissions] should deserialize"),
        PermissionsToml {
            entries: BTreeMap::from([(
                "workspace".to_string(),
                PermissionProfileToml {
                    filesystem: Some(FilesystemPermissionsToml {
                        entries: BTreeMap::from([
                            (
                                ":minimal".to_string(),
                                FilesystemPermissionToml::Access(VfsAccessMode::Read),
                            ),
                            (
                                ":project_roots".to_string(),
                                FilesystemPermissionToml::Scoped(BTreeMap::from([
                                    (".".to_string(), VfsAccessMode::Write),
                                    ("docs".to_string(), VfsAccessMode::Read),
                                ])),
                            ),
                        ]),
                    }),
                    network: Some(NetworkToml {
                        enabled: Some(true),
                        proxy_url: Some("http://127.0.0.1:43128".to_string()),
                        enable_socks5: Some(false),
                        socks_url: None,
                        enable_socks5_udp: None,
                        allow_upstream_proxy: Some(false),
                        dangerously_allow_non_loopback_proxy: None,
                        dangerously_allow_all_unix_sockets: None,
                        mode: None,
                        allowed_domains: Some(vec!["openai.com".to_string()]),
                        denied_domains: None,
                        allow_unix_sockets: None,
                        allow_local_binding: None,
                    }),
                },
            )]),
        }
    );
}

#[test]
fn permissions_profiles_network_populates_runtime_network_proxy_spec() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(cwd.path().join(".git"), "gitdir: nowhere")?;

    let config = Config::load_from_base_config_with_overrides(
        ConfigToml {
            default_permissions: Some("workspace".to_string()),
            permissions: Some(PermissionsToml {
                entries: BTreeMap::from([(
                    "workspace".to_string(),
                    PermissionProfileToml {
                        filesystem: Some(FilesystemPermissionsToml {
                            entries: BTreeMap::from([(
                                ":minimal".to_string(),
                                FilesystemPermissionToml::Access(VfsAccessMode::Read),
                            )]),
                        }),
                        network: Some(NetworkToml {
                            enabled: Some(true),
                            proxy_url: Some("http://127.0.0.1:43128".to_string()),
                            enable_socks5: Some(false),
                            ..Default::default()
                        }),
                    },
                )]),
            }),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )?;
    let network = config
        .permissions
        .network
        .as_ref()
        .expect("enabled profile network should produce a NetworkProxySpec");

    assert_eq!(network.proxy_host_and_port(), "127.0.0.1:43128");
    assert!(!network.socks_enabled());
    Ok(())
}

#[test]
fn permissions_profiles_network_disabled_by_default_does_not_start_proxy() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(cwd.path().join(".git"), "gitdir: nowhere")?;

    let config = Config::load_from_base_config_with_overrides(
        ConfigToml {
            default_permissions: Some("workspace".to_string()),
            permissions: Some(PermissionsToml {
                entries: BTreeMap::from([(
                    "workspace".to_string(),
                    PermissionProfileToml {
                        filesystem: Some(FilesystemPermissionsToml {
                            entries: BTreeMap::from([(
                                ":minimal".to_string(),
                                FilesystemPermissionToml::Access(VfsAccessMode::Read),
                            )]),
                        }),
                        network: Some(NetworkToml {
                            allowed_domains: Some(vec!["openai.com".to_string()]),
                            ..Default::default()
                        }),
                    },
                )]),
            }),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )?;

    assert!(config.permissions.network.is_none());
    Ok(())
}

#[test]
fn default_permissions_profile_populates_runtime_sandbox_policy() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::create_dir_all(cwd.path().join("docs"))?;
    std::fs::write(cwd.path().join(".git"), "gitdir: nowhere")?;

    let cfg = ConfigToml {
        default_permissions: Some("workspace".to_string()),
        permissions: Some(PermissionsToml {
            entries: BTreeMap::from([(
                "workspace".to_string(),
                PermissionProfileToml {
                    filesystem: Some(FilesystemPermissionsToml {
                        entries: BTreeMap::from([
                            (
                                ":minimal".to_string(),
                                FilesystemPermissionToml::Access(VfsAccessMode::Read),
                            ),
                            (
                                ":project_roots".to_string(),
                                FilesystemPermissionToml::Scoped(BTreeMap::from([
                                    (".".to_string(), VfsAccessMode::Write),
                                    ("docs".to_string(), VfsAccessMode::Read),
                                ])),
                            ),
                        ]),
                    }),
                    network: None,
                },
            )]),
        }),
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.permissions.vfs_policy,
        VfsPolicy::restricted(vec![
            VfsEntry {
                path: VfsPath::Special {
                    value: VfsSpecialPath::Minimal,
                },
                access: VfsAccessMode::Read,
            },
            VfsEntry {
                path: VfsPath::Special {
                    value: VfsSpecialPath::project_roots(None),
                },
                access: VfsAccessMode::Write,
            },
            VfsEntry {
                path: VfsPath::Special {
                    value: VfsSpecialPath::project_roots(Some("docs".into())),
                },
                access: VfsAccessMode::Read,
            },
        ]),
    );
    assert_eq!(
        config.permissions.sandbox_policy.get(),
        &SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            read_only_access: ReadOnlyAccess::Restricted {
                include_platform_defaults: true,
                readable_roots: vec![
                    AbsolutePathBuf::try_from(cwd.path().join("docs")).expect("absolute docs path"),
                ],
            },
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        }
    );
    assert_eq!(config.permissions.socket_policy, SocketPolicy::Restricted);
    Ok(())
}

#[test]
fn permissions_profiles_require_default_permissions() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(cwd.path().join(".git"), "gitdir: nowhere")?;

    let err = Config::load_from_base_config_with_overrides(
        ConfigToml {
            permissions: Some(PermissionsToml {
                entries: BTreeMap::from([(
                    "workspace".to_string(),
                    PermissionProfileToml {
                        filesystem: Some(FilesystemPermissionsToml {
                            entries: BTreeMap::from([(
                                ":minimal".to_string(),
                                FilesystemPermissionToml::Access(VfsAccessMode::Read),
                            )]),
                        }),
                        network: None,
                    },
                )]),
            }),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )
    .expect_err("missing default_permissions should be rejected");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        "config defines `[permissions]` profiles but does not set `default_permissions`"
    );
    Ok(())
}

#[test]
fn permissions_profiles_reject_writes_outside_workspace_root() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(cwd.path().join(".git"), "gitdir: nowhere")?;
    let external_write_path = "/tmp";

    let err = Config::load_from_base_config_with_overrides(
        ConfigToml {
            default_permissions: Some("workspace".to_string()),
            permissions: Some(PermissionsToml {
                entries: BTreeMap::from([(
                    "workspace".to_string(),
                    PermissionProfileToml {
                        filesystem: Some(FilesystemPermissionsToml {
                            entries: BTreeMap::from([(
                                external_write_path.to_string(),
                                FilesystemPermissionToml::Access(VfsAccessMode::Write),
                            )]),
                        }),
                        network: None,
                    },
                )]),
            }),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )
    .expect_err("writes outside the workspace root should be rejected");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        err.to_string()
            .contains("filesystem writes outside the workspace root"),
        "{err}"
    );
    Ok(())
}

#[test]
fn permissions_profiles_reject_nested_entries_for_non_project_roots() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(cwd.path().join(".git"), "gitdir: nowhere")?;

    let err = Config::load_from_base_config_with_overrides(
        ConfigToml {
            default_permissions: Some("workspace".to_string()),
            permissions: Some(PermissionsToml {
                entries: BTreeMap::from([(
                    "workspace".to_string(),
                    PermissionProfileToml {
                        filesystem: Some(FilesystemPermissionsToml {
                            entries: BTreeMap::from([(
                                ":minimal".to_string(),
                                FilesystemPermissionToml::Scoped(BTreeMap::from([(
                                    "docs".to_string(),
                                    VfsAccessMode::Read,
                                )])),
                            )]),
                        }),
                        network: None,
                    },
                )]),
            }),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )
    .expect_err("nested entries outside :project_roots should be rejected");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        "filesystem path `:minimal` does not support nested entries"
    );
    Ok(())
}

fn load_workspace_permission_profile(profile: PermissionProfileToml) -> std::io::Result<Config> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(cwd.path().join(".git"), "gitdir: nowhere")?;

    Config::load_from_base_config_with_overrides(
        ConfigToml {
            default_permissions: Some("workspace".to_string()),
            permissions: Some(PermissionsToml {
                entries: BTreeMap::from([("workspace".to_string(), profile)]),
            }),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )
}

#[test]
fn permissions_profiles_allow_unknown_special_paths() -> std::io::Result<()> {
    let config = load_workspace_permission_profile(PermissionProfileToml {
        filesystem: Some(FilesystemPermissionsToml {
            entries: BTreeMap::from([(
                ":future_special_path".to_string(),
                FilesystemPermissionToml::Access(VfsAccessMode::Read),
            )]),
        }),
        network: None,
    })?;

    assert_eq!(
        config.permissions.vfs_policy,
        VfsPolicy::restricted(vec![VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::unknown(":future_special_path", None),
            },
            access: VfsAccessMode::Read,
        }]),
    );
    assert_eq!(
        config.permissions.sandbox_policy.get(),
        &SandboxPolicy::ReadOnly {
            access: ReadOnlyAccess::Restricted {
                include_platform_defaults: false,
                readable_roots: Vec::new(),
            },
            network_access: false,
        }
    );
    assert!(
        config.startup_warnings.iter().any(|warning| warning.contains(
            "Configured filesystem path `:future_special_path` is not recognized by this version of Chaos and will be ignored."
        )),
        "{:?}",
        config.startup_warnings
    );
    Ok(())
}

#[test]
fn permissions_profiles_allow_unknown_special_paths_with_nested_entries() -> std::io::Result<()> {
    let config = load_workspace_permission_profile(PermissionProfileToml {
        filesystem: Some(FilesystemPermissionsToml {
            entries: BTreeMap::from([(
                ":future_special_path".to_string(),
                FilesystemPermissionToml::Scoped(BTreeMap::from([(
                    "docs".to_string(),
                    VfsAccessMode::Read,
                )])),
            )]),
        }),
        network: None,
    })?;

    assert_eq!(
        config.permissions.vfs_policy,
        VfsPolicy::restricted(vec![VfsEntry {
            path: VfsPath::Special {
                value: VfsSpecialPath::unknown(":future_special_path", Some("docs".into())),
            },
            access: VfsAccessMode::Read,
        }]),
    );
    assert!(
        config.startup_warnings.iter().any(|warning| warning.contains(
            "Configured filesystem path `:future_special_path` with nested entry `docs` is not recognized by this version of Chaos and will be ignored."
        )),
        "{:?}",
        config.startup_warnings
    );
    Ok(())
}

#[test]
fn permissions_profiles_allow_missing_filesystem_with_warning() -> std::io::Result<()> {
    let config = load_workspace_permission_profile(PermissionProfileToml {
        filesystem: None,
        network: None,
    })?;

    assert_eq!(
        config.permissions.vfs_policy,
        VfsPolicy::restricted(Vec::new())
    );
    assert_eq!(
        config.permissions.sandbox_policy.get(),
        &SandboxPolicy::ReadOnly {
            access: ReadOnlyAccess::Restricted {
                include_platform_defaults: false,
                readable_roots: Vec::new(),
            },
            network_access: false,
        }
    );
    assert!(
        config.startup_warnings.iter().any(|warning| warning.contains(
            "Permissions profile `workspace` does not define any recognized filesystem entries for this version of Chaos."
        )),
        "{:?}",
        config.startup_warnings
    );
    Ok(())
}

#[test]
fn permissions_profiles_allow_empty_filesystem_with_warning() -> std::io::Result<()> {
    let config = load_workspace_permission_profile(PermissionProfileToml {
        filesystem: Some(FilesystemPermissionsToml {
            entries: BTreeMap::new(),
        }),
        network: None,
    })?;

    assert_eq!(
        config.permissions.vfs_policy,
        VfsPolicy::restricted(Vec::new())
    );
    assert!(
        config.startup_warnings.iter().any(|warning| warning.contains(
            "Permissions profile `workspace` does not define any recognized filesystem entries for this version of Chaos."
        )),
        "{:?}",
        config.startup_warnings
    );
    Ok(())
}

#[test]
fn permissions_profiles_reject_project_root_parent_traversal() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(cwd.path().join(".git"), "gitdir: nowhere")?;

    let err = Config::load_from_base_config_with_overrides(
        ConfigToml {
            default_permissions: Some("workspace".to_string()),
            permissions: Some(PermissionsToml {
                entries: BTreeMap::from([(
                    "workspace".to_string(),
                    PermissionProfileToml {
                        filesystem: Some(FilesystemPermissionsToml {
                            entries: BTreeMap::from([(
                                ":project_roots".to_string(),
                                FilesystemPermissionToml::Scoped(BTreeMap::from([(
                                    "../sibling".to_string(),
                                    VfsAccessMode::Read,
                                )])),
                            )]),
                        }),
                        network: None,
                    },
                )]),
            }),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )
    .expect_err("parent traversal should be rejected for project root subpaths");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        err.to_string(),
        "filesystem subpath `../sibling` must be a descendant path without `.` or `..` components"
    );
    Ok(())
}

#[test]
fn permissions_profiles_allow_network_enablement() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let cwd = TempDir::new()?;
    std::fs::write(cwd.path().join(".git"), "gitdir: nowhere")?;

    let config = Config::load_from_base_config_with_overrides(
        ConfigToml {
            default_permissions: Some("workspace".to_string()),
            permissions: Some(PermissionsToml {
                entries: BTreeMap::from([(
                    "workspace".to_string(),
                    PermissionProfileToml {
                        filesystem: Some(FilesystemPermissionsToml {
                            entries: BTreeMap::from([(
                                ":minimal".to_string(),
                                FilesystemPermissionToml::Access(VfsAccessMode::Read),
                            )]),
                        }),
                        network: Some(NetworkToml {
                            enabled: Some(true),
                            ..Default::default()
                        }),
                    },
                )]),
            }),
            ..Default::default()
        },
        ConfigOverrides {
            cwd: Some(cwd.path().to_path_buf()),
            ..Default::default()
        },
        chaos_home.path().to_path_buf(),
    )?;

    assert!(
        config.permissions.socket_policy.is_enabled(),
        "expected network sandbox policy to be enabled",
    );
    assert!(
        config
            .permissions
            .sandbox_policy
            .get()
            .has_full_network_access()
    );
    Ok(())
}
