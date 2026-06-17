use crate::app::CosmicPortal;
use crate::fl;
use crate::remote_desktop::{
    DEVICE_KEYBOARD, DEVICE_POINTER, DEVICE_TOUCHSCREEN, PERSIST_NONE, PERSIST_UNTIL_REVOKED,
    PERSIST_WHILE_RUNNING,
};
use crate::screencast_dialog::{self, CaptureSources};
use crate::wayland::WaylandHelper;
use crate::widget::keyboard_wrapper::KeyboardWrapper;
use ashpd::desktop::screencast::SourceType;
use ashpd::enumflags2::BitFlags;
use cosmic::desktop::fde;
use cosmic::iced::keyboard::Key;
use cosmic::iced::keyboard::key::Named;
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced::{self, window};
use cosmic::widget::{self, autosize};
use cosmic_client_toolkit::sctk::output::OutputInfo;
use cosmic_client_toolkit::toplevel_info::ToplevelInfo;
use fde::unicase::Ascii;
use fde::{DesktopEntry, get_languages_from_env};
use std::sync::LazyLock;
use tokio::sync::mpsc;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use zbus::zvariant;

pub static REMOTE_DESKTOP_ID: LazyLock<window::Id> = LazyLock::new(window::Id::unique);
pub static REMOTE_DESKTOP_WIDGET_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("remote-desktop".to_string()));

#[derive(Clone, Debug)]
pub struct RemoteDesktopResponse {
    pub persist_mode: u32,
    pub capture_sources: CaptureSources,
}

pub async fn hide_remote_desktop_prompt(
    subscription_tx: &mpsc::Sender<crate::subscription::Event>,
    session_handle: &zvariant::ObjectPath<'_>,
) {
    let _ = subscription_tx
        .send(crate::subscription::Event::CancelRemoteDesktop(
            session_handle.to_owned(),
        ))
        .await;
}

#[allow(clippy::too_many_arguments)]
pub async fn show_remote_desktop_prompt(
    subscription_tx: &mpsc::Sender<crate::subscription::Event>,
    session_handle: &zvariant::ObjectPath<'_>,
    app_id: String,
    device_types: u32,
    persist_mode: u32,
    screen_cast_enabled: bool,
    multiple: bool,
    source_types: BitFlags<SourceType>,
    wayland_helper: &WaylandHelper,
) -> Option<RemoteDesktopResponse> {
    let locales = get_languages_from_env();
    let desktop_entries = load_desktop_entries(&locales).await;
    let entry = get_desktop_entry(&desktop_entries, &app_id);
    let app_name = entry.and_then(|x| Some(x.name(&locales)?.into_owned()));
    let app_icon = entry.and_then(|x| Some(x.icon()?.to_string()));

    let (outputs, toplevels) = if screen_cast_enabled {
        screencast_dialog::gather_capture_sources(wayland_helper, &desktop_entries).await
    } else {
        (Vec::new(), Vec::new())
    };

    let (tx, mut rx) = mpsc::channel(1);
    let args = Args {
        session_handle: session_handle.to_owned(),
        app_name,
        app_icon,
        device_types,
        persist_mode: persist_mode.min(PERSIST_UNTIL_REVOKED),
        screen_cast_enabled,
        multiple,
        source_types,
        outputs,
        toplevels,
        capture_sources: Default::default(),
        tx,
    };
    subscription_tx
        .send(crate::subscription::Event::RemoteDesktop(args))
        .await
        .unwrap();
    rx.recv().await.unwrap()
}

async fn load_desktop_entries(locales: &[String]) -> Vec<DesktopEntry> {
    let mut entries = Vec::new();
    for p in fde::Iter::new(fde::default_paths()) {
        if let Ok(data) = tokio::fs::read_to_string(&p).await
            && let Ok(entry) = DesktopEntry::from_str(&p, &data, Some(locales))
        {
            entries.push(entry.to_owned());
        }
    }
    entries
}

fn get_desktop_entry<'a>(entries: &'a [DesktopEntry], id: &str) -> Option<&'a DesktopEntry> {
    fde::find_app_by_id(entries, Ascii::new(id))
}

fn create_dialog() -> cosmic::Task<crate::app::Msg> {
    get_layer_surface(SctkLayerSurfaceSettings {
        id: *REMOTE_DESKTOP_ID,
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        namespace: "remote-desktop".into(),
        layer: Layer::Overlay,
        size: None,
        ..Default::default()
    })
}

fn device_type_icon_label(device_type: u32) -> Option<(&'static str, String)> {
    match device_type {
        DEVICE_KEYBOARD => Some(("input-keyboard-symbolic", fl!("remote-desktop", "keyboard"))),
        DEVICE_POINTER => Some(("input-mouse-symbolic", fl!("remote-desktop", "pointer"))),
        DEVICE_TOUCHSCREEN => Some((
            "input-touchpad-symbolic",
            fl!("remote-desktop", "touchscreen"),
        )),
        _ => None,
    }
}

