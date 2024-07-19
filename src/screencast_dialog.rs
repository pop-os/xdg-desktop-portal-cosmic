use crate::app::{CosmicPortal, OutputState};
use cosmic::iced::{self, window, Limits};
use cosmic::iced_runtime::command::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced_sctk::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface, KeyboardInteractivity, Layer,
};
use cosmic::{theme, widget};
use cosmic_client_toolkit::sctk::output::OutputInfo;
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1;
use once_cell::sync::Lazy;
use tokio::sync::mpsc;
use wayland_client::protocol::wl_output::WlOutput;

// TODO translate

pub static SCREENCAST_ID: Lazy<window::Id> = Lazy::new(window::Id::unique);

pub async fn show_screencast_prompt(
    subscription_tx: &mpsc::Sender<crate::subscription::Event>,
    outputs: Vec<(WlOutput, OutputInfo)>,
) -> Option<CaptureSources> {
    dbg!(&outputs);
    let (tx, mut rx) = mpsc::channel(1);
    let args = Args {
        outputs,
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

#[derive(Clone, Debug)]
enum Tab {
    Outputs,
    Windows,
}

#[derive(Clone)]
pub struct Args {
    // TODO multiple arg, etc?
    outputs: Vec<(WlOutput, OutputInfo)>,
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
pub enum CaptureSource {
    Output(WlOutput),
}

#[derive(Clone, Debug)]
pub enum Msg {
    ActivateTab(widget::segmented_button::Entity),
    SelectOutput(WlOutput),
    SelectToplevel(ZcosmicToplevelHandleV1),
    Share,
    Cancel,
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Command<crate::app::Msg> {
    let Some(args) = portal.screencast_args.as_mut() else {
        return cosmic::Command::none();
    };

    match msg {
        Msg::ActivateTab(tab) => {}
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
        portal.screencast_tab_model.insert().text("Outputs");
        portal.screencast_tab_model.insert().text("Windows");
        command = create_dialog();
    } // TODO: else, update dialog? or error.
    portal.screencast_args = Some(args);
    command
}

pub(crate) fn view(portal: &CosmicPortal) -> cosmic::Element<Msg> {
    let Some(args) = portal.screencast_args.as_ref() else {
        return widget::horizontal_space(iced::Length::Fixed(1.0)).into();
    };
    let mut cancel_button = widget::button::standard("Cancel").on_press(Msg::Cancel);
    let mut share_button =
        widget::button::standard("Share").style(cosmic::style::Button::Suggested);
    if !args.capture_sources.outputs.is_empty() || !args.capture_sources.toplevels.is_empty() {
        share_button = share_button.on_press(Msg::Share);
    }

    let tabs =
        widget::tab_bar::horizontal(&portal.screencast_tab_model).on_activate(Msg::ActivateTab);

    let mut list = widget::ListColumn::new();
    for (output, output_info) in &args.outputs {
        let is_selected = args.capture_sources.outputs.contains(output);
        let label = widget::text(output_info.name.as_ref().unwrap());
        let button = widget::button(label)
            .width(iced::Length::Fill)
            .padding(0)
            .style(theme::style::Button::Transparent)
            .selected(is_selected)
            .on_press(Msg::SelectOutput(output.clone()));
        list = list.add(button);
    }
    let control = widget::column::with_children(vec![tabs.into(), list.into()]);
    widget::dialog("Screencast")
        .secondary_action(cancel_button)
        .primary_action(share_button)
        .control(control)
        .into()
}
