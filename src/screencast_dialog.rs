use crate::app::{CosmicPortal, OutputState};
use crate::fl;
use crate::wayland::{CaptureSource, WaylandHelper};
use crate::widget::screenshot::MyImage;
use cosmic::desktop::IconSource;
use cosmic::iced::{self, window, Limits};
use cosmic::iced_runtime::command::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced_sctk::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface, KeyboardInteractivity, Layer,
};
use cosmic::{theme, widget};
use cosmic_client_toolkit::sctk::output::OutputInfo;
use cosmic_client_toolkit::toplevel_info::ToplevelInfo;
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1;
use freedesktop_desktop_entry as fde;
use freedesktop_desktop_entry::{get_languages_from_env, DesktopEntry};
use once_cell::sync::Lazy;
use std::fs;
use std::sync::Arc;
use tokio::sync::mpsc;
use wayland_client::protocol::wl_output::WlOutput;

// TODO translate

pub static SCREENCAST_ID: Lazy<window::Id> = Lazy::new(window::Id::unique);

pub async fn show_screencast_prompt(
    subscription_tx: &mpsc::Sender<crate::subscription::Event>,
    outputs: Vec<(WlOutput, OutputInfo)>,
    toplevels: Vec<(ZcosmicToplevelHandleV1, ToplevelInfo)>,
    wayland_helper: &WaylandHelper,
) -> Option<CaptureSources> {
    let desktop_entries = load_desktop_entries().await;
    let toplevels = toplevels
        .into_iter()
        .map(|(handle, info)| {
            let icon = get_desktop_entry_icon(&desktop_entries, info.app_id.as_str());
            (handle, info, icon)
        })
        .collect();

    let mut outputs2 = Vec::new();
    for (output, info) in outputs {
        let source = CaptureSource::Output(output.clone());
        let image = wayland_helper
            .capture_source_shm(source, false)
            .await
            .and_then(|image| image.image().ok())
            .map(|image| {
                widget::image::Handle::from_pixels(
                    image.width(),
                    image.height(),
                    MyImage(Arc::new(image)),
                )
            });
        outputs2.push((output, info, image));
    }

    let (tx, mut rx) = mpsc::channel(1);
    let args = Args {
        outputs: outputs2,
        toplevels,
        tx,
        capture_sources: Default::default(),
        tab: Tab::Outputs, // TODO
    };
    subscription_tx
        .send(crate::subscription::Event::Screencast(args))
        .await
        .unwrap();
    rx.recv().await.unwrap()
}

fn load_desktop_entry_from_app_ids<I, L>(id: I, locales: &[L]) -> DesktopEntry<'static>
where
    I: AsRef<str>,
    L: AsRef<str>,
{
    let srcs = fde::Iter::new(fde::default_paths())
        .filter_map(|p| fs::read_to_string(&p).ok().and_then(|e| Some((p, e))))
        .collect::<Vec<_>>();

    let entries = srcs
        .iter()
        .filter_map(|(p, data)| DesktopEntry::from_str(p, data, Some(locales)).ok())
        .collect::<Vec<_>>();

    fde::matching::get_best_match(
        &[&id],
        &entries,
        fde::matching::MatchAppIdOptions::default(),
    )
    .unwrap_or(&fde::DesktopEntry::from_appid(id.as_ref()))
    .to_owned()
}

async fn load_desktop_entries() -> Vec<DesktopEntry<'static>> {
    let locales = get_languages_from_env();

    let mut entries = Vec::new();
    for p in fde::Iter::new(fde::default_paths()) {
        if let Ok(data) = tokio::fs::read_to_string(&p).await {
            if let Ok(entry) = DesktopEntry::from_str(&p, &data, Some(&locales)) {
                entries.push(entry.to_owned());
            }
        }
    }
    entries
}

