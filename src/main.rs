use std::{collections::HashMap, future};

mod documents;
mod screenshot;
use screenshot::Screenshot;

static DBUS_NAME: &str = "org.freedesktop.impl.portal.desktop.cosmic";
static DBUS_PATH: &str = "/org/freedesktop/portal/desktop";

const PORTAL_RESPONSE_SUCCESS: u32 = 0;
const PORTAL_RESPONSE_CANCELLED: u32 = 1;
const PORTAL_RESPONSE_OTHER: u32 = 2;

// org.freedesktop.impl.portal.Request/org.freedesktop.impl.portal.Session
// - implemented by objects at different paths
// org.freedesktop.impl.portal.Inhibit
// org.freedesktop.impl.portal.Screenshot
// - save to /run/user/$UID/doc/ with document portal fuse filesystem
//
// zbus: implement multiple interfaces at one path?

struct Request;

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    fn close(&self) {}
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    let connection = zbus::ConnectionBuilder::session()?
        .name(DBUS_NAME)?
        .serve_at(DBUS_PATH, Screenshot)?
        .build()
        .await?;

    future::pending::<()>().await;

    Ok(())
}
