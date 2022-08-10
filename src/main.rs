use futures::future::poll_fn;
use std::{future, process};
use tokio::io::{unix::AsyncFd, Interest};
use wayland_client::{
    protocol::{wl_output, wl_registry},
    Connection, Dispatch, QueueHandle,
};

mod documents;
mod screenshot;
use screenshot::Screenshot;
mod screencast;
use screencast::ScreenCast;
mod screencast_thread;

static DBUS_NAME: &str = "org.freedesktop.impl.portal.desktop.cosmic";
static DBUS_PATH: &str = "/org/freedesktop/portal/desktop";

const PORTAL_RESPONSE_SUCCESS: u32 = 0;
const PORTAL_RESPONSE_CANCELLED: u32 = 1;
const PORTAL_RESPONSE_OTHER: u32 = 2;

// org.freedesktop.impl.portal.Request/org.freedesktop.impl.portal.Session
// - implemented by objects at different paths
// org.freedesktop.impl.portal.Inhibit
// org.freedesktop.impl.portal.Screenshot

struct Request;

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    fn close(&self) {}
}

struct Session {
    close_cb: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
}

impl Session {
    fn new<F: FnOnce() + Send + Sync + 'static>(cb: F) -> Self {
        Self {
            close_cb: Some(Box::new(cb)),
        }
    }
}

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.Session")]
impl Session {
    async fn close(&mut self, #[zbus(signal_context)] signal_ctxt: zbus::SignalContext<'_>) {
        // XXX error?
        let _ = self.closed(&signal_ctxt).await;
        let _ = signal_ctxt
            .connection()
            .object_server()
            .remove::<Self, _>(signal_ctxt.path())
            .await;
        if let Some(cb) = self.close_cb.take() {
            cb();
        }
    }

    #[dbus_interface(signal)]
    async fn closed(&self, signal_ctxt: &zbus::SignalContext<'_>) -> zbus::Result<()>;

    #[dbus_interface(property, name = "version")]
    fn version(&self) -> u32 {
        1 // XXX?
    }
}

struct Globals(std::collections::BTreeMap<u32, (String, u32)>);

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        app_data: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Globals>,
    ) {
        println!("{:?}", event);
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                app_data.0.insert(name, (interface, version));
            }
            wl_registry::Event::GlobalRemove { name } => {
                app_data.0.remove(&name);
            }
            _ => {}
        }
    }
}

async fn read_event_task(connection: wayland_client::Connection) {
    // XXX unwrap
    let fd = connection.prepare_read().unwrap().connection_fd();
    let async_fd = AsyncFd::with_interest(fd, Interest::READABLE).unwrap();
    loop {
        let read_event_guard = connection.prepare_read().unwrap();
        let mut read_guard = async_fd.readable().await.unwrap();
        read_event_guard.read().unwrap();
        read_guard.clear_ready();
    }
}

// Connect to wayland and start task reading events from socket
fn connect_to_wayland() -> wayland_client::Connection {
    let wayland_connection = match wayland_client::Connection::connect_to_env() {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("Error: {}", err);
            process::exit(1)
        }
    };

    tokio::spawn(read_event_task(wayland_connection.clone()));

    wayland_connection
}

fn monitor_wayland_registry(connection: wayland_client::Connection) {
    // XXX unwrap
    let display = connection.display();
    let mut event_queue = connection.new_event_queue::<Globals>();
    let _registry = display.get_registry(&event_queue.handle(), ()).unwrap();
    let mut data = Globals(Default::default());
    event_queue.flush().unwrap();

    tokio::spawn(async move {
        poll_fn(|cx| event_queue.poll_dispatch_pending(cx, &mut data)).await;
    });
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let wayland_connection = connect_to_wayland();

    monitor_wayland_registry(wayland_connection.clone());

    let _connection = zbus::ConnectionBuilder::session()?
        .name(DBUS_NAME)?
        .serve_at(DBUS_PATH, Screenshot::new(wayland_connection.clone()))?
        .serve_at(DBUS_PATH, ScreenCast::new(wayland_connection))?
        .build()
        .await?;

    future::pending::<()>().await;

    Ok(())
}
