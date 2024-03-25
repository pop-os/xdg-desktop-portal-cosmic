#![allow(dead_code, unused_variables)]

use cosmic::iced::wayland::actions::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced::wayland::actions::window::SctkWindowSettings;
use cosmic::iced_sctk::commands::layer_surface::{destroy_layer_surface, get_layer_surface};
use cosmic::iced_sctk::commands::window::{close_window, get_window};
use cosmic::widget::{button, container, dropdown, horizontal_space, icon, text, Row};
use cosmic::{
    iced::{
        widget::{column, row},
        window, Length,
    },
    iced_core::Alignment,
};
use once_cell::sync::Lazy;
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::wayland::WaylandHelper;
use crate::{app::CosmicPortal, fl};
use crate::{subscription, PortalResponse};

#[derive(zvariant::DeserializeDict, zvariant::Type, Debug, Clone)]
#[zvariant(signature = "a{sv}")]
pub(crate) struct AccessDialogOptions {
    modal: Option<bool>,
    deny_label: Option<String>,
    grant_label: Option<String>,
    icon: Option<String>,
    //(ID returned with the response, choices (ID, label), label, initial selection or "" meaning the portal should choose)
    #[allow(clippy::type_complexity)]
    choices: Option<Vec<(String, String, Vec<(String, String)>, String)>>,
}

pub static ACCESS_ID: Lazy<window::Id> = Lazy::new(window::Id::unique);

#[derive(zvariant::SerializeDict, zvariant::Type, Debug, Clone)]
#[zvariant(signature = "a{sv}")]
pub struct AccessDialogResult {
    choices: Vec<(String, String)>,
}

pub struct Access {
    wayland_helper: WaylandHelper,
    tx: Sender<subscription::Event>,
}

impl Access {
    pub fn new(wayland_helper: WaylandHelper, tx: Sender<subscription::Event>) -> Self {
        Self { wayland_helper, tx }
    }
}

