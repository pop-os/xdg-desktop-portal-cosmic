use crate::screencast::{self, CaptureOutcome, SessionData, StreamProps};
use crate::wayland::WaylandHelper;
use crate::{
    PortalResponse, Request, Session, remote_desktop_dialog, screencast_dialog, subscription,
};
use std::collections::HashMap;
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

// Device types, as defined by the RemoteDesktop portal spec.
pub(crate) const DEVICE_KEYBOARD: u32 = 1;
pub(crate) const DEVICE_POINTER: u32 = 2;
pub(crate) const DEVICE_TOUCHSCREEN: u32 = 4;
const ALL_DEVICE_TYPES: u32 = DEVICE_KEYBOARD | DEVICE_POINTER | DEVICE_TOUCHSCREEN;

// Persist modes, as defined by the RemoteDesktop portal spec.
pub(crate) const PERSIST_NONE: u32 = 0;
pub(crate) const PERSIST_WHILE_RUNNING: u32 = 1;
pub(crate) const PERSIST_UNTIL_REVOKED: u32 = 2;

pub(crate) struct RemoteDesktopData {
    pub(crate) device_types: u32,
    pub(crate) clipboard_enabled: bool,
    pub(crate) persist_mode: u32,
    pub(crate) granted_persist_mode: u32,
    pub(crate) screen_cast_enabled: bool,
}

impl Default for RemoteDesktopData {
    fn default() -> Self {
        Self {
            device_types: ALL_DEVICE_TYPES,
            clipboard_enabled: false,
            persist_mode: PERSIST_NONE,
            granted_persist_mode: PERSIST_NONE,
            screen_cast_enabled: false,
        }
    }
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct CreateSessionResult {
    session_id: String,
}

#[derive(zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct SelectDevicesOptions {
    // Default: all
    types: Option<u32>,
    restore_data: Option<(String, u32, zvariant::OwnedValue)>,
    // Default: 0
    persist_mode: Option<u32>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StartResult {
    devices: u32,
    clipboard_enabled: bool,
    streams: Vec<(u32, StreamProps)>,
}

#[zbus::proxy(
    interface = "com.system76.CosmicComp.Ei",
    default_service = "com.system76.CosmicComp",
    default_path = "/com/system76/CosmicComp/Ei"
)]
trait CosmicCompEi {
    fn get_sender_socket(&self, device_types: u32) -> zbus::Result<zvariant::OwnedFd>;
}

pub struct RemoteDesktop {
    wayland_helper: WaylandHelper,
    tx: Sender<subscription::Event>,
}

impl RemoteDesktop {
    pub fn new(wayland_helper: WaylandHelper, tx: Sender<subscription::Event>) -> Self {
        Self { wayland_helper, tx }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.RemoteDesktop")]
impl RemoteDesktop {
    async fn create_session(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> PortalResponse<CreateSessionResult> {
        connection
            .object_server()
            .at(
                &session_handle,
                Session::new(SessionData::new_remote_desktop(), |session_data| {
                    session_data.close()
                }),
            )
            .await
            .unwrap(); // XXX unwrap
        PortalResponse::Success(CreateSessionResult {
            session_id: "foo".to_string(), // XXX
        })
    }

    async fn select_devices(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: SelectDevicesOptions,
    ) -> PortalResponse<HashMap<String, zvariant::OwnedValue>> {
        let Some(interface) =
            crate::session_interface::<SessionData>(connection, &session_handle).await
        else {
            return PortalResponse::Other;
        };
        let mut session_data = interface.get_mut().await;
        let Some(remote_desktop) = session_data.remote_desktop.as_mut() else {
            return PortalResponse::Other;
        };
        remote_desktop.device_types = options.types.unwrap_or(ALL_DEVICE_TYPES) & ALL_DEVICE_TYPES;
        remote_desktop.persist_mode = options.persist_mode.unwrap_or(PERSIST_NONE);
        // TODO: restore_data
        PortalResponse::Success(HashMap::new())
    }

    async fn start(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> PortalResponse<StartResult> {
        // Dismiss whichever prompt is up: the permission dialog or the screencast picker.
        let on_cancel = || async {
            remote_desktop_dialog::hide_remote_desktop_prompt(&self.tx, &session_handle).await;
            screencast_dialog::hide_screencast_prompt(&self.tx, &session_handle).await;
        };
        Request::run(connection, &handle, on_cancel, async {
            let Some(interface) =
                crate::session_interface::<SessionData>(connection, &session_handle).await
            else {
                return PortalResponse::Other;
            };

            let (device_types, clipboard_enabled, persist_mode, screen_cast_enabled) = {
                let session_data = interface.get().await;
                let Some(remote_desktop) = session_data.remote_desktop.as_ref() else {
                    return PortalResponse::Other;
                };
                (
                    remote_desktop.device_types,
                    remote_desktop.clipboard_enabled,
                    remote_desktop.persist_mode,
                    remote_desktop.screen_cast_enabled,
                )
            };

            let resp = remote_desktop_dialog::show_remote_desktop_prompt(
                &self.tx,
                &session_handle,
                app_id.clone(),
                device_types,
                persist_mode,
            )
            .await;
            let Some(response) = resp else {
                return PortalResponse::Cancelled;
            };

            if interface.get().await.closed {
                return PortalResponse::Cancelled;
            }
            if let Some(remote_desktop) = interface.get_mut().await.remote_desktop.as_mut() {
                remote_desktop.granted_persist_mode = response.persist_mode;
            }

            // Reuse the ScreenCast.Start capture path; streams are returned here.
            let streams = if screen_cast_enabled {
                match screencast::capture(
                    connection,
                    &self.wayland_helper,
                    &self.tx,
                    &session_handle,
                    app_id,
                )
                .await
                {
                    CaptureOutcome::Success(result) => result.streams,
                    CaptureOutcome::Cancelled => return PortalResponse::Cancelled,
                    CaptureOutcome::Other => return PortalResponse::Other,
                }
            } else {
                Vec::new()
            };

            PortalResponse::Success(StartResult {
                devices: device_types,
                clipboard_enabled,
                streams,
            })
        })
        .await
    }

    async fn connect_to_EIS(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> zbus::fdo::Result<zvariant::OwnedFd> {
        let Some(interface) =
            crate::session_interface::<SessionData>(connection, &session_handle).await
        else {
            return Err(zbus::fdo::Error::Failed("No such session".to_string()));
        };
        let Some(device_types) = interface
            .get()
            .await
            .remote_desktop
            .as_ref()
            .map(|remote_desktop| remote_desktop.device_types)
        else {
            return Err(zbus::fdo::Error::Failed(
                "Not a remote desktop session".to_string(),
            ));
        };
        let proxy = CosmicCompEiProxy::new(connection).await?;
        proxy
            .get_sender_socket(device_types)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to connect to EIS: {e}")))
    }

    // TODO: Notify*

    #[zbus(property)]
    async fn available_device_types(&self) -> u32 {
        ALL_DEVICE_TYPES
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        2
    }
}
