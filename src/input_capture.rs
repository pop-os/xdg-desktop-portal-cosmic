use crate::{PortalResponse, Session};
use std::{
    collections::HashMap,
    env,
    os::{fd::OwnedFd, unix::net::UnixStream},
};
use zbus::{object_server::SignalEmitter, zvariant};

// Capability flags
const CAPABILITY_KEYBOARD: u32 = 1;
const CAPABILITY_POINTER: u32 = 2;
#[allow(dead_code)]
const CAPABILITY_TOUCHSCREEN: u32 = 4;

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct CreateSessionResult {
    session_handle: String,
    capabilities: u32,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct GetZonesResult {
    zones: Vec<Zone>,
    zone_set: u32,
}

#[derive(zvariant::SerializeDict, zvariant::Type, Clone)]
#[zvariant(signature = "a{sv}")]
struct Zone {
    width: u32,
    height: u32,
    x: i32,
    y: i32,
}

#[allow(dead_code)]
#[derive(zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct Barrier {
    barrier_id: u32,
    position: (i32, i32, i32, i32), // x1, y1, x2, y2
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct SetPointerBarriersResult {
    failed_barriers: Vec<u32>,
}

#[allow(dead_code)]
struct InputCaptureSession {
    capabilities: u32,
    zones: Vec<Zone>,
    enabled: bool,
    active: bool,
}

impl Default for InputCaptureSession {
    fn default() -> Self {
        Self {
            capabilities: CAPABILITY_KEYBOARD | CAPABILITY_POINTER,
            zones: Vec::new(),
            enabled: false,
            active: false,
        }
    }
}

pub struct InputCapture;

#[zbus::interface(name = "org.freedesktop.impl.portal.InputCapture")]
impl InputCapture {
    /// Create a new input capture session
    async fn create_session(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        _handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        options: HashMap<String, zvariant::OwnedValue>,
    ) -> PortalResponse<CreateSessionResult> {
        log::info!("InputCapture: CreateSession for app {}", app_id);

        // Get requested capabilities from options, default to keyboard + pointer
        let capabilities = options
            .get("capabilities")
            .and_then(|v| v.downcast_ref::<u32>().ok())
            .unwrap_or(CAPABILITY_KEYBOARD | CAPABILITY_POINTER);

        let session = InputCaptureSession {
            capabilities,
            ..Default::default()
        };

        // Register the session object on DBus
        if let Err(e) = connection
            .object_server()
            .at(&session_handle, Session::new(session, |_| {}))
            .await
        {
            log::error!("Failed to create session: {}", e);
            return PortalResponse::Other;
        }

        PortalResponse::Success(CreateSessionResult {
            session_handle: session_handle.to_string(),
            capabilities,
        })
    }

    /// Get the zones (screens/monitors) available for input capture
    async fn get_zones(
        &self,
        #[zbus(connection)] _connection: &zbus::Connection,
        _handle: zvariant::ObjectPath<'_>,
        _session_handle: zvariant::ObjectPath<'_>,
        _app_id: String,
        _options: HashMap<String, zvariant::OwnedValue>,
    ) -> PortalResponse<GetZonesResult> {
        log::info!("InputCapture: GetZones");

        // TODO: Get actual monitor geometry from cosmic-comp
        // For now, return a placeholder zone
        let zones = vec![Zone {
            width: 2560,  // TODO: Get from compositor
            height: 1440,
            x: 0,
            y: 0,
        }];

        PortalResponse::Success(GetZonesResult {
            zones,
            zone_set: 1, // Increment when zones change
        })
    }

    /// Set pointer barriers that trigger input capture when crossed
    async fn set_pointer_barriers(
        &self,
        #[zbus(connection)] _connection: &zbus::Connection,
        _handle: zvariant::ObjectPath<'_>,
        _session_handle: zvariant::ObjectPath<'_>,
        _app_id: String,
        _options: HashMap<String, zvariant::OwnedValue>,
        barriers: Vec<HashMap<String, zvariant::OwnedValue>>,
        _zone_set: u32,
    ) -> PortalResponse<SetPointerBarriersResult> {
        log::info!("InputCapture: SetPointerBarriers with {} barriers", barriers.len());

        // TODO: Register barriers with cosmic-comp
        // For now, accept all barriers
        PortalResponse::Success(SetPointerBarriersResult {
            failed_barriers: Vec::new(),
        })
    }

    /// Enable input capture - barriers become active
    async fn enable(
        &self,
        #[zbus(connection)] _connection: &zbus::Connection,
        _session_handle: zvariant::ObjectPath<'_>,
        _app_id: String,
        _options: HashMap<String, zvariant::OwnedValue>,
    ) -> PortalResponse<HashMap<String, zvariant::OwnedValue>> {
        log::info!("InputCapture: Enable");

        // TODO: Tell cosmic-comp to start monitoring barriers
        PortalResponse::Success(HashMap::new())
    }

    /// Disable input capture - barriers become inactive
    async fn disable(
        &self,
        #[zbus(connection)] _connection: &zbus::Connection,
        _session_handle: zvariant::ObjectPath<'_>,
        _app_id: String,
        _options: HashMap<String, zvariant::OwnedValue>,
    ) -> PortalResponse<HashMap<String, zvariant::OwnedValue>> {
        log::info!("InputCapture: Disable");

        // TODO: Tell cosmic-comp to stop monitoring barriers
        PortalResponse::Success(HashMap::new())
    }

    /// Release captured input back to the compositor
    async fn release(
        &self,
        #[zbus(connection)] _connection: &zbus::Connection,
        _session_handle: zvariant::ObjectPath<'_>,
        _app_id: String,
        _options: HashMap<String, zvariant::OwnedValue>,
    ) -> PortalResponse<HashMap<String, zvariant::OwnedValue>> {
        log::info!("InputCapture: Release");

        // TODO: Release input capture, emit Deactivated signal
        PortalResponse::Success(HashMap::new())
    }

    /// Connect to EIS (Emulated Input Server) for receiving captured events
    async fn connect_to_eis(
        &self,
        #[zbus(connection)] _connection: &zbus::Connection,
        _session_handle: zvariant::ObjectPath<'_>,
        _app_id: String,
        _options: HashMap<String, zvariant::OwnedValue>,
    ) -> zbus::fdo::Result<zvariant::OwnedFd> {
        log::info!("InputCapture: ConnectToEIS");

        // Connect to libei socket
        // The compositor should expose this via LIBEI_SOCKET env var
        if let Ok(path) = env::var("LIBEI_SOCKET") {
            if let Ok(socket) = UnixStream::connect(&path) {
                log::info!("Connected to EIS at {}", path);
                return Ok(OwnedFd::from(socket).into());
            }
        }

        // Try common socket paths
        if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
            let eis_path = format!("{}/eis-0", runtime_dir);
            if let Ok(socket) = UnixStream::connect(&eis_path) {
                log::info!("Connected to EIS at {}", eis_path);
                return Ok(OwnedFd::from(socket).into());
            }
        }

        log::error!("Failed to connect to EIS socket");
        Err(zbus::fdo::Error::Failed("No EIS socket available".into()))
    }

    // Signals - these need to be emitted by the compositor when appropriate

    #[zbus(signal)]
    async fn disabled(
        _signal_ctxt: &SignalEmitter<'_>,
        _session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn activated(
        _signal_ctxt: &SignalEmitter<'_>,
        _session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        _signal_ctxt: &SignalEmitter<'_>,
        _session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn zones_changed(
        _signal_ctxt: &SignalEmitter<'_>,
        _session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, zvariant::OwnedValue>,
    ) -> zbus::Result<()>;

    // Properties

    #[zbus(property)]
    async fn supported_capabilities(&self) -> u32 {
        CAPABILITY_KEYBOARD | CAPABILITY_POINTER
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        1
    }
}
