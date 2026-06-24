use crate::screencast::{
    self, CaptureOutcome, PersistedCaptureSources, RestoreData, SessionData, StreamProps,
};
use crate::wayland::WaylandHelper;
use crate::{
    PortalResponse, Request, Session, remote_desktop_dialog, remote_desktop_ei, subscription,
};
use remote_desktop_ei::{Command, EiSender};
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
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
    restore: Option<PersistedRemoteDesktop>,
    pub(crate) ei_sender: Option<EiSender>,
    pub(crate) stream_offsets: Vec<(u32, (i32, i32))>,
}

impl Default for RemoteDesktopData {
    fn default() -> Self {
        Self {
            device_types: ALL_DEVICE_TYPES,
            clipboard_enabled: false,
            persist_mode: PERSIST_NONE,
            granted_persist_mode: PERSIST_NONE,
            screen_cast_enabled: false,
            restore: None,
            ei_sender: None,
            stream_offsets: Vec::new(),
        }
    }
}

/// Private payload of a RemoteDesktop `("COSMIC", 1, _)` restore blob.
struct PersistedRemoteDesktop {
    device_types: u32,
    clipboard_enabled: bool,
    screen_cast_enabled: bool,
    sources: PersistedCaptureSources,
}

impl From<PersistedRemoteDesktop> for RestoreData {
    fn from(p: PersistedRemoteDesktop) -> RestoreData {
        RestoreData::cosmic_v1(zvariant::Structure::from((
            p.device_types,
            p.clipboard_enabled,
            p.screen_cast_enabled,
            p.sources.outputs,
            p.sources.toplevels,
        )))
    }
}

