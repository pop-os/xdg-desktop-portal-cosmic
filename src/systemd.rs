// SPDX-License-Identifier: GPL-3.0-only

use serde::Deserialize;
use zbus::{zvariant, Result};

static COSMIC_SCOPE: &str = "app-cosmic-";
static FLATPAK_SCOPE: &str = "app-flatpak-";

/// Proxy for the `org.freedesktop.systemd1.Manager` interface
#[zbus::proxy(
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1",
    interface = "org.freedesktop.systemd1.Manager"
)]
trait Systemd1 {
    fn list_units(&self) -> Result<Vec<Unit>>;
}

#[derive(Debug, Clone, PartialEq, Eq, zvariant::Type, zvariant::DeserializeDict)]
#[zvariant(signature = "a(ssssssouso)")]
pub struct Unit {
    pub name: String,
    pub description: String,
    pub load_state: LoadState,
    pub active_state: ActiveState,
    pub sub_state: String,
    pub following: String,
    pub unit_object: zvariant::OwnedObjectPath,
    pub job_id: u32,
    pub job_type: String,
    pub job_object: zvariant::OwnedObjectPath,
}

impl Unit {
    /// Returns appid if COSMIC or Flatpak launched this unit
    pub fn cosmic_flatpak_name(&self) -> Option<&str> {
        self.name
            .strip_prefix(COSMIC_SCOPE)
            .or_else(|| self.name.strip_prefix(FLATPAK_SCOPE))
            .and_then(|with_scope| with_scope.strip_suffix(".scope"))
    }
}

/// Load state for systemd units
///
/// Source: https://github.com/systemd/systemd/blob/main/man/systemctl.xml
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, zvariant::Type)]
#[zvariant(signature = "s", rename_all = "kebab-case")]
pub enum LoadState {
    Stub,
    Loaded,
    NotFound,
    BadSetting,
    Error,
    Merged,
    Masked,
}

/// Activated state for systemd units
///
/// Source: https://github.com/systemd/systemd/blob/main/man/systemctl.xml
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, zvariant::Type)]
#[zvariant(signature = "s", rename_all = "kebab-case")]
pub enum ActiveState {
    Active,
    Reloading,
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Maintenance,
}
