//! Implementation of the XDG InputCapture portal.
//!
//! COSMIC's compositor owns the physical input stream and the pointer-barrier
//! state. This portal interface handles the public protocol, permission UI,
//! and forwards the EIS file descriptor to the compositor through its private
//! session-bus interface.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use tokio::sync::mpsc::Sender;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{self, OwnedValue};

use crate::screencast::RestoreData;
use crate::{DBUS_PATH, PortalResponse, Request, Session, access, subscription};

pub(crate) const DEVICE_KEYBOARD: u32 = 1;
pub(crate) const DEVICE_POINTER: u32 = 2;
const SUPPORTED_DEVICE_TYPES: u32 = DEVICE_KEYBOARD | DEVICE_POINTER;
const MAX_BARRIERS: usize = 256;
type Zone = (u32, u32, i32, i32);

fn release_owned_session(owner: &Mutex<Option<String>>, handle: &str) {
    let mut current = owner.lock().unwrap();
    if current.as_deref() == Some(handle) {
        *current = None;
    }
}

#[derive(Debug, zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "dict")]
struct Barrier {
    barrier_id: u32,
    position: (i32, i32, i32, i32),
}

#[derive(Debug, zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct ZonesResult {
    zones: Vec<Zone>,
    zone_set: u32,
}

#[derive(Debug, zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct SetPointerBarriersResult {
    failed_barriers: Vec<u32>,
}

#[derive(Debug, zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StartResult {
    capabilities: u32,
    clipboard_enabled: bool,
    restore_data: Option<RestoreData>,
}

#[derive(Debug, zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct StartOptions {
    capabilities: u32,
    restore_data: Option<RestoreData>,
    persist_mode: Option<u32>,
}

/// Private restore payload. The public restore blob is never sufficient on its
/// own: PermissionStore must still contain a grant for this application.
fn restore_matches(data: &RestoreData, app_id: &str, capabilities: u32) -> bool {
    let Some(payload) = data.cosmic_v1_data() else {
        return false;
    };
    let Ok(structure) = zvariant::Structure::try_from(&**payload) else {
        return false;
    };
    let Ok((stored_app, stored_capabilities)): Result<(String, u32), _> = structure.try_into()
    else {
        return false;
    };
    stored_app == app_id && stored_capabilities == capabilities
}

fn restore_data(app_id: &str, capabilities: u32) -> RestoreData {
    RestoreData::cosmic_v1(zvariant::Structure::from((app_id, capabilities)))
}

/// The portal-side state associated with one InputCapture session.
pub(crate) struct InputCaptureData {
    connection: zbus::Connection,
    session_handle: zvariant::OwnedObjectPath,
    pub(crate) device_types: u32,
    started: bool,
    connected: bool,
    session_owner: Arc<Mutex<Option<String>>>,
}

impl InputCaptureData {
    fn new(
        connection: zbus::Connection,
        session_handle: zvariant::OwnedObjectPath,
        session_owner: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            connection,
            session_handle,
            device_types: 0,
            started: false,
            connected: false,
            session_owner,
        }
    }

    fn close(&mut self) {
        let connection = self.connection.clone();
        let session_handle = self.session_handle.to_string();
        let session_owner = self.session_owner.clone();
        tokio::spawn(async move {
            if let Ok(proxy) = CosmicCompInputCaptureProxy::new(&connection).await {
                let _ = proxy.close(&session_handle).await;
            }
            release_owned_session(&session_owner, &session_handle);
        });
    }
}

#[derive(Clone, Debug)]
pub(crate) enum CompSignal {
    Activated {
        session_handle: String,
        activation_id: u32,
        barrier_id: u32,
        cursor_position: (f64, f64),
    },
    Deactivated {
        session_handle: String,
        activation_id: u32,
        cursor_position: (f64, f64),
    },
    Disabled {
        session_handle: String,
    },
    ZonesChanged {
        session_handle: String,
        zone_set: u32,
    },
}

