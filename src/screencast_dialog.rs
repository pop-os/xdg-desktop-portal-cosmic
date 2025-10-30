use crate::app::CosmicPortal;
use crate::fl;
use crate::wayland::{CaptureSource, WaylandHelper};
use crate::widget::keyboard_wrapper::KeyboardWrapper;
use ashpd::{desktop::screencast::SourceType, enumflags2::BitFlags};
use cosmic::desktop::IconSourceExt;
use cosmic::iced::{
    self,
    keyboard::{Key, key::Named},
    window,
};
use fde::IconSource;

use cosmic::iced_runtime::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced_winit::commands::layer_surface::{
    KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
};
use cosmic::widget::autosize;
use cosmic::{theme, widget};
use cosmic_client_toolkit::sctk::output::OutputInfo;
use cosmic_client_toolkit::toplevel_info::ToplevelInfo;
use freedesktop_desktop_entry as fde;
use freedesktop_desktop_entry::unicase::Ascii;
use freedesktop_desktop_entry::{DesktopEntry, get_languages_from_env};
use std::mem;
use std::sync::LazyLock;
use tokio::sync::mpsc;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use zbus::zvariant;

pub static SCREENCAST_ID: LazyLock<window::Id> = LazyLock::new(window::Id::unique);
pub static SCREENCAST_WIDGET_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("screencast".to_string()));

pub async fn hide_screencast_prompt(
    subscription_tx: &mpsc::Sender<crate::subscription::Event>,
    session_handle: &zvariant::ObjectPath<'_>,
) {
    let _ = subscription_tx
        .send(crate::subscription::Event::CancelScreencast(
            session_handle.to_owned(),
        ))
        .await;
}

pub async fn show_screencast_prompt(
    subscription_tx: &mpsc::Sender<crate::subscription::Event>,
    session_handle: &zvariant::ObjectPath<'_>,
    app_id: String,
    multiple: bool,
    source_types: BitFlags<SourceType>,
    wayland_helper: &WaylandHelper,
) -> Option<CaptureSources> {
    let locales = get_languages_from_env();
    let desktop_entries = load_desktop_entries(&locales).await;

    let toplevels = wayland_helper
        .toplevels()
        .into_iter()
        .map(|info| {
            let icon = get_desktop_entry(&desktop_entries, &info.app_id)
                .and_then(|x| Some(x.icon()?.to_string()));
            (info, icon)
        })
        .collect();

    let mut outputs = Vec::new();
    for output in wayland_helper.outputs() {
        let Some(info) = wayland_helper.output_info(&output) else {
            continue;
        };
        let source = CaptureSource::Output(output.clone());
        let image = wayland_helper
            .capture_source_shm(source, false)
            .await
            .and_then(|image| image.image_transformed().ok())
            .map(|image| {
                widget::image::Handle::from_rgba(image.width(), image.height(), image.into_vec())
            });
        outputs.push((output, info, image));
    }

    let app_name = get_desktop_entry(&desktop_entries, &app_id)
        .and_then(|x| Some(x.name(&locales)?.into_owned()));

    let (tx, mut rx) = mpsc::channel(1);
    let args = Args {
        session_handle: session_handle.to_owned(),
        outputs,
        toplevels,
        multiple,
        source_types,
        app_name,
        tx,
        capture_sources: Default::default(),
    };
    subscription_tx
        .send(crate::subscription::Event::Screencast(args))
        .await
        .unwrap();
    rx.recv().await.unwrap()
}

async fn load_desktop_entries(locales: &[String]) -> Vec<DesktopEntry> {
    let mut entries = Vec::new();
    for p in fde::Iter::new(fde::default_paths()) {
        if let Ok(data) = tokio::fs::read_to_string(&p).await
            && let Ok(entry) = DesktopEntry::from_str(&p, &data, Some(locales)) {
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
        id: *SCREENCAST_ID,
        keyboard_interactivity: KeyboardInteractivity::Exclusive,
        namespace: "screencast".into(),
        layer: Layer::Overlay,
        size: None,
        ..Default::default()
    })
}

#[derive(Clone, Copy, Debug)]
enum Tab {
    Outputs,
    Windows,
}

#[derive(Debug, Clone)]
pub struct Args {
    session_handle: zvariant::ObjectPath<'static>,
    multiple: bool,
    source_types: BitFlags<SourceType>,
    outputs: Vec<(WlOutput, OutputInfo, Option<widget::image::Handle>)>,
    toplevels: Vec<(ToplevelInfo, Option<String>)>,
    app_name: Option<String>,
    // Should be oneshot, but need `Clone` bound
    tx: mpsc::Sender<Option<CaptureSources>>,
    capture_sources: CaptureSources,
}

