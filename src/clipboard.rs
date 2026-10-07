//! `org.freedesktop.impl.portal.Clipboard`
//!
//! The portal is an `ext-data-control-v1` client, the same role KDE's backend
//! plays. `RequestClipboard` grants a session, `SelectionRead` pipes
//! `offer.receive`, and `SetSelection` creates a data source whose `send`
//! events become `SelectionTransfer`. One object serves InputCapture and
//! RemoteDesktop.

use std::collections::HashMap;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{
    Connection, Dispatch, DispatchError, EventQueue, Proxy, QueueHandle, event_created_child,
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{self, OwnedValue};

use crate::input_capture::{InputCaptureData, called_by_portal_frontend};
use crate::screencast::SessionData;
use crate::{DBUS_PATH, session_interface};

const TRANSFER_TIMEOUT: Duration = Duration::from_secs(2);

struct Snapshot {
    mimes: Vec<String>,
    owner: Option<String>,
}

enum Cmd {
    Grant {
        path: String,
    },
    DropSession {
        path: String,
    },
    SetSelection {
        session: String,
        mimes: Vec<String>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Read {
        mime: String,
        reply: oneshot::Sender<Result<OwnedFd, String>>,
    },
    Write {
        serial: u32,
        reply: oneshot::Sender<Result<OwnedFd, String>>,
    },
    WriteDone {
        serial: u32,
    },
    Snapshot {
        reply: oneshot::Sender<Snapshot>,
    },
}

enum ClipEvent {
    Owner {
        sessions: Vec<String>,
        mimes: Vec<String>,
        owner: Option<String>,
    },
    Transfer {
        session: String,
        mime: String,
        serial: u32,
    },
}

struct Backend {
    cmd_tx: Sender<Cmd>,
    wake: OwnedFd,
    available: Arc<AtomicBool>,
    events: Mutex<Option<UnboundedReceiver<ClipEvent>>>,
}

impl Backend {
    fn send(&self, cmd: Cmd) -> bool {
        if self.cmd_tx.send(cmd).is_err() {
            return false;
        }
        let _ = rustix::io::write(&self.wake, &[1]);
        true
    }
}

static BACKEND: OnceLock<Arc<Backend>> = OnceLock::new();

pub(crate) fn is_available() -> bool {
    BACKEND
        .get()
        .is_some_and(|backend| backend.available.load(Ordering::Acquire))
}

pub(crate) fn start() {
    BACKEND.get_or_init(|| {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (wake_read, wake_write) =
            rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK)
                .expect("clipboard wake pipe");
        let available = Arc::new(AtomicBool::new(false));
        let backend = Arc::new(Backend {
            cmd_tx,
            wake: wake_write,
            available: available.clone(),
            events: Mutex::new(Some(event_rx)),
        });
        thread::Builder::new()
            .name("clipboard-wl".into())
            .spawn(move || thread_main(cmd_rx, wake_read, event_tx, available))
            .expect("clipboard thread");
        backend
    });
}

pub(crate) fn spawn_forwarder(connection: zbus::Connection) {
    let Some(backend) = BACKEND.get() else {
        return;
    };
    let Some(mut events) = backend.events.lock().unwrap().take() else {
        return;
    };
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let Err(err) = forward_event(&connection, event).await {
                tracing::debug!("clipboard signal: {err}");
            }
        }
    });
}

pub(crate) fn session_closed(path: &str) {
    let Some(backend) = BACKEND.get() else {
        return;
    };
    backend.send(Cmd::DropSession {
        path: path.to_string(),
    });
}

pub(crate) fn schedule_announce(connection: zbus::Connection, session: String) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        announce(&connection, &session).await;
    });
}

pub(crate) async fn announce(connection: &zbus::Connection, session: &str) {
    let Ok(snapshot) = snapshot().await else {
        return;
    };
    let is_owner = snapshot.owner.as_deref() == Some(session);
    if let Err(err) = emit_owner(connection, session, &snapshot.mimes, is_owner).await {
        tracing::debug!("clipboard announce {session}: {err}");
    }
}

