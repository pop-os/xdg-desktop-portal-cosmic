use crate::app::{CosmicPortal, OutputState};
use cosmic::iced::{self, window, Limits};
use cosmic::iced_runtime::command::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced_sctk::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface, KeyboardInteractivity, Layer,
};
use cosmic::{theme, widget};
use cosmic_client_toolkit::sctk::output::OutputInfo;
use once_cell::sync::Lazy;
use tokio::sync::mpsc;
use wayland_client::protocol::wl_output::WlOutput;

// TODO translate

pub static SCREENCAST_ID: Lazy<window::Id> = Lazy::new(window::Id::unique);

pub async fn show_screencast_prompt(
    subscription_tx: &mpsc::Sender<crate::subscription::Event>,
    outputs: Vec<(WlOutput, OutputInfo)>,
) -> Option<DialogResult> {
    dbg!(&outputs);
    let (tx, mut rx) = mpsc::channel(1);
    let args = Args { outputs, tx, capture_source: None };
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
pub struct DialogResult {
    pub capture_source: CaptureSource,
}

#[derive(Clone)]
pub struct Args {
    // TODO multiple
    outputs: Vec<(WlOutput, OutputInfo)>,
    // Should be oneshot, but need `Clone` bound
    tx: mpsc::Sender<Option<DialogResult>>,
    capture_source: Option<CaptureSource>,
}

#[derive(Clone, Debug)]
pub enum CaptureSource {
    Output(WlOutput),
}

#[derive(Clone, Debug)]
pub enum Msg {
    Cancel,
    Share,
    SelectOutput(WlOutput),
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Command<crate::app::Msg> {
    let Some(args) = portal.screencast_args.as_mut() else {
        return cosmic::Command::none();
    };

    match msg {
        Msg::SelectOutput(output) => {
            args.capture_source = Some(CaptureSource::Output(output));
        }
        Msg::Share => {
            if let Some(args) = portal.screencast_args.take() {
                let tx = args.tx;
                let response = args.capture_source.map(|capture_source| {
                    DialogResult { capture_source }
                });
                tokio::spawn(async move {
                    if let Err(err) = tx.send(response).await {
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
    let mut share_button = widget::button::standard("Share");
    if args.capture_source.is_some() {
        share_button = share_button.on_press(Msg::Share);
    }
    //let mut items = Vec::new();
    let mut list = widget::ListColumn::new();
    for (output, output_info) in &args.outputs {
        let label = widget::text(output_info.name.as_ref().unwrap());
        let button = cosmic::iced::widget::button(label)
            .width(iced::Length::Fill)
            .padding(0)
            .style(theme::iced::Button::Transparent)
            .on_press(Msg::SelectOutput(output.clone()));
        list = list.add(button);
    }
    //let control = widget::Column::with_children(items);
    let control = list;
    // WIP
    widget::dialog("Screencast")
        .secondary_action(cancel_button)
        .primary_action(share_button)
        .control(control)
        .into()
}
