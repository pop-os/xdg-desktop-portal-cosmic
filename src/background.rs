// SPDX-License-Identifier: GPL-3.0-only

use std::{
    borrow::Cow,
    io,
    path::Path,
    sync::{Arc, Condvar, Mutex},
};

// use ashpd::enumflags2::{bitflags, BitFlag, BitFlags};
use cosmic::{iced::window, widget};
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1;
use futures::{FutureExt, TryFutureExt};
use tokio::{
    fs,
    io::AsyncWriteExt,
    sync::{mpsc, watch},
};
use zbus::{fdo, object_server::SignalContext, zvariant};

use crate::{
    app::CosmicPortal,
    config::{self, background::PermissionDialog},
    fl, subscription,
    wayland::WaylandHelper,
    PortalResponse,
};

/// Background portal backend
///
/// https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.Background.html
pub struct Background {
    wayland_helper: WaylandHelper,
    tx: mpsc::Sender<subscription::Event>,
    rx_conf: watch::Receiver<config::Config>,
}

impl Background {
    pub fn new(
        wayland_helper: WaylandHelper,
        tx: mpsc::Sender<subscription::Event>,
        rx_conf: watch::Receiver<config::Config>,
    ) -> Self {
        let toplevel_signal = wayland_helper.toplevel_signal();
        let toplevel_tx = tx.clone();
        std::thread::Builder::new()
            .name("background-toplevel-updates".into())
            .spawn(move || Background::toplevel_signal(toplevel_signal, toplevel_tx))
            .expect("Spawning toplevels update thread should succeed");

        Self {
            wayland_helper,
            tx,
            rx_conf,
        }
    }

    /// Trigger [`Background::running_applications_changed`] on toplevel updates
    fn toplevel_signal(signal: Arc<(Mutex<bool>, Condvar)>, tx: mpsc::Sender<subscription::Event>) {
        loop {
            let (lock, cvar) = &*signal;
            let mut updated = lock.lock().unwrap();

            log::debug!("Waiting for toplevel updates");
            while !*updated {
                updated = cvar.wait(updated).unwrap();
            }

            log::debug!("Emitting RunningApplicationsChanged in response to toplevel updates");
            debug_assert!(*updated);
            *updated = false;
            if let Err(e) = tx.blocking_send(subscription::Event::BackgroundToplevels) {
                log::warn!("Failed sending event to trigger RunningApplicationsChanged: {e:?}");
            }
        }
    }

    /// Write `desktop_entry` to path `launch_entry`.
    ///
    /// The primary purpose of this function is to ease error handling.
    async fn write_autostart(
        autostart_entry: &Path,
        desktop_entry: &freedesktop_desktop_entry::DesktopEntry<'_>,
    ) -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o644)
            .open(&autostart_entry)
            .map_ok(tokio::io::BufWriter::new)
            .await?;

        file.write_all(desktop_entry.to_string().as_bytes()).await?;
        /// Shouldn't be needed, but the file never seemed to flush to disk until I did it manually
        file.flush().await
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Background")]
impl Background {
    /// Status on running apps (active, running, or background)
    async fn get_app_state(&self) -> fdo::Result<AppStates> {
        let apps: Vec<_> = self
            .wayland_helper
            .toplevels()
            .into_iter()
            .map(|(_, info)| {
                let status = if info
                    .state
                    .contains(&zcosmic_toplevel_handle_v1::State::Activated)
                {
                    AppStatus::Active
                } else if !info.state.is_empty() {
                    AppStatus::Running
                } else {
                    // xxx Is this the correct way to determine if a program is running in the
                    // background? If a toplevel exists but isn't running, activated, et cetera,
                    // then it logically must be in the background (?)
                    AppStatus::Background
                };

                AppState {
                    app_id: info.app_id,
                    status,
                }
            })
            .collect();

        log::debug!("GetAppState returning {} toplevels", apps.len());
        #[cfg(debug_assertions)]
        log::trace!("App status: {apps:#?}");

        Ok(AppStates { apps })
    }

