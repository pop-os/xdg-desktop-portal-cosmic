use std::collections::HashMap;
use zbus::zvariant;

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

pub struct Screenshot;

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
            crate::PORTAL_RESPONSE_SUCCESS,
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
            crate::PORTAL_RESPONSE_SUCCESS,
            PickColorResult {
                color: (1., 1., 1.),
            },
        )
    }
}
