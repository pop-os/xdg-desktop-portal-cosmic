#![allow(dead_code, unused_variables)]

use futures::stream::{FuturesOrdered, StreamExt};
use std::{
    collections::HashMap,
    mem,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::screencast_dialog;
use crate::screencast_thread::ScreencastThread;
use crate::subscription;
use crate::wayland::{CaptureSource, WaylandHelper};
use crate::PortalResponse;

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

#[derive(zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct SelectSourcesOptions {
    // Default: monitor
    types: Option<u32>,
    // Default: false
    multiple: Option<bool>,
    cursor_mode: Option<u32>,
    restore_data: Option<(String, u32, zvariant::OwnedValue)>,
    // Default: 0
    persist_mode: Option<u32>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StartResult {
    streams: Vec<(u32, HashMap<String, zvariant::OwnedValue>)>,
    persist_mode: Option<u32>,
    restore_data: Option<(String, u32, zvariant::OwnedValue)>,
}

#[derive(Default)]
struct SessionData {
    screencast_threads: Vec<ScreencastThread>,
    cursor_mode: Option<u32>,
    multiple: bool,
    closed: bool,
}

impl SessionData {
    fn close(&mut self) {
        for thread in mem::take(&mut self.screencast_threads) {
            thread.stop();
        }
        self.closed = true
        // XXX Remove from hashmap?
    }
}

pub struct ScreenCast {
    sessions: Mutex<HashMap<zvariant::ObjectPath<'static>, Arc<Mutex<SessionData>>>>,
    wayland_helper: WaylandHelper,
    tx: Sender<subscription::Event>,
}

impl ScreenCast {
    pub fn new(wayland_helper: WaylandHelper, tx: Sender<subscription::Event>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            wayland_helper,
            tx,
        }
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
        let session_data = Arc::new(Mutex::new(SessionData::default()));
        self.sessions
            .lock()
            .unwrap()
            .insert(session_handle.to_owned(), session_data.clone());
        let destroy_session = move || session_data.lock().unwrap().close();
        connection
            .object_server()
            .at(&session_handle, crate::Session::new(destroy_session))
            .await
            .unwrap(); // XXX unwrap
        PortalResponse::Success(CreateSessionResult {
            session_id: "foo".to_string(), // XXX
        })
    }

    async fn select_sources(
        &self,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: SelectSourcesOptions,
    ) -> PortalResponse<HashMap<String, zvariant::OwnedValue>> {
        // TODO: Handle other options
        // TODO: Prompt what monitor to record?
        match self.sessions.lock().unwrap().get(&session_handle) {
            Some(session_data) => {
                let mut session_data = session_data.lock().unwrap();
                session_data.cursor_mode = options.cursor_mode;
                session_data.multiple = options.multiple.unwrap_or(false);
                PortalResponse::Success(HashMap::new())
            }
            None => PortalResponse::Other,
        }
    }

    async fn start(
        &self,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> PortalResponse<StartResult> {
        let session_data = match self.sessions.lock().unwrap().get(&session_handle) {
            Some(session_data) => session_data.clone(),
            None => {
                return PortalResponse::Other;
            }
        };

        let (cursor_mode, multiple) = {
            let session_data = session_data.lock().unwrap();
            let cursor_mode = session_data.cursor_mode.unwrap_or(CURSOR_MODE_HIDDEN);
            let multiple = session_data.multiple;
            (cursor_mode, multiple)
        };

        // XXX
        let mut outputs = self.wayland_helper.outputs();
        if outputs.is_empty() {
            log::error!("No output");
            return PortalResponse::Other;
        }

        // Show dialog to prompt for what to capture
        let outputs = self
            .wayland_helper
            .outputs()
            .iter()
            .filter_map(|o| Some((o.clone(), self.wayland_helper.output_info(o)?)))
            .collect();
        let toplevels = self.wayland_helper.toplevels();
        let Some(capture_sources) = screencast_dialog::show_screencast_prompt(
            &self.tx,
            app_id,
            outputs,
            toplevels,
            &self.wayland_helper,
        )
        .await
        else {
            return PortalResponse::Cancelled;
        };

        let overlay_cursor = cursor_mode == CURSOR_MODE_EMBEDDED;
        // Use `FuturesOrdered` so streams are in consistent order
        let mut res_futures = FuturesOrdered::new();
        for output in capture_sources.outputs {
            res_futures.push_back(ScreencastThread::new(
                self.wayland_helper.clone(),
                CaptureSource::Output(output),
                overlay_cursor,
            ));
        }
        for toplevel in capture_sources.toplevels {
            res_futures.push_back(ScreencastThread::new(
                self.wayland_helper.clone(),
                CaptureSource::Toplevel(toplevel),
                overlay_cursor,
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
        let mut session_data = session_data.lock().unwrap();
        if session_data.closed {
            for thread in screencast_threads {
                thread.stop();
            }
            return PortalResponse::Cancelled;
        }

        let streams = screencast_threads
            .iter()
            .map(|thread| (thread.node_id(), HashMap::new()))
            .collect();
        session_data.screencast_threads = screencast_threads;

        PortalResponse::Success(StartResult {
            // XXX
            streams,
            persist_mode: None,
            restore_data: None,
        })
    }

    #[zbus(property)]
    async fn available_source_types(&self) -> u32 {
        // XXX
        SOURCE_TYPE_MONITOR
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