fn device_name(device_type: u32) -> Option<String> {
    match device_type {
        DEVICE_KEYBOARD => Some(fl!("remote-desktop", "device-keyboard")),
        DEVICE_POINTER => Some(fl!("remote-desktop", "device-pointer")),
        DEVICE_TOUCHSCREEN => Some(fl!("remote-desktop", "device-touchscreen")),
        _ => None,
    }
}

/// The requested devices as they read in a sentence: "keyboard", "keyboard and
/// mouse", "keyboard, mouse and touchscreen"
fn device_list(device_types: u32) -> String {
    let names: Vec<String> = [DEVICE_KEYBOARD, DEVICE_POINTER, DEVICE_TOUCHSCREEN]
        .into_iter()
        .filter(|device_type| device_types & device_type != 0)
        .filter_map(device_name)
        .collect();

    match names.as_slice() {
        [] => fl!("remote-desktop", "device-list-fallback"),
        [name] => name.clone(),
        [rest @ .., last] => {
            // Join right to left, so the final pair gets "x and y" and anything
            // before it is comma separated.
            let mut list = last.clone();
            for (i, name) in rest.iter().rev().enumerate() {
                list = if i == 0 {
                    fl!(
                        "remote-desktop",
                        "device-list-pair",
                        first = name.as_str(),
                        second = list
                    )
                } else {
                    fl!(
                        "remote-desktop",
                        "device-list-comma",
                        first = name.as_str(),
                        second = list
                    )
                };
            }
            list
        }
    }
}

/// Label of the primary action, which grants the persistence the app asked for.
fn allow_label(persist_mode: u32) -> String {
    match persist_mode {
        PERSIST_UNTIL_REVOKED => fl!("always-allow"),
        PERSIST_WHILE_RUNNING => fl!("remote-desktop", "allow-while-running"),
        _ => fl!("allow"),
    }
}

/// Prompt above the source picker, naming the sources it actually offers and
/// saying up front when more than one may be picked.
fn select_sources_label(
    tab_model: &widget::segmented_button::Model<widget::segmented_button::SingleSelect>,
    app_name: &str,
    multiple: bool,
) -> String {
    let outputs = screencast_dialog::has_tab(tab_model, screencast_dialog::Tab::Outputs);
    let windows = screencast_dialog::has_tab(tab_model, screencast_dialog::Tab::Windows);
    match (outputs, windows, multiple) {
        (true, true, false) => fl!(
            "remote-desktop",
            "select-screen-or-window",
            app_name = app_name
        ),
        (true, true, true) => fl!(
            "remote-desktop",
            "select-screens-or-windows",
            app_name = app_name
        ),
        (false, true, false) => fl!("remote-desktop", "select-window", app_name = app_name),
        (false, true, true) => fl!("remote-desktop", "select-windows", app_name = app_name),
        (_, _, false) => fl!("remote-desktop", "select-screen", app_name = app_name),
        (_, _, true) => fl!("remote-desktop", "select-screens", app_name = app_name),
    }
}

#[derive(Debug, Clone)]
pub struct Args {
    session_handle: zvariant::ObjectPath<'static>,
    app_name: Option<String>,
    app_icon: Option<String>,
    device_types: u32,
    persist_mode: u32,
    screen_cast_enabled: bool,
    multiple: bool,
    source_types: BitFlags<SourceType>,
    outputs: Vec<(WlOutput, OutputInfo, Option<widget::image::Handle>)>,
    toplevels: Vec<(ToplevelInfo, Option<String>)>,
    capture_sources: CaptureSources,
    // Should be oneshot, but need `Clone` bound
    tx: mpsc::Sender<Option<RemoteDesktopResponse>>,
}

