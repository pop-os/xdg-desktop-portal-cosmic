use std::collections::HashMap;
use zbus::zvariant;

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

pub struct ScreenCast;

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCast {
    async fn create_session(
        &self,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> (u32, CreateSessionResult) {
        (
            crate::PORTAL_RESPONSE_SUCCESS,
            CreateSessionResult {
                session_id: "foo".to_string(), // XXX
            },
        )
    }

    async fn select_sources(
        &self,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: SelectSourcesOptions,
    ) -> (u32, HashMap<String, zvariant::OwnedValue>) {
        // TODO: XXX
        (crate::PORTAL_RESPONSE_SUCCESS, HashMap::new())
    }

    async fn start(
        &self,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> (u32, StartResult) {
        (
            crate::PORTAL_RESPONSE_SUCCESS,
            StartResult {
                // XXX
                streams: Vec::new(),
                persist_mode: None,
                restore_data: None,
            },
        )
    }

    #[dbus_interface(property)]
    async fn available_source_types(&self) -> u32 {
        // XXX
        SOURCE_TYPE_MONITOR
    }

    #[dbus_interface(property)]
    async fn available_cursor_modes(&self) -> u32 {
        // XXX
        CURSOR_MODE_HIDDEN
    }

    #[dbus_interface(property)]
    async fn version(&self) -> u32 {
        4
    }
}