    /// Notifies the user that an app is running in the background
    async fn notify_background(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: String,
        name: String,
    ) -> PortalResponse<NotifyBackgroundResult> {
        log::debug!("Request handle: {handle:?}");

        // Request a copy of the config from the main app instance
        // This is also cleaner than storing the config because it's difficult to keep it
        // updated without synch primitives and we also avoid &mut self
        let config = self.rx_conf.borrow().background;

        match config.default_perm {
            // Skip dialog based on default response set in configs
            PermissionDialog::Allow => {
                log::debug!("AUTO ALLOW {name} based on default permission");
                PortalResponse::Success(NotifyBackgroundResult {
                    result: PermissionResponse::Allow,
                })
            }
            PermissionDialog::Deny => {
                log::debug!("AUTO DENY {name} based on default permission");
                PortalResponse::Success(NotifyBackgroundResult {
                    result: PermissionResponse::Deny,
                })
            }
            // Dialog
            PermissionDialog::Ask => {
                log::debug!("Requesting background permission for running app {app_id} ({name})",);

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
                        log::error!("Failed to send message to register permissions dialog: {e:?}")
                    })
                    .map_ok(|_| PortalResponse::<NotifyBackgroundResult>::Other)
                    .map_err(|_| ())
                    .and_then(|_| rx.recv().map(|out| out.ok_or(())))
                    .unwrap_or_else(|_| PortalResponse::Other)
                    .await
            }
        }
    }

    /// Enable or disable autostart for an application
    ///
    /// Deprecated in terms of the portal but seemingly still in use
    /// Spec: https://specifications.freedesktop.org/autostart-spec/latest/
    pub async fn enable_autostart(
        &self,
        appid: String,
        enable: bool,
        exec: Vec<String>,
        flags: u32,
    ) -> fdo::Result<bool> {
        log::info!(
            "{} autostart for {appid}",
            if enable { "Enabling" } else { "Disabling" }
        );

        let Some((autostart_dir, launch_entry)) = dirs::config_dir().map(|config| {
            let autostart = config.join("autostart");
            (
                autostart.clone(),
                autostart.join(format!("{appid}.desktop")),
            )
        }) else {
            return Err(fdo::Error::FileNotFound("XDG_CONFIG_HOME".into()));
        };

        if !enable {
            log::debug!("Removing autostart entry {}", launch_entry.display());
            match fs::remove_file(&launch_entry).await {
                Ok(()) => Ok(false),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    log::warn!("Service asked to disable autostart for {appid} but the entry doesn't exist");
                    Ok(false)
                }
                Err(e) => {
                    log::error!(
                        "Error removing autostart entry for {appid}\n\tPath: {}\n\tError: {e}",
                        launch_entry.display()
                    );
                    Err(fdo::Error::FileNotFound(format!(
                        "{e}: ({})",
                        launch_entry.display()
                    )))
                }
            }
        } else {
            match fs::create_dir(&autostart_dir).await {
                Ok(()) => log::debug!("Created autostart directory at {}", autostart_dir.display()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
                Err(e) => {
                    log::error!(
                        "Error creating autostart directory: {e} (app: {appid}) (dir: {})",
                        autostart_dir.display()
                    );
                    return Err(fdo::Error::IOError(format!(
                        "{e}: ({})",
                        autostart_dir.display()
                    )));
                }
            }

            let mut autostart_fde = freedesktop_desktop_entry::DesktopEntry {
                appid: Cow::Borrowed(&appid),
                path: Default::default(),
                groups: Default::default(),
                ubuntu_gettext_domain: None,
            };
            autostart_fde.add_desktop_entry("Type", "Application");
            autostart_fde.add_desktop_entry("Name", &appid);

            log::debug!("{appid} autostart command line: {exec:?}");
            let exec = match shlex::try_join(exec.iter().map(|term| term.as_str())) {
                Ok(exec) => exec,
                Err(e) => {
                    log::error!("Failed to sanitize command line for {appid}\n\tCommand: {exec:?}\n\tError: {e}");
                    return Err(fdo::Error::InvalidArgs(format!("{e}: {exec:?}")));
                }
            };
            log::debug!("{appid} sanitized autostart command line: {exec}");
            autostart_fde.add_desktop_entry("Exec", &exec);

            /// xxx Replace with enumflags later when it's added as a dependency instead of adding
            /// it now for one bit (literally)
            let dbus_activation = flags & 0x1 == 1;
            if dbus_activation {
                autostart_fde.add_desktop_entry("DBusActivatable", "true");
            }

            // GNOME and KDE both set this key
            autostart_fde.add_desktop_entry("X-Flatpak", &appid);

            Self::write_autostart(&launch_entry, &autostart_fde)
                .inspect_err(|e| {
                    log::error!(
                        "Failed to write autostart entry for {appid} to `{}`: {e}",
                        launch_entry.display()
                    );
                })
                .map_err(|e| fdo::Error::IOError(format!("{e}: {}", launch_entry.display())))
                .map_ok(|()| true)
                .await
        }
    }

    /// Emitted when running applications change their state
    #[zbus(signal)]
    pub async fn running_applications_changed(context: &SignalContext<'_>) -> zbus::Result<()>;
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

#[derive(Clone, Debug, zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct AppStates {
    apps: Vec<AppState>,
}

#[derive(Clone, Debug, zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "{sv}")]
struct AppState {
    app_id: String,
    status: AppStatus,
}

/// Result vardict for [`Background::notify_background`]
#[derive(Clone, Debug, zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
pub struct NotifyBackgroundResult {
    result: PermissionResponse,
}

/// Response for apps requesting to run in the background for [`Background::notify_background`]
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

/// Background permissions dialog state
#[derive(Clone, Debug)]
pub struct Args {
    pub handle: zvariant::ObjectPath<'static>,
    pub id: window::Id,
    pub app_id: String,
    tx: mpsc::Sender<PortalResponse<NotifyBackgroundResult>>,
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

// #[bitflags]
// #[repr(u32)]
// #[derive(Clone, Copy, Debug, PartialEq)]
// enum AutostartFlags {
//     DBus = 0x01,
// }

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
            "Replaced old dialog args for (window: {:?}) (app: {}) (handle: {})",
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
                log::warn!("Window {id:?} doesn't exist for some reason");
                return cosmic::Command::none();
            };

            log::trace!(
                "User selected {choice:?} for (app: {app_id}) (handle: {handle}) on window {id:?}"
            );
            // Return result to portal handler and update the config
            tokio::spawn(async move {
                if let Err(e) = tx
                    .send(PortalResponse::Success(NotifyBackgroundResult {
                        result: choice,
                    }))
                    .await
                {
                    log::error!(
                        "Failed to send response from user to the background handler: {e:?}"
                    );
                }
            });
        }
        Msg::Cancel(id) => {
            let Some(Args {
                handle,
                id,
                app_id,
                tx,
            }) = portal.background_prompts.remove(&id)
            else {
                log::warn!("Window {id:?} doesn't exist for some reason");
                return cosmic::Command::none();
            };

            log::trace!(
                "User cancelled dialog for (window: {:?}) (app: {}) (handle: {})",
                id,
                app_id,
                handle
            );
            tokio::spawn(async move {
                if let Err(e) = tx.send(PortalResponse::Cancelled).await {
                    log::error!("Failed to send cancellation response to background handler {e:?}");
                }
            });
        }
    }

    cosmic::Command::none()
}
