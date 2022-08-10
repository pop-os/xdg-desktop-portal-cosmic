use std::future;

mod dmabuf_frame;
mod documents;
mod screenshot;
use screenshot::Screenshot;
mod screencast;
use screencast::ScreenCast;
mod screencast_thread;
mod wayland;

static DBUS_NAME: &str = "org.freedesktop.impl.portal.desktop.cosmic";
static DBUS_PATH: &str = "/org/freedesktop/portal/desktop";

const PORTAL_RESPONSE_SUCCESS: u32 = 0;
const PORTAL_RESPONSE_CANCELLED: u32 = 1;
const PORTAL_RESPONSE_OTHER: u32 = 2;

// TODO: org.freedesktop.impl.portal.Inhibit?

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

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let wayland_connection = wayland::connect_to_wayland();
    let wayland_helper = wayland::WaylandHelper::new(wayland_connection);

    let _connection = zbus::ConnectionBuilder::session()?
        .name(DBUS_NAME)?
        .serve_at(DBUS_PATH, Screenshot::new(wayland_helper.clone()))?
        .serve_at(DBUS_PATH, ScreenCast::new(wayland_helper))?
        .build()
        .await?;

    future::pending::<()>().await;

    Ok(())
}