impl TryFrom<&RestoreData> for PersistedRemoteDesktop {
    type Error = ();
    fn try_from(restore_data: &RestoreData) -> Result<Self, ()> {
        let data = restore_data.cosmic_v1_data().ok_or(())?;
        let structure = zvariant::Structure::try_from(&**data).map_err(|_| ())?;
        let (device_types, clipboard_enabled, screen_cast_enabled, outputs, toplevels) =
            structure.try_into().map_err(|_| ())?;
        Ok(PersistedRemoteDesktop {
            device_types,
            clipboard_enabled,
            screen_cast_enabled,
            sources: PersistedCaptureSources { outputs, toplevels },
        })
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
    restore_data: Option<RestoreData>,
    // Default: 0
    persist_mode: Option<u32>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StartResult {
    devices: u32,
    clipboard_enabled: bool,
    streams: Vec<(u32, StreamProps)>,
    persist_mode: u32,
    restore_data: Option<RestoreData>,
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

    async fn ei_sender(
        &self,
        connection: &zbus::Connection,
        session_handle: &zvariant::ObjectPath<'_>,
    ) -> Option<EiSender> {
        let interface = crate::session_interface::<SessionData>(connection, session_handle).await?;

        let device_types = {
            let session_data = interface.get().await;
            let remote_desktop = session_data.remote_desktop.as_ref()?;
            if let Some(sender) = remote_desktop.ei_sender.clone() {
                return Some(sender);
            }
            remote_desktop.device_types
        };

        let proxy = CosmicCompEiProxy::new(connection).await.ok()?;
        let fd = match proxy.get_sender_socket(device_types).await {
            Ok(fd) => fd,
            Err(err) => {
                log::error!("Failed to get ei sender socket: {err}");
                return None;
            }
        };
        let stream = UnixStream::from(std::os::fd::OwnedFd::from(fd));
        match EiSender::connect(stream, device_types).await {
            Ok(sender) => {
                if let Some(remote_desktop) = interface.get_mut().await.remote_desktop.as_mut() {
                    remote_desktop.ei_sender = Some(sender.clone());
                }
                Some(sender)
            }
            Err(err) => {
                log::error!("Failed to create remote desktop ei sender: {err}");
                None
            }
        }
    }

    /// The global logical offset of a stream (the captured output's position).
    /// Defaults to (0, 0) if the stream is unknown.
    async fn stream_offset(
        &self,
        connection: &zbus::Connection,
        session_handle: &zvariant::ObjectPath<'_>,
        stream: u32,
    ) -> (i32, i32) {
        let Some(interface) =
            crate::session_interface::<SessionData>(connection, session_handle).await
        else {
            return (0, 0);
        };
        let session_data = interface.get().await;
        session_data
            .remote_desktop
            .as_ref()
            .and_then(|rd| {
                rd.stream_offsets
                    .iter()
                    .find(|(node, _)| *node == stream)
                    .map(|(_, off)| *off)
            })
            .unwrap_or((0, 0))
    }

    async fn notify(
        &self,
        connection: &zbus::Connection,
        session_handle: &zvariant::ObjectPath<'_>,
        command: Command,
    ) {
        if let Some(sender) = self.ei_sender(connection, session_handle).await {
            sender.send(command);
        }
    }
}

#[allow(unused_variables)]
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
        remote_desktop.restore = options
            .restore_data
            .as_ref()
            .and_then(|restore_data| PersistedRemoteDesktop::try_from(restore_data).ok());
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
        let on_cancel =
            || remote_desktop_dialog::hide_remote_desktop_prompt(&self.tx, &session_handle);
        Request::run(connection, &handle, on_cancel, async {
            let Some(interface) =
                crate::session_interface::<SessionData>(connection, &session_handle).await
            else {
                return PortalResponse::Other;
            };

            let restore_consent = {
                let mut session_data = interface.get_mut().await;
                let Some(remote_desktop) = session_data.remote_desktop.as_mut() else {
                    return PortalResponse::Other;
                };
                remote_desktop.restore.take()
            };

            let (
                device_types,
                clipboard_enabled,
                persist_mode,
                screen_cast_enabled,
                multiple,
                source_types,
            ) = {
                let session_data = interface.get().await;
                let Some(remote_desktop) = session_data.remote_desktop.as_ref() else {
                    return PortalResponse::Other;
                };
                (
                    remote_desktop.device_types,
                    remote_desktop.clipboard_enabled,
                    remote_desktop.persist_mode,
                    remote_desktop.screen_cast_enabled,
                    session_data.multiple,
                    session_data.source_types,
                )
            };

            if screen_cast_enabled && self.wayland_helper.outputs().is_empty() {
                log::error!("No output");
                return PortalResponse::Other;
            }

            // Restore silently only if the prior consent still matches this
            // request: the same device set, the same screen-sharing intent, and
            // the saved monitors/windows still resolve. Otherwise re-prompt.
            let restored = match &restore_consent {
                Some(consent) => {
                    consent.device_types & ALL_DEVICE_TYPES == device_types
                        && consent.screen_cast_enabled == screen_cast_enabled
                        && (!screen_cast_enabled || consent.sources.resolves(&self.wayland_helper))
                }
                None => false,
            };

            // Replay the consented sources so `capture` skips its own picker too.
            if restored
                && screen_cast_enabled
                && let Some(consent) = &restore_consent
            {
                interface.get_mut().await.persisted_capture_sources = Some(consent.sources.clone());
            }

            let response = if restored {
                None
            } else {
                match remote_desktop_dialog::show_remote_desktop_prompt(
                    &self.tx,
                    &session_handle,
                    app_id.clone(),
                    device_types,
                    persist_mode,
                    screen_cast_enabled,
                    multiple,
                    source_types,
                    &self.wayland_helper,
                )
                .await
                {
                    Some(response) => Some(response),
                    None => return PortalResponse::Cancelled,
                }
            };

            if interface.get().await.closed {
                return PortalResponse::Cancelled;
            }

            let granted_persist_mode = response.as_ref().map_or(persist_mode, |r| r.persist_mode);
            if let Some(remote_desktop) = interface.get_mut().await.remote_desktop.as_mut() {
                remote_desktop.granted_persist_mode = granted_persist_mode;
            }

            // Reuse the ScreenCast.Start capture path; streams are returned here.
            let (streams, sources) = if screen_cast_enabled {
                let outcome = match response {
                    Some(response) => {
                        screencast::capture_from_sources(
                            connection,
                            &self.wayland_helper,
                            &session_handle,
                            response.capture_sources,
                        )
                        .await
                    }
                    None => {
                        screencast::capture(
                            connection,
                            &self.wayland_helper,
                            &self.tx,
                            &session_handle,
                            app_id,
                        )
                        .await
                    }
                };
                match outcome {
                    CaptureOutcome::Success(result) => {
                        (result.streams, result.sources.unwrap_or_default())
                    }
                    CaptureOutcome::Cancelled => return PortalResponse::Cancelled,
                    CaptureOutcome::Other => return PortalResponse::Other,
                }
            } else {
                (Vec::new(), PersistedCaptureSources::default())
            };

            let restore_data = (granted_persist_mode != PERSIST_NONE).then(|| {
                PersistedRemoteDesktop {
                    device_types,
                    clipboard_enabled,
                    screen_cast_enabled,
                    sources,
                }
                .into()
            });

            // Record each stream's global logical offset so absolute pointer input
            // (sent in a stream's local space) can be mapped to global coordinates.
            let stream_offsets: Vec<(u32, (i32, i32))> = streams
                .iter()
                .map(|(node, props)| (*node, props.position().unwrap_or((0, 0))))
                .collect();
            if let Some(remote_desktop) = interface.get_mut().await.remote_desktop.as_mut() {
                remote_desktop.stream_offsets = stream_offsets;
            }

            PortalResponse::Success(StartResult {
                devices: device_types,
                clipboard_enabled,
                streams,
                persist_mode: granted_persist_mode,
                restore_data,
            })
        })
        .await
    }

    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
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

