#![allow(dead_code, unused_variables)]

use ashpd::desktop::screencast::SourceType;
use ashpd::enumflags2::BitFlags;
use futures::stream::{FuturesOrdered, StreamExt};
use std::collections::HashMap;
use std::mem;
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::remote_desktop::RemoteDesktopData;
use crate::screencast_dialog::{self, CaptureSources};
use crate::screencast_thread::ScreencastThread;
use crate::wayland::{CaptureSource, WaylandHelper};
use crate::{PortalResponse, Request, subscription};

const CURSOR_MODE_HIDDEN: u32 = 1;
const CURSOR_MODE_EMBEDDED: u32 = 2;
const CURSOR_MODE_METADATA: u32 = 4;

const SOURCE_TYPE_MONITOR: u32 = 1;
const SOURCE_TYPE_WINDOW: u32 = 2;
const SOURCE_TYPE_VIRTUAL: u32 = 4;

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct CreateSessionResult {
    session_id: String,
}

#[derive(Clone, Default)]
pub(crate) struct PersistedCaptureSources {
    pub(crate) outputs: Vec<String>,
    pub(crate) toplevels: Vec<String>,
}

impl PersistedCaptureSources {
    pub(crate) fn from_capture_sources(
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

    /// Whether every persisted output and window still maps to a live source.
    pub(crate) fn resolves(&self, wayland_helper: &WaylandHelper) -> bool {
        self.to_capture_sources(wayland_helper).is_some()
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, zvariant::Type)]
#[zvariant(signature = "(suv)")]
pub(crate) struct RestoreData {
    vendor: String,
    version: u32,
    data: zvariant::OwnedValue,
}

impl RestoreData {
    /// Wrap an implementation-private structure as a `("COSMIC", 1, v)` blob.
    pub(crate) fn cosmic_v1(data: zvariant::Structure) -> Self {
        RestoreData {
            vendor: "COSMIC".to_string(),
            version: 1,
            data: zvariant::Value::from(data).try_to_owned().unwrap(),
        }
    }

    /// Return the private payload if this is a `("COSMIC", 1, _)` blob.
    pub(crate) fn cosmic_v1_data(&self) -> Option<&zvariant::OwnedValue> {
        ((&*self.vendor, self.version) == ("COSMIC", 1)).then_some(&self.data)
    }
}

impl From<PersistedCaptureSources> for RestoreData {
    fn from(sources: PersistedCaptureSources) -> RestoreData {
        RestoreData::cosmic_v1(zvariant::Structure::from((
            sources.outputs,
            sources.toplevels,
        )))
    }
}

impl TryFrom<&RestoreData> for PersistedCaptureSources {
    type Error = ();
    fn try_from(restore_data: &RestoreData) -> Result<Self, ()> {
        let data = restore_data.cosmic_v1_data().ok_or(())?;
        let structure = zvariant::Structure::try_from(&**data).map_err(|_| ())?;
        let (outputs, toplevels) = structure.try_into().map_err(|_| ())?;
        Ok(PersistedCaptureSources { outputs, toplevels })
    }
}

// TODO TryFrom

#[derive(zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct SelectSourcesOptions {
    // Default: monitor
    types: Option<u32>,
    // Default: false
    multiple: Option<bool>,
    cursor_mode: Option<u32>,
    restore_data: Option<RestoreData>,
    // Default: 0
    persist_mode: Option<u32>,
}

#[derive(Clone, zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
pub struct StreamProps {
    position: Option<(i32, i32)>,
    size: (i32, i32),
    source_type: u32,
    // TODO: Add when remote desktop portal is implemented
    mapping_id: Option<String>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StartResult {
    streams: Vec<(u32, StreamProps)>,
    persist_mode: Option<u32>,
    restore_data: Option<RestoreData>,
}

#[derive(Default)]
pub(crate) struct SessionData {
    screencast_threads: Vec<ScreencastThread>,
    cursor_mode: Option<u32>,
    pub(crate) multiple: bool,
    pub(crate) source_types: BitFlags<SourceType>,
    pub(crate) persisted_capture_sources: Option<PersistedCaptureSources>,
    pub(crate) closed: bool,
    pub(crate) remote_desktop: Option<RemoteDesktopData>,
}

impl SessionData {
    pub(crate) fn new_remote_desktop() -> Self {
        Self {
            remote_desktop: Some(RemoteDesktopData::default()),
            ..Default::default()
        }
    }