async fn snapshot() -> Result<Snapshot, ()> {
    let Some(backend) = BACKEND.get() else {
        return Err(());
    };
    let (tx, rx) = oneshot::channel();
    if !backend.send(Cmd::Snapshot { reply: tx }) {
        return Err(());
    }
    tokio::time::timeout(TRANSFER_TIMEOUT, rx)
        .await
        .ok()
        .and_then(Result::ok)
        .ok_or(())
}

async fn roundtrip_unit(
    build: impl FnOnce(oneshot::Sender<Result<(), String>>) -> Cmd,
) -> zbus::fdo::Result<()> {
    let backend = BACKEND.get().ok_or_else(backend_stopped)?;
    let (tx, rx) = oneshot::channel();
    if !backend.send(build(tx)) {
        return Err(backend_stopped());
    }
    match tokio::time::timeout(TRANSFER_TIMEOUT, rx).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(message))) => Err(zbus::fdo::Error::Failed(message)),
        _ => Err(backend_stopped()),
    }
}

async fn roundtrip_fd(
    build: impl FnOnce(oneshot::Sender<Result<OwnedFd, String>>) -> Cmd,
) -> zbus::fdo::Result<zvariant::OwnedFd> {
    let backend = BACKEND.get().ok_or_else(backend_stopped)?;
    let (tx, rx) = oneshot::channel();
    if !backend.send(build(tx)) {
        return Err(backend_stopped());
    }
    match tokio::time::timeout(TRANSFER_TIMEOUT, rx).await {
        Ok(Ok(Ok(fd))) => Ok(zvariant::OwnedFd::from(fd)),
        Ok(Ok(Err(message))) => Err(zbus::fdo::Error::Failed(message)),
        _ => Err(backend_stopped()),
    }
}

fn backend_stopped() -> zbus::fdo::Error {
    zbus::fdo::Error::Failed("clipboard backend unavailable".into())
}

pub(crate) struct Clipboard;

