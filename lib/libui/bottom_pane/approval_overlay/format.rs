use chaos_ipc::models::MacOsAutomationPermission;
use chaos_ipc::models::MacOsContactsPermission;
use chaos_ipc::models::MacOsPreferencesPermission;
use chaos_ipc::models::PermissionProfile;
use chaos_ipc::request_permissions::RequestPermissionProfile;

pub fn format_additional_permissions_rule(
    additional_permissions: &PermissionProfile,
) -> Option<String> {
    let mut parts = Vec::new();
    if additional_permissions
        .network
        .as_ref()
        .and_then(|network| network.enabled)
        .unwrap_or(false)
    {
        parts.push("network".to_string());
    }
    if let Some(file_system) = additional_permissions.file_system.as_ref() {
        if let Some(read) = file_system.read.as_ref() {
            let reads = read
                .iter()
                .map(|path| format!("`{}`", path.display()))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("read {reads}"));
        }
        if let Some(write) = file_system.write.as_ref() {
            let writes = write
                .iter()
                .map(|path| format!("`{}`", path.display()))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("write {writes}"));
        }
    }
    if let Some(macos) = additional_permissions.macos.as_ref() {
        if !matches!(
            macos.macos_preferences,
            MacOsPreferencesPermission::ReadOnly
        ) {
            let value = match macos.macos_preferences {
                MacOsPreferencesPermission::ReadOnly => "readonly",
                MacOsPreferencesPermission::ReadWrite => "readwrite",
                MacOsPreferencesPermission::None => "none",
            };
            parts.push(format!("macOS preferences {value}"));
        }
        match &macos.macos_automation {
            MacOsAutomationPermission::All => {
                parts.push("macOS automation all".to_string());
            }
            MacOsAutomationPermission::BundleIds(bundle_ids) => {
                if !bundle_ids.is_empty() {
                    parts.push(format!("macOS automation {}", bundle_ids.join(", ")));
                }
            }
            MacOsAutomationPermission::None => {}
        }
        if macos.macos_accessibility {
            parts.push("macOS accessibility".to_string());
        }
        if macos.macos_calendar {
            parts.push("macOS calendar".to_string());
        }
        if macos.macos_reminders {
            parts.push("macOS reminders".to_string());
        }
        if !matches!(macos.macos_contacts, MacOsContactsPermission::None) {
            let value = match macos.macos_contacts {
                MacOsContactsPermission::None => "none",
                MacOsContactsPermission::ReadOnly => "readonly",
                MacOsContactsPermission::ReadWrite => "readwrite",
            };
            parts.push(format!("macOS contacts {value}"));
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

pub fn format_requested_permissions_rule(permissions: &RequestPermissionProfile) -> Option<String> {
    format_additional_permissions_rule(&permissions.clone().into())
}
