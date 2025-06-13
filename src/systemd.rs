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
pub trait Systemd1 {
    fn list_units(&self) -> Result<Vec<Unit>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, zvariant::Type)]
#[cfg_attr(test, derive(Default))]
#[zvariant(signature = "(ssssssouso)")]
pub struct Unit {
    pub name: String,
    pub description: String,
    pub load_state: LoadState,
    pub active_state: ActiveState,
    pub sub_state: SubState,
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
            .or_else(|| self.name.strip_prefix(FLATPAK_SCOPE))?
            .rsplit_once('-')
            .and_then(|(appid, pid_scope)| {
                // Check if unit name ends in `-{PID}.scope`
                _ = pid_scope.strip_suffix(".scope")?.parse::<u32>().ok()?;
                Some(appid)
            })
    }
}

/// Load state for systemd units
///
/// Source: https://github.com/systemd/systemd/blob/main/man/systemctl.xml
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, zvariant::Type)]
#[cfg_attr(test, derive(Default))]
#[zvariant(signature = "s")]
#[serde(rename_all = "kebab-case")]
pub enum LoadState {
    #[cfg_attr(test, default)]
    Stub,
    Loaded,
    NotFound,
    BadSetting,
    Error,
    Merged,
    Masked,
}

/// Sub-state for systemd units
///
/// Source: https://github.com/systemd/systemd/blob/main/man/systemctl.xml
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, zvariant::Type)]
#[cfg_attr(test, derive(Default))]
#[zvariant(signature = "s")]
#[serde(rename_all = "kebab-case")]
pub enum SubState {
    #[cfg_attr(test, default)]
    Dead,
    Active,
    Waiting,
    Running,
    Failed,
    Cleaning,
    Tentative,
    Plugged,
    Mounting,
    MountingDone,
    Mounted,
    Remounting,
    Unmounting,
    RemountingSigterm,
    RemountingSigkill,
    UnmountingSigterm,
    UnmountingSigkill,
    Stop,
    StopWatchdog,
    StopSigterm,
    StopSigkill,
    StartChown,
    Abandoned,
    Condition,
    Start,
    StartPre,
    StartPost,
    StopPre,
    StopPreSigterm,
    StopPreSigkill,
    StopPost,
    Exited,
    Reload,
    ReloadSignal,
    ReloadNotify,
    FinalWatchdog,
    FinalSigterm,
    FinalSigkill,
    DeadBeforeAutoRestart,
    FailedBeforeAutoRestart,
    DeadResourcesPinned,
    AutoRestart,
    AutoRestartQueued,
    Listening,
    Activating,
    ActivatingDone,
    Deactivating,
    DeactivatingSigterm,
    DeactivatingSigkill,
    Elapsed,
}

/// Activated state for systemd units
///
/// Source: https://github.com/systemd/systemd/blob/main/man/systemctl.xml
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, zvariant::Type)]
#[cfg_attr(test, derive(Default))]
#[zvariant(signature = "s")]
#[serde(rename_all = "kebab-case")]
pub enum ActiveState {
    Active,
    Reloading,
    #[cfg_attr(test, default)]
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Maintenance,
}

#[cfg(test)]
mod tests {
    use super::Unit;

    const APPID: &str = "com.system76.CosmicFiles";

    fn unit_with_name(name: &str) -> Unit {
        Unit {
            name: name.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn parse_appid_without_scope_fails() {
        let unit = unit_with_name(APPID);
        let name = unit.cosmic_flatpak_name();
        assert!(
            name.is_none(),
            "Only apps launched by COSMIC or Flatpak should be parsed; got: {name:?}"
        );
    }

    #[test]
    fn parse_appid_with_scope_pid() {
        let unit = unit_with_name(&format!("app-cosmic-{APPID}-1234.scope"));
        let name = unit
            .cosmic_flatpak_name()
            .expect("Should parse app launched by COSMIC");
        assert_eq!(APPID, name);
    }

    #[test]
    fn parse_appid_with_scope_no_pid_fails() {
        let unit = unit_with_name(&format!("app-cosmic-{APPID}.scope"));
        let name = unit.cosmic_flatpak_name();
        assert!(
            name.is_none(),
            "Apps launched by COSMIC/Flatpak should have a PID in its scope name"
        );
    }
}
