use cosmic::cosmic_theme::palette::Srgba;
use futures::future::{AbortHandle, abortable};
use std::{collections::HashMap, future::Future};
use zbus::{
    object_server::{InterfaceRef, SignalEmitter},
    zvariant::{self, OwnedValue},
};

pub use cosmic_portal_config as config;

mod access;
mod app;
mod buffer;
mod documents;
mod file_chooser;
mod localize;
mod screencast;
mod screencast_dialog;
mod screencast_thread;
mod screenshot;
mod subscription;
mod wayland;
mod widget;

static DBUS_NAME: &str = "org.freedesktop.impl.portal.desktop.cosmic";
static DBUS_PATH: &str = "/org/freedesktop/portal/desktop";

const PORTAL_RESPONSE_SUCCESS: u32 = 0;
const PORTAL_RESPONSE_CANCELLED: u32 = 1;
const PORTAL_RESPONSE_OTHER: u32 = 2;

#[derive(zvariant::Type)]
#[zvariant(signature = "(ua{sv})")]
enum PortalResponse<T: zvariant::Type + serde::Serialize> {
    Success(T),
    Cancelled,
    Other,
}

impl<T: zvariant::Type + serde::Serialize> serde::Serialize for PortalResponse<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Success(res) => (PORTAL_RESPONSE_SUCCESS, res).serialize(serializer),
            Self::Cancelled => (
                PORTAL_RESPONSE_CANCELLED,
                HashMap::<String, zvariant::Value>::new(),
            )
                .serialize(serializer),
            Self::Other => (
                PORTAL_RESPONSE_OTHER,
                HashMap::<String, zvariant::Value>::new(),
            )
                .serialize(serializer),
        }
    }
}

struct Request(AbortHandle);

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    fn close(&self) {
        self.0.abort();
    }
}

impl Request {
    async fn run<T, DropFut, F, Fut>(
        connection: &zbus::Connection,
        handle: &zvariant::ObjectPath<'_>,
        on_cancel: F,
        task: Fut,
    ) -> PortalResponse<T>
    where
        T: zvariant::Type + serde::Serialize,
        DropFut: Future<Output = ()>,
        F: FnOnce() -> DropFut,
        Fut: Future<Output = PortalResponse<T>>,
    {
        let (abortable, abort_handle) = abortable(task);
        let _ = connection
            .object_server()
            .at(handle, Request(abort_handle))
            .await;
        let resp = abortable.await;
        let _ = connection
            .object_server()
            .remove::<Request, _>(handle)
            .await;
        match resp {
            Ok(resp) => resp,
            _ => {
                on_cancel().await;
                PortalResponse::Cancelled
            }
        }
    }
}

struct Session<T: Send + Sync + 'static> {
    data: T,
    close_cb: Option<Box<dyn FnOnce(&mut T) + Send + Sync + 'static>>,
}

impl<T: Send + Sync + 'static> Session<T> {
    fn new<F: FnOnce(&mut T) + Send + Sync + 'static>(data: T, cb: F) -> Self {
        Self {
            data,
            close_cb: Some(Box::new(cb)),
        }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl<T: Send + Sync + 'static> Session<T> {
    async fn close(&mut self, #[zbus(signal_context)] signal_ctxt: SignalEmitter<'_>) {
        // XXX error?
        let _ = self.closed(&signal_ctxt).await;
        let _ = signal_ctxt
            .connection()
            .object_server()
            .remove::<Self, _>(signal_ctxt.path())
            .await;
        if let Some(cb) = self.close_cb.take() {
            cb(&mut self.data);
        }
    }

    #[zbus(signal)]
    async fn closed(&self, _signal_ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1 // XXX?
    }
}

impl<Data: Send + Sync + 'static> std::ops::Deref for Session<Data> {
    type Target = Data;

    fn deref(&self) -> &Data {
        &self.data
    }
}

impl<Data: Send + Sync + 'static> std::ops::DerefMut for Session<Data> {
    fn deref_mut(&mut self) -> &mut Data {
        &mut self.data
    }
}

