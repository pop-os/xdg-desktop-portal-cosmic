#![allow(dead_code, unused_variables)]

use zbus::zvariant;

use crate::wayland::WaylandHelper;
use crate::PortalResponse;

#[derive(zvariant::DeserializeDict, zvariant::Type, Debug)]
#[zvariant(signature = "a{sv}")]
struct AccessDialogOptions {
    modal: Option<bool>,
    deny_label: Option<String>,
    grant_label: Option<String>,
    icon: Option<String>,
    choices: Option<Vec<(String, String, Vec<(String, String)>, String)>>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct AccessDialogResult {
    choices: Vec<(String, String)>,
}

pub struct Access {
    wayland_helper: WaylandHelper,
}

impl Access {
    pub fn new(wayland_helper: WaylandHelper) -> Self {
        Self { wayland_helper }
    }
}

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.Access")]
impl Access {
    async fn access_dialog(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        subtitle: &str,
        body: &str,
        option: AccessDialogOptions,
    ) -> PortalResponse<AccessDialogResult> {
        log::debug!("Access dialog {app_id} {parent_window} {title} {subtitle} {body} {option:?}");
        PortalResponse::Success(AccessDialogResult {
            choices: Vec::new(),
        })
    }
}
