use super::SocketPolicy;
use super::VfsAccessMode;
use super::VfsEntry;
use super::VfsPath;
use super::VfsPolicy;
use super::VfsSpecialPath;
use crate::protocol::ReadOnlyAccess;
use crate::protocol::SandboxPolicy;

impl From<&SandboxPolicy> for SocketPolicy {
    fn from(value: &SandboxPolicy) -> Self {
        if value.has_full_network_access() {
            SocketPolicy::Enabled
        } else {
            SocketPolicy::Restricted
        }
    }
}

impl From<&SandboxPolicy> for VfsPolicy {
    fn from(value: &SandboxPolicy) -> Self {
        match value {
            SandboxPolicy::RootAccess => VfsPolicy::unrestricted(),
            SandboxPolicy::ExternalSandbox { .. } => VfsPolicy::external_sandbox(),
            SandboxPolicy::ReadOnly { access, .. } => {
                let mut entries = Vec::new();
                match access {
                    ReadOnlyAccess::FullAccess => entries.push(VfsEntry {
                        path: VfsPath::Special {
                            value: VfsSpecialPath::Root,
                        },
                        access: VfsAccessMode::Read,
                    }),
                    ReadOnlyAccess::Restricted {
                        include_platform_defaults,
                        readable_roots,
                    } => {
                        entries.push(VfsEntry {
                            path: VfsPath::Special {
                                value: VfsSpecialPath::CurrentWorkingDirectory,
                            },
                            access: VfsAccessMode::Read,
                        });
                        if *include_platform_defaults {
                            entries.push(VfsEntry {
                                path: VfsPath::Special {
                                    value: VfsSpecialPath::Minimal,
                                },
                                access: VfsAccessMode::Read,
                            });
                        }
                        entries.extend(readable_roots.iter().cloned().map(|path| VfsEntry {
                            path: VfsPath::Path { path },
                            access: VfsAccessMode::Read,
                        }));
                    }
                }
                VfsPolicy::restricted(entries)
            }
            SandboxPolicy::WorkspaceWrite {
                writable_roots,
                read_only_access,
                exclude_tmpdir_env_var,
                exclude_slash_tmp,
                ..
            } => {
                let mut entries = Vec::new();
                match read_only_access {
                    ReadOnlyAccess::FullAccess => entries.push(VfsEntry {
                        path: VfsPath::Special {
                            value: VfsSpecialPath::Root,
                        },
                        access: VfsAccessMode::Read,
                    }),
                    ReadOnlyAccess::Restricted {
                        include_platform_defaults,
                        readable_roots,
                    } => {
                        if *include_platform_defaults {
                            entries.push(VfsEntry {
                                path: VfsPath::Special {
                                    value: VfsSpecialPath::Minimal,
                                },
                                access: VfsAccessMode::Read,
                            });
                        }
                        entries.extend(readable_roots.iter().cloned().map(|path| VfsEntry {
                            path: VfsPath::Path { path },
                            access: VfsAccessMode::Read,
                        }));
                    }
                }

                entries.push(VfsEntry {
                    path: VfsPath::Special {
                        value: VfsSpecialPath::CurrentWorkingDirectory,
                    },
                    access: VfsAccessMode::Write,
                });
                if !exclude_slash_tmp {
                    entries.push(VfsEntry {
                        path: VfsPath::Special {
                            value: VfsSpecialPath::SlashTmp,
                        },
                        access: VfsAccessMode::Write,
                    });
                }
                if !exclude_tmpdir_env_var {
                    entries.push(VfsEntry {
                        path: VfsPath::Special {
                            value: VfsSpecialPath::Tmpdir,
                        },
                        access: VfsAccessMode::Write,
                    });
                }
                entries.extend(writable_roots.iter().cloned().map(|path| VfsEntry {
                    path: VfsPath::Path { path },
                    access: VfsAccessMode::Write,
                }));
                VfsPolicy::restricted(entries)
            }
        }
    }
}
