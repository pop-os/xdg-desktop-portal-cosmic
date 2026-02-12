#![allow(dead_code, unused_variables)]

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::{io, mem};

use ashpd::desktop::screencast::SourceType;
use ashpd::enumflags2::BitFlags;
use futures::stream::{FuturesOrdered, StreamExt};
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::remote_desktop_dialog::{self, CaptureSources};
use crate::screencast::StreamProps;
use crate::screencast_thread::ScreencastThread;
use crate::subscription;
use crate::wayland::{CaptureSource, WaylandHelper};
use crate::{PortalResponse, Request};

const DEVICE_KEYBOARD: u32 = 1;
const DEVICE_POINTER: u32 = 2;
const DEVICE_TOUCHSCREEN: u32 = 4;

const CURSOR_MODE_HIDDEN: u32 = 1;
const CURSOR_MODE_EMBEDDED: u32 = 2;

const SOURCE_TYPE_MONITOR: u32 = 1;
const SOURCE_TYPE_WINDOW: u32 = 2;

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct CreateSessionResult {
    session_id: String,
}

#[derive(Clone)]
struct PersistedCaptureSources {
    pub outputs: Vec<String>,
    pub toplevels: Vec<String>,
}

impl PersistedCaptureSources {
    fn from_capture_sources(
        wayland_helper: &WaylandHelper,
        sources: &CaptureSources,
    ) -> Option<Self> {
        let mut outputs = Vec::new();
        for handle in &sources.outputs {
            let info = wayland_helper.output_info(handle)?;
            outputs.push(info.name.clone()?);
        }

        let mut toplevels = Vec::new();
        let toplevel_infos = wayland_helper.toplevels();
        for handle in &sources.toplevels {
            let info = toplevel_infos
                .iter()
                .find(|t| t.foreign_toplevel == *handle)?;
            toplevels.push(info.identifier.clone());
        }

        Some(Self { outputs, toplevels })
    }

    fn to_capture_sources(&self, wayland_helper: &WaylandHelper) -> Option<CaptureSources> {
        let mut outputs = Vec::new();
        for name in &self.outputs {
            outputs.push(wayland_helper.output_for_name(name)?);
        }

        let mut toplevels = Vec::new();
        let toplevel_infos = wayland_helper.toplevels();
        for identifier in &self.toplevels {
            let info = toplevel_infos
                .iter()
                .find(|t| t.identifier == *identifier)?;
            toplevels.push(info.foreign_toplevel.clone());
        }

        Some(CaptureSources { outputs, toplevels })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, zvariant::Type)]
#[zvariant(signature = "(suv)")]
struct RestoreData {
    vendor: String,
    version: u32,
    data: zvariant::OwnedValue,
}

impl From<PersistedCaptureSources> for RestoreData {
    fn from(sources: PersistedCaptureSources) -> RestoreData {
        RestoreData {
            vendor: "COSMIC".to_string(),
            version: 1,
            data: zvariant::Value::from(zvariant::Structure::from((
                sources.outputs,
                sources.toplevels,
            )))
            .try_to_owned()
            .unwrap(),
        }
    }
}

impl TryFrom<&RestoreData> for PersistedCaptureSources {
    type Error = ();
    fn try_from(restore_data: &RestoreData) -> Result<Self, ()> {
        if (&*restore_data.vendor, restore_data.version) != ("COSMIC", 1) {
            return Err(());
        }
        let structure = zvariant::Structure::try_from(&*restore_data.data).map_err(|_| ())?;
        let (outputs, toplevels) = structure.try_into().map_err(|_| ())?;
        Ok(PersistedCaptureSources { outputs, toplevels })
    }
}

#[derive(zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct SelectDevicesOptions {
    types: Option<u32>,
    restore_data: Option<RestoreData>,
    persist_mode: Option<u32>,
}

#[derive(zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct SelectSourcesOptions {
    types: Option<u32>,
    multiple: Option<bool>,
    cursor_mode: Option<u32>,
    restore_data: Option<RestoreData>,
    persist_mode: Option<u32>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StartResult {
    devices: u32,
    streams: Vec<(u32, StreamProps)>,
    persist_mode: Option<u32>,
    restore_data: Option<RestoreData>,
}

#[derive(Default)]
struct SessionData {
    device_types: u32,
    screencast_threads: Vec<ScreencastThread>,
    cursor_mode: Option<u32>,
    multiple: bool,
    source_types: BitFlags<SourceType>,
    persisted_capture_sources: Option<PersistedCaptureSources>,
    eis_socket_client: Option<zvariant::OwnedFd>,
    closed: bool,
}