impl Clipboard {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Clipboard")]
impl Clipboard {
    #[zbus(name = "RequestClipboard")]
    async fn request_clipboard(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        if !called_by_portal_frontend(connection, &header).await {
            return Err(zbus::fdo::Error::AccessDenied(
                "Clipboard frontend required".into(),
            ));
        }
        if !is_available() {
            return Err(zbus::fdo::Error::Failed(
                "ext-data-control is not available".into(),
            ));
        }
        let path = session_handle.as_str().to_string();
        if let Some(interface) =
            session_interface::<InputCaptureData>(connection, &session_handle).await
        {
            interface.get_mut().await.clipboard_requested = true;
        } else if let Some(interface) =
            session_interface::<SessionData>(connection, &session_handle).await
        {
            let mut data = interface.get_mut().await;
            let Some(remote) = data.remote_desktop.as_mut() else {
                return Err(zbus::fdo::Error::InvalidArgs(
                    "Session is not InputCapture or RemoteDesktop".into(),
                ));
            };
            remote.clipboard_enabled = true;
            remote.clipboard_session = Some(path.clone());
        } else {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Unknown clipboard session".into(),
            ));
        }
        let Some(backend) = BACKEND.get() else {
            return Err(backend_stopped());
        };
        if !backend.send(Cmd::Grant { path }) {
            return Err(backend_stopped());
        }
        Ok(())
    }

    #[zbus(name = "SetSelection")]
    async fn set_selection(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        require_frontend(connection, &header).await?;
        require_enabled(connection, &session_handle).await?;
        let session = session_handle.as_str().to_string();
        let mimes = mime_types(&options);
        roundtrip_unit(move |reply| Cmd::SetSelection {
            session,
            mimes,
            reply,
        })
        .await
    }

    #[zbus(name = "SelectionWrite")]
    async fn selection_write(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        serial: u32,
    ) -> zbus::fdo::Result<zvariant::OwnedFd> {
        require_frontend(connection, &header).await?;
        require_enabled(connection, &session_handle).await?;
        roundtrip_fd(move |reply| Cmd::Write { serial, reply }).await
    }

    #[zbus(name = "SelectionWriteDone")]
    async fn selection_write_done(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        serial: u32,
        _success: bool,
    ) -> zbus::fdo::Result<()> {
        require_frontend(connection, &header).await?;
        require_enabled(connection, &session_handle).await?;
        let Some(backend) = BACKEND.get() else {
            return Err(backend_stopped());
        };
        if !backend.send(Cmd::WriteDone { serial }) {
            return Err(backend_stopped());
        }
        Ok(())
    }

    #[zbus(name = "SelectionRead")]
    async fn selection_read(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: Header<'_>,
        session_handle: zvariant::ObjectPath<'_>,
        mime_type: String,
    ) -> zbus::fdo::Result<zvariant::OwnedFd> {
        require_frontend(connection, &header).await?;
        require_enabled(connection, &session_handle).await?;
        roundtrip_fd(move |reply| Cmd::Read {
            mime: mime_type,
            reply,
        })
        .await
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    #[zbus(signal)]
    async fn selection_owner_changed(
        emitter: &SignalEmitter<'_>,
        session_handle: &zvariant::ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn selection_transfer(
        emitter: &SignalEmitter<'_>,
        session_handle: &zvariant::ObjectPath<'_>,
        mime_type: &str,
        serial: u32,
    ) -> zbus::Result<()>;
}

async fn require_frontend(
    connection: &zbus::Connection,
    header: &Header<'_>,
) -> zbus::fdo::Result<()> {
    if called_by_portal_frontend(connection, header).await {
        Ok(())
    } else {
        Err(zbus::fdo::Error::AccessDenied(
            "Clipboard frontend required".into(),
        ))
    }
}

async fn require_enabled(
    connection: &zbus::Connection,
    session_handle: &zvariant::ObjectPath<'_>,
) -> zbus::fdo::Result<()> {
    if let Some(interface) = session_interface::<InputCaptureData>(connection, session_handle).await
    {
        let data = interface.get().await;
        if data.clipboard_requested && data.started {
            return Ok(());
        }
        return Err(zbus::fdo::Error::AccessDenied(
            "Clipboard not enabled".into(),
        ));
    }
    if let Some(interface) = session_interface::<SessionData>(connection, session_handle).await {
        let enabled = interface
            .get()
            .await
            .remote_desktop
            .as_ref()
            .is_some_and(|remote| remote.clipboard_enabled);
        if enabled {
            return Ok(());
        }
        return Err(zbus::fdo::Error::AccessDenied(
            "Clipboard not enabled".into(),
        ));
    }
    Err(zbus::fdo::Error::InvalidArgs(
        "Unknown clipboard session".into(),
    ))
}

fn mime_types(options: &HashMap<String, OwnedValue>) -> Vec<String> {
    let Some(value) = options.get("mime_types") else {
        return Vec::new();
    };
    match value.downcast_ref::<&zvariant::Array>() {
        Ok(array) => array
            .iter()
            .filter_map(|item| item.downcast_ref::<&str>().ok().map(str::to_owned))
            .collect(),
        Err(err) => {
            tracing::warn!("clipboard mime_types has an unexpected type: {err}");
            Vec::new()
        }
    }
}

async fn forward_event(connection: &zbus::Connection, event: ClipEvent) -> zbus::Result<()> {
    match event {
        ClipEvent::Owner {
            sessions,
            mimes,
            owner,
        } => {
            for session in sessions {
                let is_owner = owner.as_deref() == Some(session.as_str());
                emit_owner(connection, &session, &mimes, is_owner).await?;
            }
            Ok(())
        }
        ClipEvent::Transfer {
            session,
            mime,
            serial,
        } => emit_transfer(connection, &session, &mime, serial).await,
    }
}

async fn emit_owner(
    connection: &zbus::Connection,
    session: &str,
    mimes: &[String],
    session_is_owner: bool,
) -> zbus::Result<()> {
    let iface = connection
        .object_server()
        .interface::<_, Clipboard>(DBUS_PATH)
        .await?;
    let path = object_path(session)?;
    let mime_types = zvariant::Value::Array(zvariant::Array::from(mimes.to_vec()))
        .try_to_owned()
        .map_err(|err| zbus::Error::Failure(err.to_string()))?;
    let options = HashMap::from([
        ("mime_types".to_string(), mime_types),
        (
            "session_is_owner".to_string(),
            OwnedValue::from(session_is_owner),
        ),
    ]);
    Clipboard::selection_owner_changed(iface.signal_emitter(), &path, options).await
}

async fn emit_transfer(
    connection: &zbus::Connection,
    session: &str,
    mime: &str,
    serial: u32,
) -> zbus::Result<()> {
    let iface = connection
        .object_server()
        .interface::<_, Clipboard>(DBUS_PATH)
        .await?;
    let path = object_path(session)?;
    Clipboard::selection_transfer(iface.signal_emitter(), &path, mime, serial).await
}

fn object_path(session: &str) -> zbus::Result<zvariant::OwnedObjectPath> {
    zvariant::ObjectPath::try_from(session)
        .map(zvariant::OwnedObjectPath::from)
        .map_err(|err| zbus::Error::Failure(err.to_string()))
}

fn thread_main(
    cmd_rx: Receiver<Cmd>,
    wake_read: OwnedFd,
    event_tx: tokio::sync::mpsc::UnboundedSender<ClipEvent>,
    available: Arc<AtomicBool>,
) {
    if let Err(err) = run(&cmd_rx, &wake_read, event_tx, &available) {
        tracing::warn!("clipboard data-control unavailable: {err:#}");
        available.store(false, Ordering::Release);
    }
    while let Ok(cmd) = cmd_rx.recv() {
        fail_cmd(cmd);
    }
}

fn fail_cmd(cmd: Cmd) {
    match cmd {
        Cmd::SetSelection { reply, .. } => {
            let _ = reply.send(Err("ext-data-control is not available".into()));
        }
        Cmd::Read { reply, .. } | Cmd::Write { reply, .. } => {
            let _ = reply.send(Err("ext-data-control is not available".into()));
        }
        Cmd::Snapshot { reply } => {
            let _ = reply.send(Snapshot {
                mimes: Vec::new(),
                owner: None,
            });
        }
        Cmd::Grant { .. } | Cmd::DropSession { .. } | Cmd::WriteDone { .. } => {}
    }
}

struct Offer {
    offer: ExtDataControlOfferV1,
    mimes: Vec<String>,
}

struct ClipState {
    // Keeps the seat binding alive for the data device.
    _seat: Option<WlSeat>,
    manager: Option<ExtDataControlManagerV1>,
    device: Option<ExtDataControlDeviceV1>,
    pending: Vec<Offer>,
    current: Option<Offer>,
    primary: Option<ExtDataControlOfferV1>,
    source: Option<ExtDataControlSourceV1>,
    owner: Option<String>,
    sessions: Vec<String>,
    writes: HashMap<u32, OwnedFd>,
    next_serial: u32,
    events: tokio::sync::mpsc::UnboundedSender<ClipEvent>,
    available: Arc<AtomicBool>,
}

impl ClipState {
    fn publish(&self) {
        if self.sessions.is_empty() {
            return;
        }
        let mimes = self
            .current
            .as_ref()
            .map(|offer| offer.mimes.clone())
            .unwrap_or_default();
        let _ = self.events.send(ClipEvent::Owner {
            sessions: self.sessions.clone(),
            mimes,
            owner: self.owner.clone(),
        });
    }
}

fn take_pending(state: &mut ClipState, offer: ExtDataControlOfferV1) -> Offer {
    if let Some(index) = state
        .pending
        .iter()
        .position(|item| item.offer.id() == offer.id())
    {
        state.pending.remove(index)
    } else {
        Offer {
            offer,
            mimes: Vec::new(),
        }
    }
}

fn run(
    cmd_rx: &Receiver<Cmd>,
    wake_read: &OwnedFd,
    event_tx: tokio::sync::mpsc::UnboundedSender<ClipEvent>,
    available: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let conn = Connection::connect_to_env().map_err(|err| anyhow::anyhow!("wayland: {err}"))?;
    let (globals, mut queue) =
        registry_queue_init(&conn).map_err(|err| anyhow::anyhow!("wayland globals: {err}"))?;
    let qh = queue.handle();
    let seat: WlSeat = globals
        .bind(&qh, 1..=8, ())
        .map_err(|err| anyhow::anyhow!("wl_seat: {err}"))?;
    let manager: ExtDataControlManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|err| anyhow::anyhow!("ext_data_control_manager_v1: {err}"))?;
    let device = manager.get_data_device(&seat, &qh, ());
    let mut state = ClipState {
        _seat: Some(seat),
        manager: Some(manager),
        device: Some(device),
        pending: Vec::new(),
        current: None,
        primary: None,
        source: None,
        owner: None,
        sessions: Vec::new(),
        writes: HashMap::new(),
        next_serial: 1,
        events: event_tx,
        available: available.clone(),
    };
    queue
        .roundtrip(&mut state)
        .map_err(|err| anyhow::anyhow!("clipboard roundtrip: {err}"))?;
    available.store(true, Ordering::Release);
    let mime_count = state.current.as_ref().map(|offer| offer.mimes.len()).unwrap_or(0);
    tracing::warn!("Clipboard ext-data-control ready ({mime_count} mime types)");

    loop {
        dispatch_once(&mut queue, &mut state)?;
        while let Ok(cmd) = cmd_rx.try_recv() {
            handle(&mut state, &qh, &queue, cmd);
        }
        queue.flush()?;
        let Some(guard) = conn.prepare_read() else {
            continue;
        };
        if let Ok(cmd) = cmd_rx.try_recv() {
            drop(guard);
            handle(&mut state, &qh, &queue, cmd);
            while let Ok(cmd) = cmd_rx.try_recv() {
                handle(&mut state, &qh, &queue, cmd);
            }
            continue;
        }
        let polled = {
            let wl = guard.connection_fd();
            let mut fds = [
                rustix::event::PollFd::new(&wl, rustix::event::PollFlags::IN),
                rustix::event::PollFd::new(wake_read, rustix::event::PollFlags::IN),
            ];
            match rustix::event::poll(&mut fds, None) {
                Ok(_) => Ok((fds[0].revents(), fds[1].revents())),
                Err(err) => Err(err),
            }
        };
        let (wl_events, wake_events) = match polled {
            Err(rustix::io::Errno::INTR) => {
                drop(guard);
                continue;
            }
            Err(err) => return Err(err.into()),
            Ok(events) => events,
        };
        let wayland_ready = wl_events.intersects(
            rustix::event::PollFlags::IN
                | rustix::event::PollFlags::ERR
                | rustix::event::PollFlags::HUP,
        );
        if wayland_ready {
            if let Err(err) = guard.read() {
                return Err(err.into());
            }
        } else {
            drop(guard);
        }
        if wake_events.intersects(rustix::event::PollFlags::IN) {
            drain(wake_read.as_fd());
        }
    }
}