async fn session_interface<Data: Send + Sync + 'static>(
    connection: &zbus::Connection,
    session_handle: &zvariant::ObjectPath<'_>,
) -> Option<InterfaceRef<Session<Data>>> {
    connection
        .object_server()
        .interface(session_handle)
        .await
        .ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ColorScheme {
    /// No preference
    NoPreference,
    /// Prefers dark appearance
    PreferDark,
    /// Prefers light appearance
    PreferLight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Contrast {
    /// No preference
    NoPreference,
    /// Higher contrast
    High,
}

#[derive(Debug, Clone, Copy, zvariant::Value, zvariant::OwnedValue)]
struct Color {
    red: f64,
    green: f64,
    blue: f64,
}

const APPEARANCE_NAMESPACE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME_KEY: &str = "color-scheme";
const ACCENT_COLOR_KEY: &str = "accent-color";
const CONTRAST_KEY: &str = "contrast";

struct Settings {
    pub color_scheme: ColorScheme,
    pub contrast: Contrast,
    pub accent: Srgba<f64>,
}

impl Settings {
    pub fn new() -> Self {
        let theme = cosmic::theme::system_preference();
        let cosmic = theme.cosmic();
        Self {
            contrast: if cosmic.is_high_contrast {
                Contrast::High
            } else {
                Contrast::NoPreference
            },
            color_scheme: if cosmic.is_dark {
                ColorScheme::PreferDark
            } else {
                ColorScheme::PreferLight
            },
            accent: cosmic.accent_color().into_format(),
        }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Settings")]
impl Settings {
    /// Read method (deprecated)
    async fn read(&self, namespace: &str, key: &str) -> zbus::fdo::Result<zvariant::OwnedValue> {
        self.read_one(namespace, key).await
    }

    // TODO globs
    /// ReadAll method
    async fn read_all(
        &self,
        mut namespaces: Vec<&str>,
    ) -> HashMap<String, HashMap<String, OwnedValue>> {
        let mut map = HashMap::new();
        if namespaces.is_empty() {
            namespaces = vec![APPEARANCE_NAMESPACE];
        }
        for ns in namespaces {
            let mut inner = HashMap::new();
            if ns != APPEARANCE_NAMESPACE {
                map.insert(ns.to_string(), inner);
                continue;
            }
            inner.insert(
                COLOR_SCHEME_KEY.to_string(),
                OwnedValue::from(self.color_scheme as u32),
            );
            inner.insert(
                CONTRAST_KEY.to_string(),
                OwnedValue::from(self.contrast as u32),
            );
            if let Ok(value) = OwnedValue::try_from(Color {
                red: self.accent.red,
                green: self.accent.green,
                blue: self.accent.blue,
            }) {
                inner.insert(ACCENT_COLOR_KEY.to_string(), value);
            }
            map.insert(APPEARANCE_NAMESPACE.to_string(), inner);
        }
        map
    }

    /// ReadOne method
    async fn read_one(&self, namespace: &str, key: &str) -> zbus::fdo::Result<OwnedValue> {
        match (namespace, key) {
            (APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY) => {
                Ok(OwnedValue::from(self.color_scheme as u32))
            }
            (APPEARANCE_NAMESPACE, CONTRAST_KEY) => Ok(OwnedValue::from(self.contrast as u32)),
            (APPEARANCE_NAMESPACE, ACCENT_COLOR_KEY) => OwnedValue::try_from(Color {
                red: self.accent.red,
                green: self.accent.green,
                blue: self.accent.blue,
            })
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string())),
            _ => Err(zbus::fdo::Error::Failed(
                "Unknown namespace or key".to_string(),
            )),
        }
    }

    /// SettingChanged signal
    #[zbus(signal)]
    async fn setting_changed(
        &self,
        _signal_ctxt: &SignalEmitter<'_>,
        namespace: &str,
        key: &str,
        value: zvariant::Value<'_>,
    ) -> zbus::Result<()>;

    /// version property
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

fn main() -> cosmic::iced::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    localize::localize();
    app::run()
}