#[zbus::proxy(
    interface = "com.system76.CosmicComp.InputCapture",
    default_service = "com.system76.CosmicComp",
    default_path = "/com/system76/CosmicComp/InputCapture"
)]
trait CosmicCompInputCapture {
    fn get_receiver_socket(
        &self,
        session_handle: &str,
        device_types: u32,
    ) -> zbus::Result<zvariant::OwnedFd>;

    fn get_zones(&self, session_handle: &str) -> zbus::Result<(u32, Vec<Zone>)>;

    fn set_pointer_barriers(
        &self,
        session_handle: &str,
        zone_set: u32,
        barriers: Vec<(u32, (i32, i32, i32, i32))>,
    ) -> zbus::Result<Vec<u32>>;

    fn enable(&self, session_handle: &str) -> zbus::Result<()>;
    fn disable(&self, session_handle: &str) -> zbus::Result<()>;
    fn release(
        &self,
        session_handle: &str,
        activation_id: zvariant::Optional<u32>,
        cursor_position: zvariant::Optional<(f64, f64)>,
    ) -> zbus::Result<()>;
    fn close(&self, session_handle: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn activated(
        &self,
        session_handle: &str,
        activation_id: u32,
        barrier_id: u32,
        cursor_position: (f64, f64),
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    fn deactivated(
        &self,
        session_handle: &str,
        activation_id: u32,
        cursor_position: (f64, f64),
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    fn disabled(&self, session_handle: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn zones_changed(&self, session_handle: &str, zone_set: u32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.impl.portal.PermissionStore",
    default_service = "org.freedesktop.impl.portal.PermissionStore",
    default_path = "/org/freedesktop/impl/portal/PermissionStore"
)]
trait PermissionStore {
    fn get_permission(&self, table: &str, id: &str, app: &str) -> zbus::Result<Vec<String>>;

    fn set_permission(
        &self,
        table: &str,
        create: bool,
        id: &str,
        app: &str,
        permissions: Vec<String>,
    ) -> zbus::Result<()>;
}

pub struct InputCapture {
    tx: Sender<subscription::Event>,
    session_owner: Arc<Mutex<Option<String>>>,
}

impl InputCapture {
    pub fn new(tx: Sender<subscription::Event>) -> Self {
        Self {
            tx,
            session_owner: Arc::new(Mutex::new(None)),
        }
    }

    fn claim_session(&self, session_handle: &zvariant::ObjectPath<'_>) -> bool {
        let mut owner = self.session_owner.lock().unwrap();
        if owner.is_some() {
            return false;
        }
        *owner = Some(session_handle.to_string());
        true
    }

    async fn install_session(
        &self,
        connection: &zbus::Connection,
        session_handle: &zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()> {
        connection
            .object_server()
            .at(
                session_handle,
                Session::new(
                    InputCaptureData::new(
                        connection.clone(),
                        session_handle.to_owned().into(),
                        self.session_owner.clone(),
                    ),
                    |data| data.close(),
                ),
            )
            .await?;
        Ok(())
    }

    async fn session_data(
        connection: &zbus::Connection,
        session_handle: &zvariant::ObjectPath<'_>,
    ) -> Option<zbus::object_server::InterfaceRef<Session<InputCaptureData>>> {
        crate::session_interface(connection, session_handle).await
    }
}

fn option_u32(options: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    options
        .get(key)
        .and_then(|value| value.downcast_ref::<u32>().ok())
}

fn option_pair(options: &HashMap<String, OwnedValue>, key: &str) -> Option<(f64, f64)> {
    options
        .get(key)
        .and_then(|value| value.downcast_ref::<(f64, f64)>().ok())
}

fn requested_capabilities(options: &HashMap<String, OwnedValue>) -> Option<u32> {
    let requested = option_u32(options, "capabilities")? & SUPPORTED_DEVICE_TYPES;
    (requested != 0).then_some(requested)
}

fn capabilities_result(capabilities: u32) -> HashMap<String, OwnedValue> {
    HashMap::from([("capabilities".to_string(), OwnedValue::from(capabilities))])
}

const PERMISSION_TABLE: &str = "input-capture";
const PERMISSION_ID: &str = "keyboard-and-pointer";

/// XDG's native (unsandboxed) clients have an empty app ID. Never use that
/// empty value as a persistent permission key: it is shared by every native
/// client. The frontend's session path includes the actual caller's unique
/// bus name, which we resolve to a system-owned executable for this case.
struct ConsentIdentity {
    key: Option<String>,
    label: String,
}

fn native_sender_from_session_path(path: &str) -> Option<String> {
    let suffix = path.strip_prefix("/org/freedesktop/portal/desktop/session/")?;
    let (sender, token) = suffix.split_once('/')?;
    if sender.is_empty() || token.is_empty() {
        return None;
    }
    let name = format!(":{}", sender.replace('_', "."));
    zbus::names::BusName::try_from(name.as_str()).ok()?;
    Some(name)
}

fn system_owned_executable(path: &Path) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    let mut part = Some(canonical.as_path());
    while let Some(path) = part {
        let metadata = std::fs::metadata(path).ok()?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return None;
        }
        part = path.parent();
    }
    Some(canonical)
}

fn offer_persistent_choice(requested_mode: Option<u32>, identity: &ConsentIdentity) -> bool {
    requested_mode == Some(2)
        || (requested_mode.is_none()
            && identity
                .key
                .as_deref()
                .is_some_and(|key| key.starts_with("native-exe:")))
}

async fn consent_identity(
    connection: &zbus::Connection,
    session_handle: &zvariant::ObjectPath<'_>,
    app_id: &str,
) -> ConsentIdentity {
    if !app_id.is_empty() {
        return ConsentIdentity {
            key: Some(app_id.to_string()),
            label: app_id.to_string(),
        };
    }
    let native = async {
        let sender = native_sender_from_session_path(session_handle.as_str())?;
        let bus_name = zbus::names::BusName::try_from(sender.as_str()).ok()?;
        let bus = zbus::fdo::DBusProxy::new(connection).await.ok()?;
        let pid = bus.get_connection_unix_process_id(bus_name).await.ok()?;
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        system_owned_executable(&exe)
    }
    .await;
    match native {
        Some(exe) => {
            let label = exe.to_string_lossy().into_owned();
            ConsentIdentity {
                key: Some(format!("native-exe:{label}")),
                label,
            }
        }
        None => ConsentIdentity {
            key: None,
            label: crate::fl!("unknown-application"),
        },
    }
}

/// The implementation interface is a private backend API. Checking only the
/// app ID or session path is insufficient because direct callers can forge
/// both. The XDG portal frontend is the sole allowed caller.
async fn called_by_portal_frontend(connection: &zbus::Connection, header: &Header<'_>) -> bool {
    let Some(sender) = header.sender() else {
        return false;
    };
    let Ok(bus) = zbus::fdo::DBusProxy::new(connection).await else {
        return false;
    };
    let name =
        zbus::names::WellKnownName::from_static_str_unchecked("org.freedesktop.portal.Desktop");
    bus.get_name_owner(zbus::names::BusName::WellKnown(name))
        .await
        .is_ok_and(|owner| owner.as_str() == sender.as_str())
}

async fn always_allowed(connection: &zbus::Connection, app_id: &str) -> bool {
    let Ok(proxy) = PermissionStoreProxy::new(connection).await else {
        tracing::debug!("PermissionStore is unavailable; asking for InputCapture consent");
        return false;
    };
    match proxy
        .get_permission(PERMISSION_TABLE, PERMISSION_ID, app_id)
        .await
    {
        Ok(permissions) => permissions.iter().any(|permission| permission == "yes"),
        Err(err) => {
            tracing::debug!("No persistent InputCapture permission for {app_id}: {err}");
            false
        }
    }
}

async fn remember_always_allowed(connection: &zbus::Connection, app_id: &str) {
    let Ok(proxy) = PermissionStoreProxy::new(connection).await else {
        tracing::warn!("PermissionStore is unavailable; cannot remember InputCapture consent");
        return;
    };
    if let Err(err) = proxy
        .set_permission(
            PERMISSION_TABLE,
            true,
            PERMISSION_ID,
            app_id,
            vec!["yes".to_string()],
        )
        .await
    {
        tracing::warn!("Failed to persist InputCapture consent for {app_id}: {err}");
    }
}

async fn permission_prompt(
    connection: &zbus::Connection,
    tx: &Sender<subscription::Event>,
    handle: &zvariant::ObjectPath<'_>,
    identity: &ConsentIdentity,
    parent_window: &str,
    allow_always: bool,
) -> access::ConfirmationResult {
    if let Some(key) = identity.key.as_deref()
        && always_allowed(connection, key).await
    {
        return access::ConfirmationResult::Allow;
    }

    access::show_confirmation(
        tx,
        handle.to_owned(),
        &identity.label,
        parent_window,
        access::ConfirmationLabels {
            title: crate::fl!("input-capture"),
            subtitle: crate::fl!("input-capture-request"),
            body: crate::fl!(
                "input-capture-description",
                app_name = identity.label.as_str()
            ),
            grant: crate::fl!("allow"),
            deny: crate::fl!("deny"),
            icon: "input-mouse-symbolic".to_string(),
            always_allow: allow_always && identity.key.is_some(),
        },
    )
    .await
}

#[allow(unused_variables)]
#[zbus::interface(name = "org.freedesktop.impl.portal.InputCapture")]
impl InputCapture {
    #[allow(clippy::too_many_arguments)] // Fixed backend D-Bus signature plus caller authorization.
    async fn create_session(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> PortalResponse<HashMap<String, OwnedValue>> {
        if !called_by_portal_frontend(connection, &header).await {
            return PortalResponse::Other;
        }
        let Some(requested) = requested_capabilities(&options) else {
            return PortalResponse::Other;
        };
        let identity = consent_identity(connection, &session_handle, &app_id).await;
        if self
            .install_session(connection, &session_handle)
            .await
            .is_err()
        {
            return PortalResponse::Other;
        }
        let prompt_handle = handle.to_owned();
        Request::run(connection, &handle, || async {}, async {
            let decision = permission_prompt(
                connection,
                &self.tx,
                &prompt_handle,
                &identity,
                &parent_window,
                true,
            )
            .await;
            if decision == access::ConfirmationResult::Deny {
                return PortalResponse::Cancelled;
            }
            let Some(interface) = Self::session_data(connection, &session_handle).await else {
                return PortalResponse::Other;
            };
            if !self.claim_session(&session_handle) {
                tracing::warn!("An InputCapture session already owns the seat");
                return PortalResponse::Other;
            }
            let mut data = interface.get_mut().await;
            data.device_types = requested;
            data.started = true;
            drop(data);
            if decision == access::ConfirmationResult::AlwaysAllow
                && let Some(key) = identity.key.as_deref()
            {
                remember_always_allowed(connection, key).await;
            }
            PortalResponse::Success(capabilities_result(requested))
        })
        .await
    }

    async fn create_session2(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<HashMap<String, OwnedValue>> {
        if !called_by_portal_frontend(connection, &header).await {
            return Err(zbus::fdo::Error::AccessDenied(
                "InputCapture frontend required".into(),
            ));
        }
        self.install_session(connection, &session_handle).await?;
        Ok(HashMap::new())
    }

    #[allow(clippy::too_many_arguments)] // Fixed backend D-Bus signature plus caller authorization.
    async fn start(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: StartOptions,
    ) -> PortalResponse<StartResult> {
        if !called_by_portal_frontend(connection, &header).await {
            return PortalResponse::Other;
        }
        let requested = options.capabilities & SUPPORTED_DEVICE_TYPES;
        if requested == 0 {
            return PortalResponse::Other;
        }
        let persist_mode = options.persist_mode.unwrap_or(0);
        if persist_mode > 2 {
            return PortalResponse::Other;
        }
        let identity = consent_identity(connection, &session_handle, &app_id).await;
        let allow_always = offer_persistent_choice(options.persist_mode, &identity);
        let prompt_handle = handle.to_owned();
        Request::run(connection, &handle, || async {}, async {
            let Some(interface) = Self::session_data(connection, &session_handle).await else {
                return PortalResponse::Other;
            };
            if interface.get().await.started {
                return PortalResponse::Other;
            }
            let restored = if let Some(key) = identity.key.as_deref() {
                options
                    .restore_data
                    .as_ref()
                    .is_some_and(|data| restore_matches(data, key, requested))
                    && always_allowed(connection, key).await
            } else {
                false
            };
            let decision = if restored {
                access::ConfirmationResult::Allow
            } else {
                permission_prompt(
                    connection,
                    &self.tx,
                    &prompt_handle,
                    &identity,
                    &parent_window,
                    allow_always,
                )
                .await
            };
            if decision == access::ConfirmationResult::Deny {
                return PortalResponse::Cancelled;
            }
            if !self.claim_session(&session_handle) {
                tracing::warn!("An InputCapture session already owns the seat");
                return PortalResponse::Other;
            }
            let mut data = interface.get_mut().await;
            data.device_types = requested;
            data.started = true;
            drop(data);
            if decision == access::ConfirmationResult::AlwaysAllow
                && let Some(key) = identity.key.as_deref()
            {
                remember_always_allowed(connection, key).await;
            }
            let persist_key = if persist_mode == 2 {
                identity.key.as_deref()
            } else {
                None
            };
            let restore_data = if let Some(key) = persist_key
                && always_allowed(connection, key).await
            {
                Some(restore_data(key, requested))
            } else {
                None
            };
            PortalResponse::Success(StartResult {
                capabilities: requested,
                clipboard_enabled: false,
                restore_data,
            })
        })
        .await
    }

    async fn get_zones(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> PortalResponse<ZonesResult> {
        if !called_by_portal_frontend(connection, &header).await {
            return PortalResponse::Other;
        }
        Request::run(connection, &handle, || async {}, async {
            let Some(interface) = Self::session_data(connection, &session_handle).await else {
                return PortalResponse::Other;
            };
            let data = interface.get().await;
            // libportal's legacy CreateSession() helper retrieves the zones
            // before it calls ConnectToEIS().  The portal session must only
            // have been started at this point; requiring an EIS connection
            // here makes the legacy API fail with "GetZones() failed".
            if !data.started {
                return PortalResponse::Other;
            }
            let Ok(proxy) = CosmicCompInputCaptureProxy::new(connection).await else {
                return PortalResponse::Other;
            };
            let Ok((zone_set, zones)) = proxy.get_zones(session_handle.as_str()).await else {
                return PortalResponse::Other;
            };
            PortalResponse::Success(ZonesResult { zones, zone_set })
        })
        .await
    }

    #[allow(clippy::too_many_arguments)] // Public D-Bus signature is fixed by the portal specification.
    async fn set_pointer_barriers(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        handle: zvariant::ObjectPath<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
        barriers: Vec<Barrier>,
        zone_set: u32,
    ) -> PortalResponse<SetPointerBarriersResult> {
        if !called_by_portal_frontend(connection, &header).await {
            return PortalResponse::Other;
        }
        if barriers.len() > MAX_BARRIERS {
            return PortalResponse::Other;
        }
        Request::run(connection, &handle, || async {}, async {
            let Some(interface) = Self::session_data(connection, &session_handle).await else {
                return PortalResponse::Other;
            };
            let data = interface.get().await;
            if !data.started || !data.connected {
                return PortalResponse::Other;
            }
            let Ok(proxy) = CosmicCompInputCaptureProxy::new(connection).await else {
                return PortalResponse::Other;
            };
            let barriers = barriers
                .into_iter()
                .map(|barrier| (barrier.barrier_id, barrier.position))
                .collect();
            let Ok(failed_barriers) = proxy
                .set_pointer_barriers(session_handle.as_str(), zone_set, barriers)
                .await
            else {
                return PortalResponse::Other;
            };
            PortalResponse::Success(SetPointerBarriersResult { failed_barriers })
        })
        .await
    }

    async fn enable(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> PortalResponse<HashMap<String, OwnedValue>> {
        if !called_by_portal_frontend(connection, &header).await {
            return PortalResponse::Other;
        }
        let Some(interface) = Self::session_data(connection, &session_handle).await else {
            return PortalResponse::Other;
        };
        let data = interface.get().await;
        if !data.started || !data.connected {
            return PortalResponse::Other;
        }
        let Ok(proxy) = CosmicCompInputCaptureProxy::new(connection).await else {
            return PortalResponse::Other;
        };
        match proxy.enable(session_handle.as_str()).await {
            Ok(()) => PortalResponse::Success(HashMap::new()),
            Err(err) => {
                tracing::debug!("Failed to enable InputCapture: {err}");
                PortalResponse::Other
            }
        }
    }

    async fn disable(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> PortalResponse<HashMap<String, OwnedValue>> {
        if !called_by_portal_frontend(connection, &header).await {
            return PortalResponse::Other;
        }
        let Some(interface) = Self::session_data(connection, &session_handle).await else {
            return PortalResponse::Other;
        };
        if !interface.get().await.started {
            return PortalResponse::Other;
        }
        let Ok(proxy) = CosmicCompInputCaptureProxy::new(connection).await else {
            return PortalResponse::Other;
        };
        match proxy.disable(session_handle.as_str()).await {
            Ok(()) => PortalResponse::Success(HashMap::new()),
            Err(err) => {
                tracing::debug!("Failed to disable InputCapture: {err}");
                PortalResponse::Other
            }
        }
    }

    async fn release(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> PortalResponse<HashMap<String, OwnedValue>> {
        if !called_by_portal_frontend(connection, &header).await {
            return PortalResponse::Other;
        }
        let Some(interface) = Self::session_data(connection, &session_handle).await else {
            return PortalResponse::Other;
        };
        if !interface.get().await.started {
            return PortalResponse::Other;
        }
        let Ok(proxy) = CosmicCompInputCaptureProxy::new(connection).await else {
            return PortalResponse::Other;
        };
        match proxy
            .release(
                session_handle.as_str(),
                zvariant::Optional::from(option_u32(&options, "activation_id")),
                zvariant::Optional::from(option_pair(&options, "cursor_position")),
            )
            .await
        {
            Ok(()) => PortalResponse::Success(HashMap::new()),
            Err(err) => {
                tracing::debug!("Failed to release InputCapture: {err}");
                PortalResponse::Other
            }
        }
    }

    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<zvariant::OwnedFd> {
        if !called_by_portal_frontend(connection, &header).await {
            return Err(zbus::fdo::Error::AccessDenied(
                "InputCapture frontend required".into(),
            ));
        }
        let Some(interface) = Self::session_data(connection, &session_handle).await else {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Unknown InputCapture session".to_string(),
            ));
        };
        let mut data = interface.get_mut().await;
        if !data.started || data.connected {
            return Err(zbus::fdo::Error::InvalidArgs(
                "InputCapture session is not connectable".to_string(),
            ));
        }
        let proxy = CosmicCompInputCaptureProxy::new(connection)
            .await
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
        let fd = proxy
            .get_receiver_socket(session_handle.as_str(), data.device_types)
            .await
            .map_err(|err| zbus::fdo::Error::Failed(err.to_string()))?;
        data.connected = true;
        Ok(fd)
    }

    #[zbus(property, name = "SupportedCapabilities")]
    async fn supported_capabilities(&self) -> u32 {
        SUPPORTED_DEVICE_TYPES
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        2
    }

    #[zbus(signal)]
    async fn activated(
        emitter: &SignalEmitter<'_>,
        session_handle: &zvariant::ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        emitter: &SignalEmitter<'_>,
        session_handle: &zvariant::ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn disabled(
        emitter: &SignalEmitter<'_>,
        session_handle: &zvariant::ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn zones_changed(
        emitter: &SignalEmitter<'_>,
        session_handle: &zvariant::ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
}

/// Forward a compositor signal to the public portal interface.
pub(crate) async fn forward_signal(
    connection: &zbus::Connection,
    signal: CompSignal,
) -> zbus::Result<()> {
    let iface = connection
        .object_server()
        .interface::<_, InputCapture>(DBUS_PATH)
        .await?;
    let emitter = iface.signal_emitter();
    match signal {
        CompSignal::Activated {
            session_handle,
            activation_id,
            barrier_id,
            cursor_position,
        } => {
            let session_handle = zvariant::ObjectPath::try_from(session_handle.as_str())
                .map_err(|err| zbus::Error::Failure(err.to_string()))?;
            let options = HashMap::from([
                ("activation_id".to_string(), OwnedValue::from(activation_id)),
                ("barrier_id".to_string(), OwnedValue::from(barrier_id)),
                (
                    "cursor_position".to_string(),
                    zvariant::Value::from((cursor_position.0, cursor_position.1))
                        .try_to_owned()
                        .map_err(|err| zbus::Error::Failure(err.to_string()))?,
                ),
            ]);
            InputCapture::activated(emitter, &session_handle, options).await
        }
        CompSignal::Deactivated {
            session_handle,
            activation_id,
            cursor_position,
        } => {
            let session_handle = zvariant::ObjectPath::try_from(session_handle.as_str())
                .map_err(|err| zbus::Error::Failure(err.to_string()))?;
            let options = HashMap::from([
                ("activation_id".to_string(), OwnedValue::from(activation_id)),
                (
                    "cursor_position".to_string(),
                    zvariant::Value::from((cursor_position.0, cursor_position.1))
                        .try_to_owned()
                        .map_err(|err| zbus::Error::Failure(err.to_string()))?,
                ),
            ]);
            InputCapture::deactivated(emitter, &session_handle, options).await
        }
        CompSignal::Disabled { session_handle } => {
            let session_handle = zvariant::ObjectPath::try_from(session_handle.as_str())
                .map_err(|err| zbus::Error::Failure(err.to_string()))?;
            InputCapture::disabled(emitter, &session_handle, HashMap::new()).await
        }
        CompSignal::ZonesChanged {
            session_handle,
            zone_set,
        } => {
            let session_handle = zvariant::ObjectPath::try_from(session_handle.as_str())
                .map_err(|err| zbus::Error::Failure(err.to_string()))?;
            let options = HashMap::from([("zone_set".to_string(), OwnedValue::from(zone_set))]);
            InputCapture::zones_changed(emitter, &session_handle, options).await
        }
    }
}

/// Listen to the compositor's private signal stream. The public portal
/// dispatcher consumes the resulting events and emits the standard signals.
pub(crate) async fn watch_compositor(
    connection: zbus::Connection,
    tx: Sender<subscription::Event>,
) -> zbus::Result<()> {
    let proxy = CosmicCompInputCaptureProxy::new(&connection).await?;
    let mut activated = proxy.receive_activated().await?;
    let mut deactivated = proxy.receive_deactivated().await?;
    let mut disabled = proxy.receive_disabled().await?;
    let mut zones_changed = proxy.receive_zones_changed().await?;
    loop {
        tokio::select! {
            signal = activated.next() => {
                let Some(signal) = signal else { return Ok(()); };
                let args = signal.args()?;
                tx.send(subscription::Event::InputCapture(CompSignal::Activated {
                    session_handle: args.session_handle.to_string(),
                    activation_id: args.activation_id,
                    barrier_id: args.barrier_id,
                    cursor_position: args.cursor_position,
                })).await.map_err(|err| zbus::Error::Failure(err.to_string()))?;
            }
            signal = deactivated.next() => {
                let Some(signal) = signal else { return Ok(()); };
                let args = signal.args()?;
                tx.send(subscription::Event::InputCapture(CompSignal::Deactivated {
                    session_handle: args.session_handle.to_string(),
                    activation_id: args.activation_id,
                    cursor_position: args.cursor_position,
                })).await.map_err(|err| zbus::Error::Failure(err.to_string()))?;
            }
            signal = disabled.next() => {
                let Some(signal) = signal else { return Ok(()); };
                let args = signal.args()?;
                tx.send(subscription::Event::InputCapture(CompSignal::Disabled {
                    session_handle: args.session_handle.to_string(),
                })).await.map_err(|err| zbus::Error::Failure(err.to_string()))?;
            }
            signal = zones_changed.next() => {
                let Some(signal) = signal else { return Ok(()); };
                let args = signal.args()?;
                tx.send(subscription::Event::InputCapture(CompSignal::ZonesChanged {
                    session_handle: args.session_handle.to_string(),
                    zone_set: args.zone_set,
                })).await.map_err(|err| zbus::Error::Failure(err.to_string()))?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_data_must_match_application_and_capabilities() {
        let data = restore_data("org.example.Synergy", DEVICE_KEYBOARD | DEVICE_POINTER);
        assert!(restore_matches(
            &data,
            "org.example.Synergy",
            DEVICE_KEYBOARD | DEVICE_POINTER
        ));
        assert!(!restore_matches(
            &data,
            "org.example.Other",
            DEVICE_KEYBOARD | DEVICE_POINTER
        ));
        assert!(!restore_matches(
            &data,
            "org.example.Synergy",
            DEVICE_POINTER
        ));
    }

    #[test]
    fn unsupported_or_empty_capabilities_are_rejected() {
        assert_eq!(requested_capabilities(&HashMap::new()), None);
        let options = HashMap::from([("capabilities".to_string(), OwnedValue::from(4_u32))]);
        assert_eq!(requested_capabilities(&options), None);
    }

    #[test]
    fn one_seat_owner_and_close_only_releases_its_own_session() {
        let (tx, _) = tokio::sync::mpsc::channel(1);
        let portal = InputCapture::new(tx);
        let first = zvariant::ObjectPath::try_from("/session/first").unwrap();
        let second = zvariant::ObjectPath::try_from("/session/second").unwrap();
        assert!(portal.claim_session(&first));
        assert!(!portal.claim_session(&second));
        release_owned_session(&portal.session_owner, second.as_str());
        assert!(!portal.claim_session(&second));
        release_owned_session(&portal.session_owner, first.as_str());
        assert!(portal.claim_session(&second));
    }

    #[test]
    fn native_consent_uses_frontend_session_sender_not_empty_app_id() {
        assert_eq!(
            native_sender_from_session_path(
                "/org/freedesktop/portal/desktop/session/1_30615/portal372470189"
            ),
            Some(":1.30615".to_string())
        );
        assert_eq!(
            native_sender_from_session_path("/other/session/1_30615/token"),
            None
        );
        assert_eq!(
            native_sender_from_session_path("/org/freedesktop/portal/desktop/session//token"),
            None
        );
    }

    #[test]
    fn persistent_native_consent_rejects_writable_executable_locations() {
        assert!(system_owned_executable(Path::new("/usr/bin/true")).is_some());
        assert!(system_owned_executable(&std::env::temp_dir()).is_none());
    }

    #[test]
    fn legacy_native_client_can_explicitly_choose_persistent_consent() {
        let identity = ConsentIdentity {
            key: Some("native-exe:/opt/Synergy/synergy-core".into()),
            label: "Synergy".into(),
        };
        assert!(offer_persistent_choice(None, &identity));
        assert!(!offer_persistent_choice(Some(0), &identity));
        assert!(offer_persistent_choice(Some(2), &identity));
    }
}
