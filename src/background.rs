// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{hash_map::Entry, HashMap};

use tokio::sync::mpsc::Sender;
use zbus::{object_server::SignalContext, zvariant};

use crate::{config, subscription, PortalResponse};

const POP_SHELL_DEST: &str = "com.System76.PopShell";
const POP_SHELL_PATH: &str = "/com.System76.PopShell";

/// Background portal backend
///
/// https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.Background.html
pub struct Background {
    tx: Sender<subscription::Event>,
    config: config::background::Background,
}

impl Background {
    pub fn new(tx: Sender<subscription::Event>) -> Self {
        let config = config::Config::load().0.background;
        Self { tx, config }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Background")]
impl Background {
    /// Get information on running apps
    async fn get_app_state(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        // handle: zvariant::ObjectPath<'_>,
    ) -> PortalResponse<GetAppState> {
        // TODO: How do I get running programs?
        log::warn!("[background] GetAppState is currently unimplemented");
        PortalResponse::Other
    }

    async fn notify_background(
        &mut self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        app_id: String,
        name: String,
    ) -> PortalResponse<NotifyBackgroundResult> {
        // Implementation notes
        log::debug!("[background] Request handle: {handle:?}");

        match self.config.apps.entry(app_id) {
            Entry::Vacant(entry) => {
                log::debug!(
                    "[background] Requesting permission for {} ({name})",
                    entry.key()
                );

                // TODO: Dialog for user confirmation.
                // For now, just allow all requests like GNOME
                PortalResponse::Success(NotifyBackgroundResult {
                    result: PermissionResponse::Allow,
                })
            }
            Entry::Occupied(entry) if *entry.get() => {
                log::debug!(
                    "[background] AUTO ALLOW {} ({name}) based on cached response",
                    entry.key()
                );
                PortalResponse::Success(NotifyBackgroundResult {
                    result: PermissionResponse::Allow,
                })
            }
            Entry::Occupied(entry) => {
                log::debug!(
                    "[background] AUTO DENY {} ({name}) based on cached response",
                    entry.key()
                );
                PortalResponse::Success(NotifyBackgroundResult {
                    result: PermissionResponse::Deny,
                })
            }
        }
    }

    /// Enable or disable autostart for an application
    ///
    /// Deprecated but seemingly still in use
    pub async fn enable_autostart(
        &self,
        app_id: String,
        enable: bool,
        flags: u32,
    ) -> PortalResponse<bool> {
        log::debug!("[background] Autostart not implemented");
        PortalResponse::Success(enable)
    }

    #[zbus(signal)]
    pub async fn running_applications_changed(context: &SignalContext<'_>) -> zbus::Result<()>;
}

/// Information on running apps
#[derive(Clone, Debug, zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
pub struct GetAppState {
    apps: HashMap<String, AppStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, zvariant::Type)]
#[zvariant(signature = "u")]
pub enum AppStatus {
    /// No open windows
    Background = 0,
    /// At least one opened window
    Running,
    /// In the foreground
    Active,
}

/// Result vardict for [`Background::notify_background`]
#[derive(Clone, Debug, zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
pub struct NotifyBackgroundResult {
    result: PermissionResponse,
}

/// Response for apps requesting to run in the background
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, zvariant::Type)]
#[zvariant(signature = "u")]
pub enum PermissionResponse {
    /// Background permission denied
    Deny = 0,
    /// Background permission allowed whenever asked
    Allow,
    /// Background permission allowed for a single instance
    AllowOnce,
}