    pub(crate) fn close(&mut self) {
        for thread in mem::take(&mut self.screencast_threads) {
            thread.stop();
        }
        self.closed = true
    }
}

pub(crate) struct CaptureResult {
    pub(crate) streams: Vec<(u32, StreamProps)>,
    pub(crate) restore_data: Option<RestoreData>,
    pub(crate) sources: Option<PersistedCaptureSources>,
}

pub(crate) enum CaptureOutcome {
    Success(CaptureResult),
    Cancelled,
    Other,
}

pub(crate) async fn capture(
    connection: &zbus::Connection,
    wayland_helper: &WaylandHelper,
    tx: &Sender<subscription::Event>,
    session_handle: &zvariant::ObjectPath<'_>,
    app_id: String,
) -> CaptureOutcome {
    let Some(interface) = crate::session_interface::<SessionData>(connection, session_handle).await
    else {
        return CaptureOutcome::Other;
    };

    let (multiple, source_types, persisted_capture_sources) = {
        let session_data = interface.get().await;
        (
            session_data.multiple,
            session_data.source_types,
            session_data.persisted_capture_sources.clone(),
        )
    };

    // XXX
    let outputs = wayland_helper.outputs();
    if outputs.is_empty() {
        log::error!("No output");
        return CaptureOutcome::Other;
    }

    let capture_sources = if let Some(capture_sources) =
        persisted_capture_sources.and_then(|x| x.to_capture_sources(wayland_helper))
    {
        capture_sources
    } else {
        // Show dialog to prompt for what to capture
        let resp = screencast_dialog::show_screencast_prompt(
            tx,
            session_handle,
            app_id,
            multiple,
            source_types,
            wayland_helper,
        )
        .await;
        let Some(capture_sources) = resp else {
            return CaptureOutcome::Cancelled;
        };
        capture_sources
    };

    capture_from_sources(connection, wayland_helper, session_handle, capture_sources).await
}

pub(crate) async fn capture_from_sources(
    connection: &zbus::Connection,
    wayland_helper: &WaylandHelper,
    session_handle: &zvariant::ObjectPath<'_>,
    capture_sources: CaptureSources,
) -> CaptureOutcome {
    let Some(interface) = crate::session_interface::<SessionData>(connection, session_handle).await
    else {
        return CaptureOutcome::Other;
    };

    let cursor_mode = interface
        .get()
        .await
        .cursor_mode
        .unwrap_or(CURSOR_MODE_EMBEDDED);
    let overlay_cursor = cursor_mode == CURSOR_MODE_EMBEDDED;
    // Use `FuturesOrdered` so streams are in consistent order
    let mut res_futures = FuturesOrdered::new();
    for output in &capture_sources.outputs {
        let info = wayland_helper.output_info(output);
        let (position, size) = if let Some(info) = info {
            (info.logical_position, info.logical_size.unwrap_or((0, 0)))
        } else {
            (Some((0, 0)), (0, 0))
        };
        res_futures.push_back(ScreencastThread::new(
            wayland_helper.clone(),
            CaptureSource::Output(output.clone()),
            overlay_cursor,
            StreamProps {
                position,
                size,
                source_type: SOURCE_TYPE_MONITOR,
                mapping_id: None,
            },
        ));
    }
    let toplevel_infos = wayland_helper.toplevels();
    for foreign_toplevel in &capture_sources.toplevels {
        let info = toplevel_infos
            .iter()
            .find(|info| info.foreign_toplevel == *foreign_toplevel);
        let size = if let Some(info) = info {
            // Use size on output with greatest area
            // XXX: No way to get size of whole toplevel?
            info.geometry
                .values()
                .max_by_key(|info| info.width * info.height)
                .map_or((0, 0), |info| (info.width, info.height))
        } else {
            (0, 0)
        };
        res_futures.push_back(ScreencastThread::new(
            wayland_helper.clone(),
            CaptureSource::Toplevel(foreign_toplevel.clone()),
            overlay_cursor,
            StreamProps {
                position: None,
                size,
                source_type: SOURCE_TYPE_WINDOW,
                mapping_id: None,
            },
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

    // Stop any thread that didn't fail
    if failed {
        for thread in screencast_threads {
            thread.stop();
        }
        return CaptureOutcome::Other;
    }

    // Session may have already been cancelled
    if interface.get().await.closed {
        for thread in screencast_threads {
            thread.stop();
        }
        return CaptureOutcome::Cancelled;
    }

    let streams = screencast_threads
        .iter()
        .map(|thread| (thread.node_id(), thread.stream_props()))
        .collect();
    interface.get_mut().await.screencast_threads = screencast_threads;

    let persisted_capture_sources =
        PersistedCaptureSources::from_capture_sources(wayland_helper, &capture_sources);

    CaptureOutcome::Success(CaptureResult {
        streams,
        restore_data: persisted_capture_sources.clone().map(|x| x.into()),
        sources: persisted_capture_sources,
    })
}

pub struct ScreenCast {
    wayland_helper: WaylandHelper,
    tx: Sender<subscription::Event>,
}

impl ScreenCast {
    pub fn new(wayland_helper: WaylandHelper, tx: Sender<subscription::Event>) -> Self {
        Self { wayland_helper, tx }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCast {
    async fn create_session(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> PortalResponse<CreateSessionResult> {
        // TODO: handle
        let session_data = SessionData::default();
        connection
            .object_server()
            .at(
                &session_handle,
                crate::Session::new(session_data, |session_data| session_data.close()),
            )
            .await
            .unwrap(); // XXX unwrap
        PortalResponse::Success(CreateSessionResult {
            session_id: "foo".to_string(), // XXX
        })
    }

    async fn select_sources(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: SelectSourcesOptions,
    ) -> PortalResponse<HashMap<String, zvariant::OwnedValue>> {
        // TODO: Handle other options
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
                    if let Ok(persisted_capture_sources) = restore_data.try_into() {
                        session_data.persisted_capture_sources = Some(persisted_capture_sources);
                    } else {
                        log::warn!("unrecognized screencopy restore data: {:?}", restore_data);
                    }
                }
                // RemoteDesktop sessions capture in RemoteDesktop.Start, not here.
                if let Some(remote_desktop) = session_data.remote_desktop.as_mut() {
                    remote_desktop.screen_cast_enabled = true;
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
        let on_cancel = || screencast_dialog::hide_screencast_prompt(&self.tx, &session_handle);
        Request::run(connection, &handle, on_cancel, async {
            match capture(
                connection,
                &self.wayland_helper,
                &self.tx,
                &session_handle,
                app_id,
            )
            .await
            {
                CaptureOutcome::Success(result) => PortalResponse::Success(StartResult {
                    streams: result.streams,
                    persist_mode: None,
                    restore_data: result.restore_data,
                }),
                CaptureOutcome::Cancelled => PortalResponse::Cancelled,
                CaptureOutcome::Other => PortalResponse::Other,
            }
        })
        .await
    }

    #[zbus(property)]
    async fn available_source_types(&self) -> u32 {
        SOURCE_TYPE_MONITOR | SOURCE_TYPE_WINDOW
    }

    #[zbus(property)]
    async fn available_cursor_modes(&self) -> u32 {
        // TODO: Support metadata?
        CURSOR_MODE_HIDDEN | CURSOR_MODE_EMBEDDED
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        4
    }
}
