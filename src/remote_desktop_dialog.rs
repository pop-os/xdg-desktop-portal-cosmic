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
use freedesktop_desktop_entry as fde;
use freedesktop_desktop_entry::unicase::Ascii;
use freedesktop_desktop_entry::{DesktopEntry, get_languages_from_env};
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

    let persist_options: Vec<u32> = (PERSIST_NONE..=persist_mode.min(PERSIST_UNTIL_REVOKED))
        .filter(|mode| persist_mode_label(*mode).is_some())
        .collect();

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
        persist_options,
        selected_persist: 0,
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

fn persist_mode_label(persist_mode: u32) -> Option<String> {
    match persist_mode {
        PERSIST_NONE => Some(fl!("remote-desktop", "persist-none")),
        PERSIST_WHILE_RUNNING => Some(fl!("remote-desktop", "persist-while-running")),
        PERSIST_UNTIL_REVOKED => Some(fl!("remote-desktop", "persist-until-revoked")),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Args {
    session_handle: zvariant::ObjectPath<'static>,
    app_name: Option<String>,
    app_icon: Option<String>,
    device_types: u32,
    persist_options: Vec<u32>,
    selected_persist: usize,
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
                log::error!("Failed to send remote desktop event: {}", err);
            }
        });
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    SelectPersist(usize),
    ActivateTab(widget::segmented_button::Entity),
    SelectOutput(WlOutput),
    SelectToplevel(ExtForeignToplevelHandleV1),
    Allow,
    Cancel,
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Task<crate::app::Msg> {
    match msg {
        Msg::SelectPersist(index) => {
            if let Some(args) = portal.remote_desktop_args.as_mut() {
                args.selected_persist = index;
            }
            cosmic::Task::none()
        }
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
        Msg::Allow => {
            if let Some(args) = portal.remote_desktop_args.take() {
                let persist_mode = args
                    .persist_options
                    .get(args.selected_persist)
                    .copied()
                    .unwrap_or(PERSIST_NONE);
                let capture_sources = args.capture_sources.clone();
                args.send_response(Some(RemoteDesktopResponse {
                    persist_mode,
                    capture_sources,
                }));
                return destroy_layer_surface(*REMOTE_DESKTOP_ID);
            }
            cosmic::Task::none()
        }
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
        if args.source_types.contains(SourceType::Monitor) {
            portal
                .screencast_tab_model
                .insert()
                .data(screencast_dialog::Tab::Outputs)
                .text(fl!("output"));
        }
        if args.source_types.contains(SourceType::Window) {
            portal
                .screencast_tab_model
                .insert()
                .data(screencast_dialog::Tab::Windows)
                .text(fl!("window"));
        }
        portal.screencast_tab_model.activate_position(0);
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

    let persist: Option<cosmic::Element<Msg>> = if args.persist_options.len() > 1 {
        let labels: Vec<String> = args
            .persist_options
            .iter()
            .filter_map(|mode| persist_mode_label(*mode))
            .collect();
        let dropdown = widget::dropdown(labels, Some(args.selected_persist), Msg::SelectPersist);
        Some(
            widget::row::with_children(vec![
                widget::text(fl!("remote-desktop", "remember")).into(),
                dropdown.into(),
            ])
            .spacing(spacing.space_s as f32)
            .align_y(iced::Alignment::Center)
            .into(),
        )
    } else {
        None
    };

    let icon =
        widget::icon::from_name(args.app_icon.as_deref().unwrap_or("image-missing")).size(64);

    let cancel_button = widget::button::standard(fl!("deny")).on_press(Msg::Cancel);
    let allow_button = widget::button::standard(fl!("allow"))
        .class(cosmic::style::Button::Suggested)
        .on_press(Msg::Allow);

    let dialog = if args.screen_cast_enabled {
        let mut header_col = widget::column::with_children(vec![
            widget::text::title3(fl!("remote-desktop")).into(),
            widget::text::body(fl!("remote-desktop", "description", app_name = app_name)).into(),
            devices.into(),
        ])
        .spacing(spacing.space_s as f32);
        if let Some(persist) = persist {
            header_col = header_col.push(persist);
        }
        let header = widget::row::with_children(vec![icon.into(), header_col.into()])
            .spacing(spacing.space_s as f32);

        let sources = screencast_dialog::sources_view(
            &portal.screencast_tab_model,
            &args.outputs,
            &args.toplevels,
            &args.capture_sources,
            Msg::ActivateTab,
            Msg::SelectOutput,
            Msg::SelectToplevel,
        );

        widget::dialog()
            .control(header)
            .control(sources)
            .secondary_action(cancel_button)
            .primary_action(allow_button)
    } else {
        let mut control = widget::column::with_children(vec![devices.into()])
            .spacing(spacing.space_m as f32)
            .align_x(iced::Alignment::Center);
        if let Some(persist) = persist {
            control = control.push(persist);
        }
        widget::dialog()
            .title(fl!("remote-desktop"))
            .body(fl!("remote-desktop", "description", app_name = app_name))
            .icon(icon)
            .control(control)
            .secondary_action(cancel_button)
            .primary_action(allow_button)
    };

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
