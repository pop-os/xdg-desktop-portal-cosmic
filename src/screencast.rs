#![allow(dead_code, unused_variables)]

use ashpd::{desktop::screencast::SourceType, enumflags2::BitFlags};
use futures::stream::{FuturesOrdered, StreamExt};
use std::{collections::HashMap, mem};
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::screencast_dialog::{self, CaptureSources};
use crate::screencast_thread::ScreencastThread;
use crate::subscription;
use crate::wayland::{CaptureSource, WaylandHelper};
use crate::{PortalResponse, Request};

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

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StreamProps {
    source_type: u32,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StartResult {
    streams: Vec<(u32, StreamProps)>,
    persist_mode: Option<u32>,
    restore_data: Option<RestoreData>,
}

#[derive(Default)]
struct SessionData {
    screencast_threads: Vec<ScreencastThread>,
    cursor_mode: Option<u32>,
    multiple: bool,
    source_types: BitFlags<SourceType>,
    persisted_capture_sources: Option<PersistedCaptureSources>,
    closed: bool,
}

impl SessionData {
    fn close(&mut self) {
        for thread in mem::take(&mut self.screencast_threads) {
            thread.stop();
        }
        self.closed = true
    }
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
            let Some(interface) =
                crate::session_interface::<SessionData>(connection, &session_handle).await
            else {
                return PortalResponse::Other;
            };

            let (cursor_mode, multiple, source_types, persisted_capture_sources) = {
                let session_data = interface.get_mut().await;
                let cursor_mode = session_data.cursor_mode.unwrap_or(CURSOR_MODE_HIDDEN);
                let multiple = session_data.multiple;
                let source_types = session_data.source_types;
                let persisted_capture_sources = session_data.persisted_capture_sources.clone();
                (
                    cursor_mode,
                    multiple,
                    source_types,
                    persisted_capture_sources,
                )
            };

            // XXX
            let outputs = self.wayland_helper.outputs();
            if outputs.is_empty() {
                log::error!("No output");
                return PortalResponse::Other;
            }

            let capture_sources = if let Some(capture_sources) =
                persisted_capture_sources.and_then(|x| x.to_capture_sources(&self.wayland_helper))
            {
                capture_sources
            } else {
                // Show dialog to prompt for what to capture
                let resp = screencast_dialog::show_screencast_prompt(
                    &self.tx,
                    &session_handle,
                    app_id,
                    multiple,
                    source_types,
                    &self.wayland_helper,
                )
                .await;
                let Some(capture_sources) = resp else {
                    return PortalResponse::Cancelled;
                };
                capture_sources
            };

            let overlay_cursor = cursor_mode == CURSOR_MODE_EMBEDDED;
            // Use `FuturesOrdered` so streams are in consistent order
            let mut res_futures = FuturesOrdered::new();
            for output in &capture_sources.outputs {
                res_futures.push_back(ScreencastThread::new(
                    self.wayland_helper.clone(),
                    CaptureSource::Output(output.clone()),
                    overlay_cursor,
                    SOURCE_TYPE_MONITOR,
                ));
            }
            for foreign_toplevel in &capture_sources.toplevels {
                res_futures.push_back(ScreencastThread::new(
                    self.wayland_helper.clone(),
                    CaptureSource::Toplevel(foreign_toplevel.clone()),
                    overlay_cursor,
                    SOURCE_TYPE_WINDOW,
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
                return PortalResponse::Other;
            }

            // Session may have already been cancelled
            if interface.get().await.closed {
                for thread in screencast_threads {
                    thread.stop();
                }
                return PortalResponse::Cancelled;
            }

            let streams = screencast_threads
                .iter()
                .map(|thread| {
                    (
                        thread.node_id(),
                        StreamProps {
                            source_type: thread.source_type(),
                        },
                    )
                })
                .collect();
            interface.get_mut().await.screencast_threads = screencast_threads;

            let persisted_capture_sources = PersistedCaptureSources::from_capture_sources(
                &self.wayland_helper,
                &capture_sources,
            );

            PortalResponse::Success(StartResult {
                streams,
                persist_mode: None,
                restore_data: persisted_capture_sources.map(|x| x.into()),
            })
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