fn get_desktop_entry_icon(entries: &[DesktopEntry<'_>], id: &str) -> Option<String> {
    fde::matching::get_best_match(&[id], &entries, fde::matching::MatchAppIdOptions::default())
        .unwrap_or(&fde::DesktopEntry::from_appid(id.as_ref()))
        .icon()
        .map(|x| x.to_string())
}

fn create_dialog() -> cosmic::Command<crate::app::Msg> {
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

#[derive(Clone)]
pub struct Args {
    // TODO multiple arg, etc?
    outputs: Vec<(WlOutput, OutputInfo, Option<widget::image::Handle>)>,
    toplevels: Vec<(ZcosmicToplevelHandleV1, ToplevelInfo, Option<String>)>,
    // Should be oneshot, but need `Clone` bound
    tx: mpsc::Sender<Option<CaptureSources>>,
    capture_sources: CaptureSources,
    tab: Tab,
}

// TODO order?
#[derive(Clone, Debug, Default)]
pub struct CaptureSources {
    pub outputs: Vec<WlOutput>,
    pub toplevels: Vec<ZcosmicToplevelHandleV1>,
}

#[derive(Clone, Debug)]
pub enum Msg {
    ActivateTab(widget::segmented_button::Entity),
    SelectOutput(WlOutput),
    SelectToplevel(ZcosmicToplevelHandleV1),
    Share,
    Cancel,
}

fn active_tab(portal: &CosmicPortal) -> Tab {
    *portal.screencast_tab_model.active_data::<Tab>().unwrap()
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Command<crate::app::Msg> {
    let Some(args) = portal.screencast_args.as_mut() else {
        return cosmic::Command::none();
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
                args.capture_sources.outputs.push(output);
            }
        }
        Msg::SelectToplevel(toplevel) => {
            if let Some(idx) = args
                .capture_sources
                .toplevels
                .iter()
                .position(|x| x == &toplevel)
            {
                args.capture_sources.toplevels.remove(idx);
            } else {
                args.capture_sources.toplevels.push(toplevel);
            }
        }
        Msg::Share => {
            if let Some(args) = portal.screencast_args.take() {
                let tx = args.tx;
                let response = args.capture_sources;
                tokio::spawn(async move {
                    if let Err(err) = tx.send(Some(response)).await {
                        log::error!("Failed to send screencast event");
                    }
                });
                return destroy_layer_surface(*SCREENCAST_ID);
            }
        }
        Msg::Cancel => {
            if let Some(args) = portal.screencast_args.take() {
                let tx = args.tx;
                tokio::spawn(async move {
                    if let Err(err) = tx.send(None).await {
                        log::error!("Failed to send screencast event");
                    }
                });
                return destroy_layer_surface(*SCREENCAST_ID);
            }
        }
    }
    cosmic::Command::none()
}

pub fn update_args(portal: &mut CosmicPortal, args: Args) -> cosmic::Command<crate::app::Msg> {
    let mut command = cosmic::Command::none();
    if portal.screencast_args.is_none() {
        portal.screencast_tab_model.clear();
        portal
            .screencast_tab_model
            .insert()
            .data(Tab::Outputs)
            .text("Output");
        portal
            .screencast_tab_model
            .insert()
            .data(Tab::Windows)
            .text("Window");
        portal.screencast_tab_model.activate_position(0); // XXX
        command = create_dialog();
    } // TODO: else, update dialog? or error.
    portal.screencast_args = Some(args);
    command
}

fn output_button<'a>(
    label: &'a str,
    is_selected: bool,
    image_handle: Option<&'a widget::image::Handle>,
    msg: Msg,
) -> cosmic::Element<'a, Msg> {
    let text = widget::text(label).style(theme::style::Text::Custom(|theme| {
        let container = theme.current_container();
        cosmic::iced_core::widget::text::Appearance {
            color: Some(container.on.into()),
        }
    }));
    let button = widget::button(text)
        .width(iced::Length::Fill)
        .padding(0)
        // TODO hover style? Etc.
        // .style(theme::style::Button::Text)
        .style(theme::style::Button::Transparent)
        .selected(is_selected)
        .on_press(msg);
    let mut children = Vec::new();
    if let Some(image_handle) = image_handle {
        children.push(widget::image::Image::new(image_handle.clone()).into());
    }
    children.push(button.into());
    // TODO
    if is_selected {
        children.push(widget::text("✓").into());
    }
    widget::column::with_children(children).spacing(12).into()
}

fn toplevel_button(
    label: &str,
    is_selected: bool,
    icon: IconSource,
    msg: Msg,
) -> cosmic::Element<Msg> {
    let text = widget::text(label).style(theme::style::Text::Custom(|theme| {
        let container = theme.current_container();
        cosmic::iced_core::widget::text::Appearance {
            color: Some(container.on.into()),
        }
    }));
    let button = widget::button(text)
        .width(iced::Length::Fill)
        .padding(0)
        // TODO hover style? Etc.
        // .style(theme::style::Button::Text)
        .style(theme::style::Button::Transparent)
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

pub(crate) fn view(portal: &CosmicPortal) -> cosmic::Element<Msg> {
    let Some(args) = portal.screencast_args.as_ref() else {
        return widget::horizontal_space(iced::Length::Fixed(1.0)).into();
    };
    let mut cancel_button = widget::button::standard(fl!("cancel")).on_press(Msg::Cancel);
    let mut share_button =
        widget::button::standard(fl!("share")).style(cosmic::style::Button::Suggested);
    if !args.capture_sources.outputs.is_empty() || !args.capture_sources.toplevels.is_empty() {
        share_button = share_button.on_press(Msg::Share);
    }

    let tabs =
        widget::tab_bar::horizontal(&portal.screencast_tab_model).on_activate(Msg::ActivateTab);

    let mut list = widget::ListColumn::new();
    match active_tab(portal) {
        Tab::Outputs => {
            for (output, output_info, image_handle) in &args.outputs {
                let label = output_info.name.as_ref().unwrap();
                let is_selected = args.capture_sources.outputs.contains(output);
                list = list.add(output_button(
                    label,
                    is_selected,
                    image_handle.as_ref(),
                    Msg::SelectOutput(output.clone()),
                ));
            }
        }
        Tab::Windows => {
            for (toplevel, toplevel_info, icon) in &args.toplevels {
                let icon = IconSource::from_unknown(icon.as_deref().unwrap_or_default());
                let label = &toplevel_info.title;
                let is_selected = args.capture_sources.toplevels.contains(toplevel);
                list = list.add(toplevel_button(
                    label,
                    is_selected,
                    icon,
                    Msg::SelectToplevel(toplevel.clone()),
                ));
            }
        }
    }

    // XXX
    let app_name = "APP NAME";

    // TODO adjust text for multiple select, types?
    let description = format!("The system wants to share the contents of your screen with \"{}\". Select a screen or window to share.", app_name);

    let control = widget::column::with_children(vec![tabs.into(), list.into()]);

    widget::dialog("Share your screen")
        .body(description)
        .secondary_action(cancel_button)
        .primary_action(share_button)
        .control(control)
        .into()
}