impl SessionData {
    fn close(&mut self) {
        for thread in mem::take(&mut self.screencast_threads) {
            thread.stop();
        }
        self.eis_socket_client.take();
        self.closed = true;
    }
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
        let session_data = SessionData::default();
        if let Err(err) = connection
            .object_server()
            .at(
                &session_handle,
                crate::Session::new(session_data, |session_data| session_data.close()),
            )
            .await
        {
            log::error!("Failed to register session object: {}", err);
            return PortalResponse::Other;
        }
        PortalResponse::Success(CreateSessionResult {
            session_id: session_handle
                .as_str()
                .rsplit('/')
                .next()
                .unwrap_or("remote")
                .to_string(),
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
        match crate::session_interface::<SessionData>(connection, &session_handle).await {
            Some(interface) => {
                let mut session_data = interface.get_mut().await;
                // Mask to valid device type bits only (keyboard, pointer, touchscreen).
                let raw_types = options.types.unwrap_or(DEVICE_KEYBOARD | DEVICE_POINTER);
                session_data.device_types =
                    raw_types & (DEVICE_KEYBOARD | DEVICE_POINTER | DEVICE_TOUCHSCREEN);
                if let Some(restore_data) = &options.restore_data {
                    if let Ok(persisted) = restore_data.try_into() {
                        session_data.persisted_capture_sources = Some(persisted);
                    } else {
                        log::warn!("unrecognized remote desktop restore data: {:?}", restore_data);
                    }
                }
                PortalResponse::Success(HashMap::new())
            }
            None => PortalResponse::Other,
        }
    }

    async fn select_sources(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: SelectSourcesOptions,
    ) -> PortalResponse<HashMap<String, zvariant::OwnedValue>> {
        match crate::session_interface::<SessionData>(connection, &session_handle).await {
            Some(interface) => {
                let mut session_data = interface.get_mut().await;
                session_data.cursor_mode = options.cursor_mode;
                session_data.multiple = options.multiple.unwrap_or(false);
                session_data.source_types =
                    BitFlags::from_bits_truncate(options.types.unwrap_or(0));
                if session_data.source_types.is_empty() {
                    session_data.source_types = SourceType::Monitor.into();
                }
                if let Some(restore_data) = &options.restore_data {
                    if let Ok(persisted) = restore_data.try_into() {
                        session_data.persisted_capture_sources = Some(persisted);
                    } else {
                        log::warn!("unrecognized remote desktop restore data: {:?}", restore_data);
                    }
                }
                PortalResponse::Success(HashMap::new())
            }
            None => PortalResponse::Other,
        }
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

            let (device_types, cursor_mode, multiple, source_types) = {
                let session_data = interface.get_mut().await;
                let device_types = session_data.device_types;
                let cursor_mode = session_data.cursor_mode.unwrap_or(CURSOR_MODE_HIDDEN);
                let multiple = session_data.multiple;
                let source_types = session_data.source_types;
                (device_types, cursor_mode, multiple, source_types)
            };

            let outputs = self.wayland_helper.outputs();
            if outputs.is_empty() {
                log::error!("No output");
                return PortalResponse::Other;
            }

            // Always show consent dialog for RemoteDesktop sessions.
            // Unlike ScreenCast (view-only), RemoteDesktop grants input injection
            // (keyboard/mouse/touch), which is too sensitive to skip consent based
            // on restore data alone — output names are guessable and restore data
            // can be crafted by any D-Bus client.
            let capture_sources = {
                let resp = remote_desktop_dialog::show_remote_desktop_prompt(
                    &self.tx,
                    &session_handle,
                    app_id,
                    device_types,
                    !source_types.is_empty(),
                    multiple,
                    source_types,
                    &self.wayland_helper,
                )
                .await;
                let Some(capture_sources) = resp else {
                    log::info!("Remote desktop access denied by user");
                    return PortalResponse::Cancelled;
                };
                capture_sources
            };

            // Set up screencast threads for video (reuse ScreenCast infra)
            let overlay_cursor = cursor_mode == CURSOR_MODE_EMBEDDED;
            let mut res_futures = FuturesOrdered::new();
            for output in &capture_sources.outputs {
                let info = self.wayland_helper.output_info(output);
                let (position, size) = if let Some(info) = info {
                    (info.logical_position, info.logical_size.unwrap_or((0, 0)))
                } else {
                    (Some((0, 0)), (0, 0))
                };
                res_futures.push_back(ScreencastThread::new(
                    self.wayland_helper.clone(),
                    CaptureSource::Output(output.clone()),
                    overlay_cursor,
                    StreamProps::new(position, size, SOURCE_TYPE_MONITOR),
                ));
            }
            let toplevel_infos = self.wayland_helper.toplevels();
            for foreign_toplevel in &capture_sources.toplevels {
                let info = toplevel_infos
                    .iter()
                    .find(|info| info.foreign_toplevel == *foreign_toplevel);
                let size = if let Some(info) = info {
                    info.geometry
                        .values()
                        .max_by_key(|info| info.width * info.height)
                        .map_or((0, 0), |info| (info.width, info.height))
                } else {
                    (0, 0)
                };
                res_futures.push_back(ScreencastThread::new(
                    self.wayland_helper.clone(),
                    CaptureSource::Toplevel(foreign_toplevel.clone()),
                    overlay_cursor,
                    StreamProps::new(None, size, SOURCE_TYPE_WINDOW),
                ));
            }

            let mut failed = false;
            let mut screencast_threads = Vec::new();
            while let Some(res) = res_futures.next().await {
                match res {
                    Ok(thread) => screencast_threads.push(thread),
                    Err(err) => {
                        log::error!("Screencast thread failed: {}", err);
                        failed = true;
                    }
                }
            }

            if failed {
                for thread in screencast_threads {
                    thread.stop();
                }
                return PortalResponse::Other;
            }

            if interface.get().await.closed {
                for thread in screencast_threads {
                    thread.stop();
                }
                return PortalResponse::Cancelled;
            }

            // Create EIS socket pair for input injection
            let eis_client_fd = match create_eis_socket_pair() {
                Ok((server_fd, client_fd)) => {
                    // Forward the server-side fd to the compositor via D-Bus
                    // Must use the portal's own connection so the compositor's
                    // auth check sees the well-known name owner match.
                    if let Err(err) = send_eis_to_compositor(connection, server_fd).await {
                        log::warn!("Failed to send EIS socket to compositor: {}", err);
                    }
                    Some(client_fd)
                }
                Err(err) => {
                    log::error!("Failed to create EIS socket pair: {}", err);
                    None
                }
            };

            let streams = screencast_threads
                .iter()
                .map(|thread| (thread.node_id(), thread.stream_props()))
                .collect();

            {
                let mut session_data = interface.get_mut().await;
                session_data.screencast_threads = screencast_threads;
                session_data.eis_socket_client = eis_client_fd;
            }

            PortalResponse::Success(StartResult {
                devices: device_types,
                streams,
                persist_mode: None,
                // Never return restore data for RemoteDesktop sessions.
                // Input injection is too sensitive to allow any restore-based shortcuts.
                restore_data: None,
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
            return Err(zbus::fdo::Error::Failed(
                "EIS connection unavailable".to_string(),
            ));
        };

        let mut session_data = interface.get_mut().await;
        if session_data.closed {
            return Err(zbus::fdo::Error::Failed(
                "EIS connection unavailable".to_string(),
            ));
        }
        match session_data.eis_socket_client.take() {
            Some(fd) => Ok(fd),
            None => Err(zbus::fdo::Error::Failed(
                "EIS connection unavailable".to_string(),
            )),
        }
    }

    #[zbus(property)]
    fn available_device_types(&self) -> u32 {
        DEVICE_KEYBOARD | DEVICE_POINTER
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

fn create_eis_socket_pair() -> io::Result<(std::os::fd::OwnedFd, zvariant::OwnedFd)> {
    let (server, client) = UnixStream::pair()?;
    Ok((
        std::os::fd::OwnedFd::from(server),
        zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(client)),
    ))
}

/// Send the server-side EIS socket fd to the compositor via D-Bus.
///
/// Uses the portal's own D-Bus connection so the compositor's auth check
/// can verify the caller owns the portal's well-known name.
async fn send_eis_to_compositor(
    connection: &zbus::Connection,
    server_fd: std::os::fd::OwnedFd,
) -> anyhow::Result<()> {
    let proxy = zbus::Proxy::new(
        connection,
        "com.system76.CosmicComp.RemoteDesktop",
        "/com/system76/CosmicComp",
        "com.system76.CosmicComp.RemoteDesktop",
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to create compositor proxy: {}", e))?;

    proxy
        .call_method("AcceptEisSocket", &(zvariant::Fd::from(server_fd),))
        .await
        .map_err(|e| anyhow::anyhow!("D-Bus call to compositor failed: {}", e))?;
    log::info!("Successfully sent EIS socket to compositor");
    Ok(())
}