fn dispatch_once(queue: &mut EventQueue<ClipState>, state: &mut ClipState) -> anyhow::Result<()> {
    match queue.dispatch_pending(state) {
        Ok(_) => Ok(()),
        Err(err @ DispatchError::BadMessage { .. }) => {
            tracing::warn!("clipboard dispatch: {err}");
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

fn drain(fd: BorrowedFd<'_>) {
    let mut buf = [0u8; 32];
    loop {
        match rustix::io::read(fd, &mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => break,
        }
    }
}

fn handle(state: &mut ClipState, qh: &QueueHandle<ClipState>, queue: &EventQueue<ClipState>, cmd: Cmd) {
    match cmd {
        Cmd::Grant { path } => {
            if !state.sessions.iter().any(|session| session == &path) {
                state.sessions.push(path);
            }
        }
        Cmd::DropSession { path } => {
            state.sessions.retain(|session| session != &path);
            if state.owner.as_deref() == Some(path.as_str()) {
                clear_selection(state, queue);
            }
        }
        Cmd::SetSelection {
            session,
            mimes,
            reply,
        } => {
            let _ = reply.send(set_selection(state, qh, queue, session, mimes));
        }
        Cmd::Read { mime, reply } => {
            let _ = reply.send(selection_read(state, queue, mime));
        }
        Cmd::Write { serial, reply } => {
            let result = match state.writes.get(&serial) {
                Some(fd) => rustix::io::dup(fd.as_fd()).map_err(|err| err.to_string()),
                None => Err(format!("unknown selection transfer {serial}")),
            };
            let _ = reply.send(result);
        }
        Cmd::WriteDone { serial } => {
            state.writes.remove(&serial);
        }
        Cmd::Snapshot { reply } => {
            let mimes = state
                .current
                .as_ref()
                .map(|offer| offer.mimes.clone())
                .unwrap_or_default();
            let _ = reply.send(Snapshot {
                mimes,
                owner: state.owner.clone(),
            });
        }
    }
}

fn clear_selection(state: &mut ClipState, queue: &EventQueue<ClipState>) {
    if let Some(device) = state.device.clone() {
        device.set_selection(None);
        let _ = queue.flush();
    }
    state.source = None;
    state.owner = None;
    state.publish();
}

fn set_selection(
    state: &mut ClipState,
    qh: &QueueHandle<ClipState>,
    queue: &EventQueue<ClipState>,
    session: String,
    mimes: Vec<String>,
) -> Result<(), String> {
    let Some(manager) = state.manager.clone() else {
        return Err("ext-data-control is not available".into());
    };
    let Some(device) = state.device.clone() else {
        return Err("ext-data-control is not available".into());
    };
    if mimes.is_empty() {
        device.set_selection(None);
        state.source = None;
        state.owner = None;
        queue.flush().map_err(|err| err.to_string())?;
        state.publish();
        return Ok(());
    }
    let source = manager.create_data_source(qh, ());
    for mime in &mimes {
        source.offer(mime.clone());
    }
    device.set_selection(Some(&source));
    let _old = state.source.take();
    state.source = Some(source);
    state.owner = Some(session);
    queue.flush().map_err(|err| err.to_string())?;
    Ok(())
}

fn selection_read(
    state: &mut ClipState,
    queue: &EventQueue<ClipState>,
    mime: String,
) -> Result<OwnedFd, String> {
    let Some(current) = state.current.as_ref() else {
        return Err("no current clipboard selection".into());
    };
    if !current.mimes.iter().any(|offered| offered == &mime) {
        return Err(format!("mime type {mime} is not offered"));
    }
    let offer = current.offer.clone();
    let (read_fd, write_fd) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
        .map_err(|err| err.to_string())?;
    offer.receive(mime, write_fd.as_fd());
    queue.flush().map_err(|err| err.to_string())?;
    drop(write_fd);
    Ok(read_fd)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ClipState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for ClipState {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlManagerV1, ()> for ClipState {
    fn event(
        _: &mut Self,
        _: &ExtDataControlManagerV1,
        _: wayland_protocols::ext::data_control::v1::client::ext_data_control_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for ClipState {
    fn event(
        state: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_device_v1::Event::DataOffer { id } => {
                state.pending.push(Offer {
                    offer: id,
                    mimes: Vec::new(),
                });
            }
            ext_data_control_device_v1::Event::Selection { id } => {
                if let Some(old) = state.current.take() {
                    old.offer.destroy();
                }
                state.current = id.map(|offer| take_pending(state, offer));
                state.publish();
            }
            ext_data_control_device_v1::Event::PrimarySelection { id } => {
                if let Some(old) = state.primary.take() {
                    old.destroy();
                }
                if let Some(offer) = id {
                    let taken = take_pending(state, offer);
                    state.primary = Some(taken.offer);
                }
            }
            ext_data_control_device_v1::Event::Finished => {
                state.available.store(false, Ordering::Release);
            }
            _ => {}
        }
    }

    event_created_child!(ClipState, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for ClipState {
    fn event(
        state: &mut Self,
        proxy: &ExtDataControlOfferV1,
        event: ext_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let ext_data_control_offer_v1::Event::Offer { mime_type } = event else {
            return;
        };
        if let Some(offer) = state
            .pending
            .iter_mut()
            .find(|item| item.offer.id() == proxy.id())
        {
            if !offer.mimes.iter().any(|mime| mime == &mime_type) {
                offer.mimes.push(mime_type);
            }
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for ClipState {
    fn event(
        state: &mut Self,
        proxy: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_source_v1::Event::Send { mime_type, fd } => {
                let Some(session) = state.owner.clone() else {
                    return;
                };
                let serial = state.next_serial;
                state.next_serial = state.next_serial.wrapping_add(1);
                state.writes.insert(serial, fd);
                let _ = state.events.send(ClipEvent::Transfer {
                    session,
                    mime: mime_type,
                    serial,
                });
            }
            ext_data_control_source_v1::Event::Cancelled => {
                if state.source.as_ref().is_some_and(|source| source.id() == proxy.id()) {
                    state.source = None;
                    state.owner = None;
                    state.publish();
                }
            }
            _ => {}
        }
    }
}
