use std::future;

mod documents;
mod screenshot;
use screenshot::Screenshot;
mod screencast;
use screencast::ScreenCast;

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

struct Session;

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.Session")]
impl Session {
    async fn close(&self, #[zbus(signal_context)] signal_ctxt: zbus::SignalContext<'_>) {
        self.closed(&signal_ctxt).await;
        signal_ctxt
            .connection()
            .object_server()
            .remove::<Self, _>(signal_ctxt.path());
        // Notifiy ScreenCast, etc.
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
    let connection = zbus::ConnectionBuilder::session()?
        .name(DBUS_NAME)?
        .serve_at(DBUS_PATH, Screenshot)?
        .serve_at(DBUS_PATH, ScreenCast)?
        .build()
        .await?;

    future::pending::<()>().await;

    Ok(())
}