#[zbus::dbus_interface(name = "")]
impl Access {
    #[allow(clippy::too_many_arguments)]
    async fn access_dialog(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        subtitle: &str,
        body: &str,
        options: AccessDialogOptions,
    ) -> PortalResponse<AccessDialogResult> {
        // TODO send event to subscription via channel
        // await response via channel
        log::debug!("Access dialog {app_id} {parent_window} {title} {subtitle} {body} {options:?}");
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        if let Err(err) = self
            .tx
            .send(subscription::Event::Access(AccessDialogArgs {
                handle: handle.to_owned(),
                app_id: app_id.to_string(),
                parent_window: parent_window.to_string(),
                title: title.to_string(),
                subtitle: subtitle.to_string(),
                body: body.to_string(),
                options,
                tx,
            }))
            .await
        {
            log::error!("Failed to send access dialog event, {err}");
        }
        if let Some(res) = rx.recv().await {
            res
        } else {
            PortalResponse::Cancelled::<AccessDialogResult>
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Allow,
    Cancel,
    Choice(usize, usize),
}

#[derive(Clone)]
pub(crate) struct AccessDialogArgs {
    pub handle: zvariant::ObjectPath<'static>,
    pub app_id: String,
    pub parent_window: String,
    pub title: String,
    pub subtitle: String,
    pub body: String,
    pub options: AccessDialogOptions,
    pub tx: Sender<PortalResponse<AccessDialogResult>>,
}

impl AccessDialogArgs {
    pub(crate) fn get_surface(&self) -> cosmic::Command<Msg> {
        if self.options.modal.unwrap_or_default() {
            // create a modal surface
            get_window(SctkWindowSettings {
                window_id: *ACCESS_ID,
                app_id: Some(crate::DBUS_NAME.to_string()),
                title: Some(self.title.clone()),
                parent: None, // TODO parse parent window and set parent
                autosize: true,
                resizable: None,
                ..Default::default()
            })
        } else {
            // create a layer surface
            get_layer_surface(SctkLayerSurfaceSettings {
                id: *ACCESS_ID,
                layer: cosmic_client_toolkit::sctk::shell::wlr_layer::Layer::Top,
                keyboard_interactivity:
                    cosmic_client_toolkit::sctk::shell::wlr_layer::KeyboardInteractivity::OnDemand,
                pointer_interactivity: true,
                anchor: cosmic_client_toolkit::sctk::shell::wlr_layer::Anchor::empty(),
                output: cosmic::iced::wayland::actions::layer_surface::IcedOutput::Active,
                namespace: "access portal".to_string(),
                ..Default::default()
            })
        }
    }

    pub(crate) fn destroy_surface(&self) -> cosmic::Command<Msg> {
        if self.options.modal.unwrap_or_default() {
            close_window(*ACCESS_ID)
        } else {
            destroy_layer_surface(*ACCESS_ID)
        }
    }
}

pub(crate) fn view(portal: &CosmicPortal) -> cosmic::Element<Msg> {
    let spacing = portal.core.system_theme().cosmic().spacing;
    let Some(args) = portal.access_args.as_ref() else {
        return text("Oops, no access dialog args").into();
    };

    let choices = &portal.access_choices;
    let mut options = Vec::with_capacity(choices.len() + 3);
    for (i, choice) in choices.iter().enumerate() {
        options.push(dropdown(choice.1.as_slice(), choice.0, move |j| Msg::Choice(i, j)).into());
    }
    options.push(horizontal_space(Length::Fill).into());
    options.push(
        button::text(
            args.options
                .deny_label
                .clone()
                .unwrap_or_else(|| fl!("cancel")),
        )
        .on_press(Msg::Cancel)
        .into(),
    );
    options.push(
        button::text(
            args.options
                .grant_label
                .clone()
                .unwrap_or_else(|| fl!("allow")),
        )
        .on_press(Msg::Allow)
        .style(cosmic::theme::Button::Suggested)
        .into(),
    );

    container(
        column![
            row![
                icon::Icon::from(
                    icon::from_name(
                        args.options
                            .icon
                            .as_ref()
                            .map_or("image-missing", |name| name.as_str())
                    )
                    .size(64)
                )
                .width(Length::Fixed(64.0))
                .height(Length::Fixed(64.0)), // TODO icon for the dialog
                text(args.title.as_str()),
                text(args.subtitle.as_str()),
                text(args.body.as_str()),
            ],
            Row::with_children(options)
                .spacing(spacing.space_xxs as f32) // space_l
                .align_items(Alignment::Center),
        ]
        .spacing(spacing.space_l as f32), // space_l
    )
    .into()
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Command<crate::app::Msg> {
    match msg {
        Msg::Allow => {
            let args = portal.access_args.take().unwrap();
            let tx = args.tx.clone();
            tokio::spawn(async move {
                tx.send(PortalResponse::Success(AccessDialogResult {
                    choices: vec![],
                }))
                .await
            });

            args.destroy_surface()
        }
        Msg::Cancel => {
            let args = portal.access_args.take().unwrap();
            let tx = args.tx.clone();
            tokio::spawn(async move {
                tx.send(PortalResponse::Cancelled::<AccessDialogResult>)
                    .await
            });

            args.destroy_surface()
        }
        Msg::Choice(i, j) => {
            let args = portal.access_args.as_mut().unwrap();
            portal.access_choices[i].0 = Some(j);
            cosmic::iced::Command::none()
        }
    }
    .map(crate::app::Msg::Access)
}
pub fn update_args(
    portal: &mut CosmicPortal,
    msg: AccessDialogArgs,
) -> cosmic::Command<crate::app::Msg> {
    let mut cmds = Vec::with_capacity(2);
    if let Some(args) = portal.access_args.take() {
        // destroy surface and recreate
        cmds.push(args.destroy_surface());
        // send cancelled response
        tokio::spawn(async move {
            let _ = args
                .tx
                .send(PortalResponse::Cancelled::<AccessDialogResult>)
                .await;
        });
    }

    cmds.push(msg.get_surface());
    portal.access_args = Some(msg);
    cosmic::iced::Command::batch(cmds).map(crate::app::Msg::Access)
}