impl Args {
    fn send_response(self, response: Option<CaptureSources>) {
        tokio::spawn(async move {
            if let Err(err) = self.tx.send(response).await {
                log::error!("Failed to send screencast event: {}", err);
            }
        });
    }
}

// TODO order?
#[derive(Clone, Debug, Default)]
pub struct CaptureSources {
    pub outputs: Vec<WlOutput>,
    pub toplevels: Vec<ExtForeignToplevelHandleV1>,
}

impl CaptureSources {
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty() && self.toplevels.is_empty()
    }

    pub fn clear(&mut self) {
        self.outputs.clear();
        self.toplevels.clear();
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    ActivateTab(widget::segmented_button::Entity),
    SelectOutput(WlOutput),
    SelectToplevel(ExtForeignToplevelHandleV1),
    Share,
    Cancel,
}

fn active_tab(portal: &CosmicPortal) -> Tab {
    *portal.screencast_tab_model.active_data::<Tab>().unwrap()
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Task<crate::app::Msg> {
    let Some(args) = portal.screencast_args.as_mut() else {
        return cosmic::Task::none();
    };

    match msg {
        Msg::ActivateTab(tab) => {
            portal.screencast_tab_model.activate(tab);
        }
        Msg::SelectOutput(output) => {
            if let Some(idx) = args
                .capture_sources
                .outputs
                .iter()
                .position(|x| x == &output)
            {
                args.capture_sources.outputs.remove(idx);
            } else {
                if !args.multiple && !args.capture_sources.is_empty() {
                    args.capture_sources.clear();
                }
                args.capture_sources.outputs.push(output);
            }
        }
        Msg::SelectToplevel(toplevel) => {
            if let Some(idx) = args
                .capture_sources
                .toplevels
                .iter()
                .position(|t| t == &toplevel)
            {
                args.capture_sources.toplevels.remove(idx);
            } else {
                if !args.multiple && !args.capture_sources.is_empty() {
                    args.capture_sources.clear();
                }
                args.capture_sources.toplevels.push(toplevel);
            }
        }
        Msg::Share => {
            if let Some(mut args) = portal.screencast_args.take() {
                let response = mem::take(&mut args.capture_sources);
                args.send_response(Some(response));
                return destroy_layer_surface(*SCREENCAST_ID);
            }
        }
        Msg::Cancel => {
            if let Some(args) = portal.screencast_args.take() {
                args.send_response(None);
                return destroy_layer_surface(*SCREENCAST_ID);
            }
        }
    }
    cosmic::Task::none()
}

pub fn update_args(portal: &mut CosmicPortal, args: Args) -> cosmic::Task<crate::app::Msg> {
    // If the dialog is already open, cancel previous request, but re-use dialog surface
    let command = if let Some(args) = portal.screencast_args.take() {
        args.send_response(None);
        cosmic::Task::none()
    } else {
        create_dialog()
    };

    portal.screencast_tab_model.clear();
    if args.source_types.contains(SourceType::Monitor) {
        portal
            .screencast_tab_model
            .insert()
            .data(Tab::Outputs)
            .text(fl!("output"));
    }
    if args.source_types.contains(SourceType::Window) {
        portal
            .screencast_tab_model
            .insert()
            .data(Tab::Windows)
            .text(fl!("window"));
    }
    portal.screencast_tab_model.activate_position(0);

    portal.screencast_args = Some(args);

    command
}

pub fn cancel(
    portal: &mut CosmicPortal,
    session_handle: zvariant::ObjectPath<'static>,
) -> cosmic::Task<crate::app::Msg> {
    if portal
        .screencast_args
        .as_ref()
        .is_some_and(|args| args.session_handle == session_handle)
    {
        let args = portal.screencast_args.take().unwrap();
        args.send_response(None);
        destroy_layer_surface(*SCREENCAST_ID)
    } else {
        cosmic::Task::none()
    }
}

fn output_button_appearance(
    theme: &cosmic::Theme,
    is_active: bool,
    hovered: bool,
) -> widget::button::Style {
    let cosmic = theme.cosmic();
    let mut appearance = widget::button::Style::new();
    appearance.border_radius = cosmic.corner_radii.radius_s.into();
    if is_active {
        appearance.border_width = 2.0;
        appearance.border_color = cosmic.accent.base.into();
    }
    if hovered {
        appearance.background = Some(iced::Background::Color(cosmic.button.base.into()));
    }
    appearance
}

fn output_button<'a>(
    label: &'a str,
    is_selected: bool,
    image_handle: Option<&'a widget::image::Handle>,
    msg: Msg,
) -> cosmic::Element<'a, Msg> {
    let text = widget::text(label).class(theme::style::Text::Custom(|theme| {
        let container = theme.current_container();
        cosmic::iced_core::widget::text::Style {
            color: Some(container.on.into()),
        }
    }));
    let mut row_children = vec![text.into()];
    if is_selected {
        row_children.push(widget::text("✓").into());
    }
    let row = widget::row::with_children(row_children).spacing(12);

    let mut children = Vec::new();
    if let Some(image_handle) = image_handle {
        children.push(widget::image::Image::new(image_handle.clone()).into());
    }
    children.push(row.into());
    let column = widget::column::with_children(children).spacing(12);

    widget::button::custom(column)
        .width(iced::Length::Fill)
        .padding(8)
        .selected(is_selected)
        .class(cosmic::theme::Button::Custom {
            active: Box::new(move |_focused, theme| {
                output_button_appearance(theme, is_selected, false)
            }),
            disabled: Box::new(|_theme| unreachable!()),
            hovered: Box::new(move |_focused, theme| {
                output_button_appearance(theme, is_selected, true)
            }),
            pressed: Box::new(move |_focused, theme| {
                output_button_appearance(theme, is_selected, true)
            }),
        })
        .on_press(msg)
        .into()
}