    // Notify* for legacy clients that don't use ConnectToEIS.

    async fn notify_pointer_motion(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        self.notify(
            connection,
            &session_handle,
            Command::PointerMotion { dx, dy },
        )
        .await;
    }

    async fn notify_pointer_motion_absolute(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        stream: u32,
        x: f64,
        y: f64,
    ) {
        // The coordinates are in the chosen stream's (output's) local space. Resolve
        // that output's global offset so the EI sender can produce a global position.
        let offset = self
            .stream_offset(connection, &session_handle, stream)
            .await;
        self.notify(
            connection,
            &session_handle,
            Command::PointerMotionAbsolute { x, y, offset },
        )
        .await;
    }

    async fn notify_pointer_button(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        button: i32,
        state: u32,
    ) {
        self.notify(
            connection,
            &session_handle,
            Command::PointerButton { button, state },
        )
        .await;
    }

    async fn notify_pointer_axis(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        dx: f64,
        dy: f64,
    ) {
        self.notify(connection, &session_handle, Command::PointerAxis { dx, dy })
            .await;
    }

    async fn notify_pointer_axis_discrete(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        axis: u32,
        steps: i32,
    ) {
        self.notify(
            connection,
            &session_handle,
            Command::PointerAxisDiscrete { axis, steps },
        )
        .await;
    }

    async fn notify_keyboard_keycode(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        keycode: i32,
        state: u32,
    ) {
        self.notify(
            connection,
            &session_handle,
            Command::KeyboardKeycode { keycode, state },
        )
        .await;
    }

    async fn notify_keyboard_keysym(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        keysym: i32,
        state: u32,
    ) {
        self.notify(
            connection,
            &session_handle,
            Command::KeyboardKeysym { keysym, state },
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)] // signature fixed by the portal protocol
    async fn notify_touch_down(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        _stream: u32,
        slot: u32,
        x: f64,
        y: f64,
    ) {
        self.notify(
            connection,
            &session_handle,
            Command::TouchDown { slot, x, y },
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)] // signature fixed by the portal protocol
    async fn notify_touch_motion(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        _stream: u32,
        slot: u32,
        x: f64,
        y: f64,
    ) {
        self.notify(
            connection,
            &session_handle,
            Command::TouchMotion { slot, x, y },
        )
        .await;
    }

    async fn notify_touch_up(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
        slot: u32,
    ) {
        self.notify(connection, &session_handle, Command::TouchUp { slot })
            .await;
    }

    #[zbus(property)]
    async fn available_device_types(&self) -> u32 {
        ALL_DEVICE_TYPES
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        2
    }
}
