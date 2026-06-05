use crate::{PortalResponse, Session};
use std::{
    collections::HashMap,
    os::{fd::OwnedFd, unix::net::UnixStream},
};
use zbus::zvariant;

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
    streams: Vec<(u32, HashMap<String, zvariant::OwnedValue>)>,
}

struct SessionData {}

pub struct RemoteDesktop;

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
            .at(&session_handle, Session::new(SessionData {}, |_| {}))
            .await
            .unwrap(); // XXX unwrap
        PortalResponse::Success(CreateSessionResult {
            session_id: "foo".to_string(), // XXX
        })
    }

    // CreateSession
    async fn select_devices(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: SelectDevicesOptions, // XXX
    ) -> PortalResponse<HashMap<String, zvariant::OwnedValue>> {
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
        PortalResponse::Success(StartResult {
            devices: 7,
            clipboard_enabled: false,
            streams: Vec::new(),
        })
    }

    async fn connect_to_EIS(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> zbus::fdo::Result<zvariant::OwnedFd> {
        let proxy = zbus::proxy::Builder::<'_, zbus::proxy::Proxy<'_>>::new(connection)
            .destination("com.system76.CosmicComp")?
            .path("/com/system76/CosmicComp/Ei")?
            .interface("com.system76.CosmicComp.Ei")?
            .build()
            .await?;
        let path: String = proxy.call("GetSocketPath", &()).await?;
        let socket = UnixStream::connect(&path)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to connect to EIS: {e}")))?;
        Ok(OwnedFd::from(socket).into())
    }

    // TODO: Notify*

    #[zbus(property)]
    async fn available_device_types(&self) -> u32 {
        7 // XXX
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        2
    }
}