fn toplevel_button(
    label: &str,
    is_selected: bool,
    icon: IconSource,
    msg: Msg,
) -> cosmic::Element<'_, Msg> {
    let text = widget::text(label).class(theme::style::Text::Custom(|theme| {
        let container = theme.current_container();
        cosmic::iced_core::widget::text::Style {
            color: Some(container.on.into()),
        }
    }));
    let button = widget::button::custom(text)
        .width(iced::Length::Fill)
        .padding(0)
        // TODO hover style? Etc.
        // .style(theme::style::Button::Text)
        .class(theme::style::Button::Transparent)
        .selected(is_selected)
        .on_press(msg);
    let mut children = Vec::new();
    children.push(icon.as_cosmic_icon().size(24).into());
    children.push(button.into());
    // TODO
    if is_selected {
        children.push(widget::text("✓").into());
    }
    widget::row::with_children(children).spacing(12).into()
}

pub(crate) fn view(portal: &CosmicPortal) -> cosmic::Element<'_, Msg> {
    let Some(args) = portal.screencast_args.as_ref() else {
        return widget::horizontal_space()
            .width(iced::Length::Fixed(1.0))
            .into();
    };
    let cancel_button = widget::button::standard(fl!("cancel")).on_press(Msg::Cancel);
    let mut share_button =
        widget::button::standard(fl!("share")).class(cosmic::style::Button::Suggested);
    if !args.capture_sources.is_empty() {
        share_button = share_button.on_press(Msg::Share);
    }

    let tabs =
        widget::tab_bar::horizontal(&portal.screencast_tab_model).on_activate(Msg::ActivateTab);

    let list: cosmic::Element<_> = match active_tab(portal) {
        Tab::Outputs => {
            let mut children = Vec::new();
            for (output, output_info, image_handle) in &args.outputs {
                let label = output_info.name.as_ref().unwrap();
                let is_selected = args.capture_sources.outputs.contains(output);
                children.push(output_button(
                    label,
                    is_selected,
                    image_handle.as_ref(),
                    Msg::SelectOutput(output.clone()),
                ));
            }
            widget::row::with_children(children).spacing(8).into()
        }
        Tab::Windows => {
            let mut list = widget::ListColumn::new();
            for (toplevel_info, icon) in &args.toplevels {
                let icon = IconSource::from_unknown(icon.as_deref().unwrap_or_default());
                let label = &toplevel_info.title;
                let is_selected = args
                    .capture_sources
                    .toplevels.contains(&toplevel_info.foreign_toplevel);
                list = list.add(toplevel_button(
                    label,
                    is_selected,
                    icon,
                    Msg::SelectToplevel(toplevel_info.foreign_toplevel.clone()),
                ));
            }
            if args.toplevels.len() > 8 {
                widget::container(cosmic::widget::scrollable(list))
                    .max_height(380.)
                    .width(iced::Length::Fill)
                    .into()
            } else {
                list.into()
            }
        }
    };

    let unknown = fl!("unknown-application");
    let app_name = args.app_name.as_deref().unwrap_or(&unknown);

    let control = widget::column::with_children(vec![tabs.into(), list]).spacing(8);
    autosize::autosize(
        KeyboardWrapper::new(
            widget::dialog()
                .title(fl!("share-screen"))
                // TODO adjust text for multiple select, types?
                .body(fl!("share-screen", "description", app_name = app_name))
                .secondary_action(cancel_button)
                .primary_action(share_button)
                .control(control),
            |key, _| match key {
                Key::Named(Named::Enter) => Some(Msg::Share),
                Key::Named(Named::Escape) => Some(Msg::Cancel),
                _ => None,
            },
        ),
        SCREENCAST_WIDGET_ID.clone(),
    )
    .max_width(572.)
    .max_height(884.)
    .min_width(1.)
    .min_height(1.)
    .into()
}
