// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;

use cosmic::{iced::window, widget};
use futures::{FutureExt, TryFutureExt};
use tokio::sync::mpsc::Sender;
use zbus::{fdo, object_server::SignalContext, zvariant};

use crate::{app::CosmicPortal, fl, subscription, PortalResponse};

const POP_SHELL_DEST: &str = "com.System76.PopShell";
const POP_SHELL_PATH: &str = "/com.System76.PopShell";

/// Background portal backend
///
/// https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.Background.html
pub struct Background {
    tx: Sender<subscription::Event>,
}

impl Background {
    pub const fn new(tx: Sender<subscription::Event>) -> Self {
        Self { tx }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Background")]
impl Background {
    /// Current status on running apps
    async fn get_app_state(
        &self,
        // #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<HashMap<String, AppStatus>> {
        // TODO: Subscribe to Wayland window open events for running apps
        log::warn!("[background] GetAppState is currently unimplemented");
        Ok(HashMap::default())
    }

    /// Notifies the user that an app is running in the background
    async fn notify_background(
        &self,
        // #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(signal_context)] context: SignalContext<'_>,
        handle: zvariant::ObjectPath<'_>,
        app_id: String,
        name: String,
    ) -> PortalResponse<NotifyBackgroundResult> {
        log::debug!("[background] Request handle: {handle:?}");

        // Request only what's needed to avoid cloning and receiving the entire config
        // This is also cleaner than storing the config because it's difficult to keep it
        // updated without synch primitives and we also avoid &mut self
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let config = self
            .tx
            .send(subscription::Event::BackgroundGetAppPerm(
                app_id.clone(),
                tx,
            ))
            .inspect_err(|e| {
                log::error!("[background] Failed receiving background config from main app {e:?}")
            })
            .map_ok(|_| ConfigAppPerm::default())
            .map_err(|_| ())
            .and_then(|_| rx.recv().map(|out| out.ok_or(())))
            .await
            .unwrap_or_default();

        match config {
            // Skip dialog based on default response set in configs
            ConfigAppPerm::DefaultAllow => {
                log::debug!("[background] AUTO ALLOW {name} based on default permission");
                PortalResponse::Success(NotifyBackgroundResult {
                    result: PermissionResponse::Allow,
                })
            }
            ConfigAppPerm::DefaultDeny => {
                log::debug!("[background] AUTO DENY {name} based on default permission");
                PortalResponse::Success(NotifyBackgroundResult {
                    result: PermissionResponse::Deny,
                })
            }
            // Dialog
            ConfigAppPerm::Unset => {
                log::debug!("[background] Requesting permission for {app_id} ({name})",);

                let handle = handle.to_owned();
                let id = window::Id::unique();
                let (tx, mut rx) = tokio::sync::mpsc::channel(1);
                self.tx
                    .send(subscription::Event::Background(Args {
                        handle,
                        id,
                        app_id,
                        tx,
                    }))
                    .inspect_err(|e| {
                        log::error!("[background] Failed to send message to register permissions dialog: {e:?}")
                    })
                    .map_ok(|_| PortalResponse::<NotifyBackgroundResult>::Other)
                    .map_err(|_| ())
                    .and_then(|_| rx.recv().map(|out| out.ok_or(())))
                    .unwrap_or_else(|_| PortalResponse::Other)
                    .await
            }
            // We asked the user about this app already
            ConfigAppPerm::UserAllow => {
                log::debug!("[background] AUTO ALLOW {app_id} ({name}) based on cached response");
                PortalResponse::Success(NotifyBackgroundResult {
                    result: PermissionResponse::Allow,
                })
            }
            ConfigAppPerm::UserDeny => {
                log::debug!("[background] AUTO DENY {app_id} ({name}) based on cached response");
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
        commandline: Vec<String>,
        flags: u32,
    ) -> fdo::Result<bool> {
        log::warn!("[background] Autostart not implemented");
        Ok(enable)
    }

    /// Emitted when running applications change their state
    #[zbus(signal)]
    pub async fn running_applications_changed(context: &SignalContext<'_>) -> zbus::Result<()>;
}

/// Information on running apps
// #[derive(Clone, Debug, zvariant::SerializeDict, zvariant::Type)]
// #[zvariant(signature = "a{sv}")]
// pub struct GetAppState {
//     apps: HashMap<String, AppStatus>,
// }

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

/// Evaluated permissions from background config
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfigAppPerm {
    DefaultAllow,
    DefaultDeny,
    #[default]
    Unset,
    UserAllow,
    UserDeny,
}

/// Background permissions dialog state
#[derive(Clone, Debug)]
pub struct Args {
    pub handle: zvariant::ObjectPath<'static>,
    pub id: window::Id,
    pub app_id: String,
    tx: Sender<PortalResponse<NotifyBackgroundResult>>,
}

/// Background permissions dialog response
#[derive(Debug, Clone)]
pub enum Msg {
    Response {
        id: window::Id,
        choice: PermissionResponse,
    },
    Cancel(window::Id),
}

/// Permissions dialog
pub(crate) fn view(portal: &CosmicPortal, id: window::Id) -> cosmic::Element<Msg> {
    let name = portal
        .background_prompts
        .get(&id)
        .map(|args| args.app_id.as_str())
        // xxx What do I do here?
        .unwrap_or("Invalid window id");

    // TODO: Add cancel
    widget::dialog(fl!("bg-dialog-title"))
        .body(fl!("bg-dialog-body", appname = name))
        .icon(widget::icon::from_name("dialog-warning-symbolic").size(64))
        .primary_action(
            widget::button::suggested(fl!("allow")).on_press(Msg::Response {
                id,
                choice: PermissionResponse::Allow,
            }),
        )
        .secondary_action(
            widget::button::suggested(fl!("allow-once")).on_press(Msg::Response {
                id,
                choice: PermissionResponse::AllowOnce,
            }),
        )
        .tertiary_action(
            widget::button::destructive(fl!("deny")).on_press(Msg::Response {
                id,
                choice: PermissionResponse::Deny,
            }),
        )
        .into()
}

/// Update Background dialog args for a specific window
pub fn update_args(portal: &mut CosmicPortal, args: Args) -> cosmic::Command<crate::app::Msg> {
    if let Some(old) = portal.background_prompts.insert(args.id, args) {
        // xxx Can this even happen?
        log::trace!(
            "[background] Replaced old dialog args for (window: {:?}) (app: {}) (handle: {})",
            old.id,
            old.app_id,
            old.handle
        )
    }

    cosmic::Command::none()
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Command<crate::app::Msg> {
    match msg {
        Msg::Response { id, choice } => {
            let Some(Args {
                handle,
                id,
                app_id,
                tx,
            }) = portal.background_prompts.remove(&id)
            else {
                log::warn!("[background] Window {id:?} doesn't exist for some reason");
                return cosmic::Command::none();
            };

            log::trace!(
                "[background] User selected {choice:?} for (app: {app_id}) (handle: {handle})"
            );
            // Return result to portal handler and update the config
            cosmic::command::future(async move {
                if let Err(e) = tx
                    .send(PortalResponse::Success(NotifyBackgroundResult {
                        result: choice,
                    }))
                    .await
                {
                    log::error!("[background] Failed to send response from user to the background handler: {e:?}");
                }

                crate::app::Msg::ConfigUpdateBackground {
                    app_id,
                    choice: Some(choice),
                }
            })
        }
        Msg::Cancel(id) => {
            let Some(Args {
                handle,
                id,
                app_id,
                tx,
            }) = portal.background_prompts.remove(&id)
            else {
                log::warn!("[background] Window {id:?} doesn't exist for some reason");
                return cosmic::Command::none();
            };

            log::trace!(
                "[background] User cancelled dialog for (window: {:?}) (app: {}) (handle: {})",
                id,
                app_id,
                handle
            );
            cosmic::command::future(async move {
                if let Err(e) = tx.send(PortalResponse::Cancelled).await {
                    log::error!(
                        "[background] Failed to send cancellation response to background handler {e:?}"
                    );
                }

                crate::app::Msg::ConfigUpdateBackground {
                    app_id,
                    choice: None,
                }
            })
        }
    }
}
