use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use zbus::zvariant;

use crate::screencast_thread::ScreencastThread;
use crate::wayland::WaylandHelper;
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
    screencast_thread: Option<ScreencastThread>,
    closed: bool,
}

impl SessionData {
    fn close(&mut self) {
        if let Some(screencast_thread) = self.screencast_thread.take() {
            screencast_thread.stop();
        }
        self.closed = true
        // XXX Remove from hashmap?
    }
}

pub struct ScreenCast {
    sessions: Mutex<HashMap<zvariant::ObjectPath<'static>, Arc<Mutex<SessionData>>>>,
    wayland_helper: WaylandHelper,
}

impl ScreenCast {
    pub fn new(wayland_helper: WaylandHelper) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            wayland_helper,
        }
    }
}

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.ScreenCast")]
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
        // TODO: XXX
        PortalResponse::Success(HashMap::new())
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

        let (mut exporter, output) =
            if let Some(mut exporter) = self.wayland_helper.dmabuf_exporter() {
                // XXX way to select best output? Multiple?
                if let Some(output) = self.wayland_helper.outputs().first().cloned() {
                    (exporter, output)
                } else {
                    eprintln!("No output");
                    return PortalResponse::Other;
                }
            } else {
                eprintln!("No dmabuf exporter");
                return PortalResponse::Other;
            };

        // XXX overlay cursor
        let res = ScreencastThread::new(exporter, output, false).await;

        let streams = if let Ok(screencast_thread) = res {
            let node_id = screencast_thread.node_id();
            let mut session_data = session_data.lock().unwrap();
            if session_data.closed {
                screencast_thread.stop();
                return PortalResponse::Other;
            } else {
                session_data.screencast_thread = Some(screencast_thread);
                vec![(node_id, HashMap::new())]
            }
        } else {
            // XXX handle error message?
            return PortalResponse::Other;
        };
        PortalResponse::Success(StartResult {
            // XXX
            streams,
            persist_mode: None,
            restore_data: None,
        })
    }

    #[dbus_interface(property)]
    async fn available_source_types(&self) -> u32 {
        // XXX
        SOURCE_TYPE_MONITOR
    }

    #[dbus_interface(property)]
    async fn available_cursor_modes(&self) -> u32 {
        // TODO: Support metadata?
        CURSOR_MODE_HIDDEN | CURSOR_MODE_EMBEDDED
    }

    #[dbus_interface(property, name = "version")]
    async fn version(&self) -> u32 {
        4
    }
}
