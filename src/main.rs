use std::{collections::HashMap, future};
use zbus::zvariant;

mod documents;

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

#[derive(zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct ScreenshotOptions {
    modal: Option<bool>,
    interactive: Option<bool>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct ScreenshotResult {
    uri: String,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct PickColorResult {
    color: (f64, f64, f64), // (ddd)
}

struct Screenshot;

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl Screenshot {
    async fn screenshot(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        option: ScreenshotOptions,
    ) -> (u32, ScreenshotResult) {
        // connection.object_server().at(&handle, Request);

        // TODO create handle
        // XXX
        std::fs::copy(
            "/usr/share/backgrounds/pop/kate-hazen-COSMIC-desktop-wallpaper.png",
            "/tmp/out.png",
        );

        // connection.object_server().remove::<Request, _>(&handle);
        (
            PORTAL_RESPONSE_SUCCESS,
            ScreenshotResult {
                uri: format!("file:///tmp/out.png"),
            },
        )
    }

    async fn pick_color(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        option: HashMap<String, zvariant::Value<'_>>,
    ) -> (u32, PickColorResult) {
        // TODO create handle
        // XXX
        (
            PORTAL_RESPONSE_SUCCESS,
            PickColorResult {
                color: (1., 1., 1.),
            },
        )
    }
}

/*
Screenshot (IN  o     handle,
            IN  s     app_id,
            IN  s     parent_window,
            IN  a{sv} options,
            OUT u     response,
            OUT a{sv} results);

ressults has uri:string

PickColor  (IN  o     handle,
            IN  s     app_id,
            IN  s     parent_window,
            IN  a{sv} options,
            OUT u     response,
            OUT a{sv} results);
 */

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