impl Args {
    fn send_response(self, response: Option<RemoteDesktopResponse>) {
        tokio::spawn(async move {
            if let Err(err) = self.tx.send(response).await {
                tracing::error!("Failed to send remote desktop event: {}", err);
            }
        });
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    ActivateTab(widget::segmented_button::Entity),
    SelectOutput(WlOutput),
    SelectToplevel(ExtForeignToplevelHandleV1),
    Allow,
    AllowOnce,
    Cancel,
}

/// Grant the request and dismiss the dialog, persisting the consent unless the
/// user picked "allow once".
fn allow(portal: &mut CosmicPortal, once: bool) -> cosmic::Task<crate::app::Msg> {
    if let Some(args) = portal.remote_desktop_args.take() {
        let persist_mode = if once {
            PERSIST_NONE
        } else {
            args.persist_mode
        };
        let capture_sources = args.capture_sources.clone();
        args.send_response(Some(RemoteDesktopResponse {
            persist_mode,
            capture_sources,
        }));
        return destroy_layer_surface(*REMOTE_DESKTOP_ID);
    }
    cosmic::Task::none()
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Task<crate::app::Msg> {
    match msg {
        Msg::ActivateTab(tab) => {
            portal.screencast_tab_model.activate(tab);
            cosmic::Task::none()
        }
        Msg::SelectOutput(output) => {
            if let Some(args) = portal.remote_desktop_args.as_mut() {
                args.capture_sources.toggle_output(output, args.multiple);
            }
            cosmic::Task::none()
        }
        Msg::SelectToplevel(toplevel) => {
            if let Some(args) = portal.remote_desktop_args.as_mut() {
                args.capture_sources
                    .toggle_toplevel(toplevel, args.multiple);
            }
            cosmic::Task::none()
        }
        Msg::Allow => allow(portal, false),
        Msg::AllowOnce => allow(portal, true),
        Msg::Cancel => {
            if let Some(args) = portal.remote_desktop_args.take() {
                args.send_response(None);
                return destroy_layer_surface(*REMOTE_DESKTOP_ID);
            }
            cosmic::Task::none()
        }
    }
}

pub fn update_args(portal: &mut CosmicPortal, args: Args) -> cosmic::Task<crate::app::Msg> {
    // If the dialog is already open, cancel the previous request, but re-use the surface.
    let command = if let Some(args) = portal.remote_desktop_args.take() {
        args.send_response(None);
        cosmic::Task::none()
    } else {
        create_dialog()
    };

    portal.screencast_tab_model.clear();
    if args.screen_cast_enabled {
        screencast_dialog::update_tab_model(
            &mut portal.screencast_tab_model,
            args.source_types,
            &args.toplevels,
        );
    }

    portal.remote_desktop_args = Some(args);
    command
}

pub fn cancel(
    portal: &mut CosmicPortal,
    session_handle: zvariant::ObjectPath<'static>,
) -> cosmic::Task<crate::app::Msg> {
    if portal
        .remote_desktop_args
        .as_ref()
        .is_some_and(|args| args.session_handle == session_handle)
    {
        let args = portal.remote_desktop_args.take().unwrap();
        args.send_response(None);
        destroy_layer_surface(*REMOTE_DESKTOP_ID)
    } else {
        cosmic::Task::none()
    }
}

pub(crate) fn view(portal: &CosmicPortal) -> cosmic::Element<'_, Msg> {
    let spacing = portal.core.system_theme().cosmic().spacing;
    let Some(args) = portal.remote_desktop_args.as_ref() else {
        return widget::space::horizontal()
            .width(iced::Length::Fixed(1.0))
            .into();
    };

    let unknown = fl!("unknown-application");
    let app_name = args.app_name.as_deref().unwrap_or(&unknown);

    let mut devices = Vec::new();
    for device_type in [DEVICE_KEYBOARD, DEVICE_POINTER, DEVICE_TOUCHSCREEN] {
        if args.device_types & device_type == 0 {
            continue;
        }
        if let Some((icon_name, label)) = device_type_icon_label(device_type) {
            devices.push(
                widget::column::with_children(vec![
                    widget::icon::from_name(icon_name).size(32).into(),
                    widget::text(label).into(),
                ])
                .spacing(spacing.space_xxs as f32)
                .align_x(iced::Alignment::Center)
                .into(),
            );
        }
    }
    let devices = widget::row::with_children(devices).spacing(spacing.space_l as f32);

    let deny_button = widget::button::standard(fl!("deny")).on_press(Msg::Cancel);
    let allow_button = widget::button::standard(allow_label(args.persist_mode))
        .class(cosmic::style::Button::Suggested)
        .on_press(Msg::Allow);

    let mut dialog = widget::dialog()
        .title(fl!("remote-desktop"))
        .body(fl!(
            "remote-desktop",
            "description",
            app_name = app_name,
            devices = device_list(args.device_types)
        ))
        .control(devices)
        .secondary_action(deny_button)
        .primary_action(allow_button);

    if args.persist_mode != PERSIST_NONE {
        dialog = dialog
            .tertiary_action(widget::button::text(fl!("allow-once")).on_press(Msg::AllowOnce));
    }

    if args.screen_cast_enabled {
        let sources = screencast_dialog::sources_view(
            &portal.screencast_tab_model,
            &args.outputs,
            &args.toplevels,
            &args.capture_sources,
            Msg::ActivateTab,
            Msg::SelectOutput,
            Msg::SelectToplevel,
        );

        dialog = dialog.control(
            widget::column::with_children(vec![
                widget::divider::horizontal::default().into(),
                widget::text::body(select_sources_label(
                    &portal.screencast_tab_model,
                    app_name,
                    args.multiple,
                ))
                .into(),
                sources,
            ])
            .spacing(spacing.space_s as f32),
        );
    } else {
        dialog = dialog.icon(
            widget::icon::from_name(args.app_icon.as_deref().unwrap_or("image-missing")).size(64),
        );
    }

    let content = KeyboardWrapper::new(dialog, |key, _| match key {
        Key::Named(Named::Enter) => Some(Msg::Allow),
        Key::Named(Named::Escape) => Some(Msg::Cancel),
        _ => None,
    });

    autosize::autosize(content, REMOTE_DESKTOP_WIDGET_ID.clone())
        .min_width(1.)
        .min_height(1.)
        .max_width(572.)
        .max_height(884.)
        .into()
}
