use super::FileSystemAccessMode;
use super::FileSystemPath;
use super::FileSystemSandboxEntry;
use super::FileSystemSandboxPolicy;
use super::FileSystemSpecialPath;
use super::NetworkSandboxPolicy;
use crate::protocol::ReadOnlyAccess;
use crate::protocol::SandboxPolicy;

impl From<&SandboxPolicy> for NetworkSandboxPolicy {
    fn from(value: &SandboxPolicy) -> Self {
        if value.has_full_network_access() {
            NetworkSandboxPolicy::Enabled
        } else {
            NetworkSandboxPolicy::Restricted
        }
    }
}

impl From<&SandboxPolicy> for FileSystemSandboxPolicy {
    fn from(value: &SandboxPolicy) -> Self {
        match value {
            SandboxPolicy::RootAccess => FileSystemSandboxPolicy::unrestricted(),
            SandboxPolicy::ExternalSandbox { .. } => FileSystemSandboxPolicy::external_sandbox(),
            SandboxPolicy::ReadOnly { access, .. } => {
                let mut entries = Vec::new();
                match access {
                    ReadOnlyAccess::FullAccess => entries.push(FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::Root,
                        },
                        access: FileSystemAccessMode::Read,
                    }),
                    ReadOnlyAccess::Restricted {
                        include_platform_defaults,
                        readable_roots,
                    } => {
                        entries.push(FileSystemSandboxEntry {
                            path: FileSystemPath::Special {
                                value: FileSystemSpecialPath::CurrentWorkingDirectory,
                            },
                            access: FileSystemAccessMode::Read,
                        });
                        if *include_platform_defaults {
                            entries.push(FileSystemSandboxEntry {
                                path: FileSystemPath::Special {
                                    value: FileSystemSpecialPath::Minimal,
                                },
                                access: FileSystemAccessMode::Read,
                            });
                        }
                        entries.extend(readable_roots.iter().cloned().map(|path| {
                            FileSystemSandboxEntry {
                                path: FileSystemPath::Path { path },
                                access: FileSystemAccessMode::Read,
                            }
                        }));
                    }
                }
                FileSystemSandboxPolicy::restricted(entries)
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
                    ReadOnlyAccess::FullAccess => entries.push(FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::Root,
                        },
                        access: FileSystemAccessMode::Read,
                    }),
                    ReadOnlyAccess::Restricted {
                        include_platform_defaults,
                        readable_roots,
                    } => {
                        if *include_platform_defaults {
                            entries.push(FileSystemSandboxEntry {
                                path: FileSystemPath::Special {
                                    value: FileSystemSpecialPath::Minimal,
                                },
                                access: FileSystemAccessMode::Read,
                            });
                        }
                        entries.extend(readable_roots.iter().cloned().map(|path| {
                            FileSystemSandboxEntry {
                                path: FileSystemPath::Path { path },
                                access: FileSystemAccessMode::Read,
                            }
                        }));
                    }
                }

                entries.push(FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::CurrentWorkingDirectory,
                    },
                    access: FileSystemAccessMode::Write,
                });
                if !exclude_slash_tmp {
                    entries.push(FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::SlashTmp,
                        },
                        access: FileSystemAccessMode::Write,
                    });
                }
                if !exclude_tmpdir_env_var {
                    entries.push(FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::Tmpdir,
                        },
                        access: FileSystemAccessMode::Write,
                    });
                }
                entries.extend(
                    writable_roots
                        .iter()
                        .cloned()
                        .map(|path| FileSystemSandboxEntry {
                            path: FileSystemPath::Path { path },
                            access: FileSystemAccessMode::Write,
                        }),
                );
                FileSystemSandboxPolicy::restricted(entries)
            }
        }
    }
}
