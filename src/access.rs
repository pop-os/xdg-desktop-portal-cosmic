#![allow(dead_code, unused_variables)]

use cosmic::iced_runtime::platform_specific::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced_winit::commands::layer_surface::{destroy_layer_surface, get_layer_surface};
use cosmic::widget::autosize::autosize;
use cosmic::widget::{self, Column, Id, button, dropdown, icon, text};
use cosmic::{
    iced::{
        keyboard::{Key, key::Named},
        widget::{column, row},
        window,
    },
    iced_core::Alignment,
};
use std::collections::HashMap;
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::wayland::WaylandHelper;
use crate::widget::keyboard_wrapper::KeyboardWrapper;
use crate::{PortalResponse, subscription};
use crate::{app::CosmicPortal, fl};

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

#[zbus::interface(name = "org.freedesktop.impl.portal.Access")]
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
        // `widget::dialog` needs a slice of labels
        let choice_labels: Vec<Vec<String>> = options
            .choices
            .iter()
            .flatten()
            .map(|(_, _, choices, _)| choices.iter().map(|(_, label)| label.clone()).collect())
            .collect();
        let active_choices = options
            .choices
            .iter()
            .flatten()
            .map(|(id, _, _, initial)| (id.clone(), initial.clone()))
            .filter(|(_, value)| !value.is_empty())
            .collect();
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
                active_choices,
                choice_labels,
                tx,
                access_id: window::Id::NONE,
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
    Ignore,
}

#[derive(Clone, Debug)]
pub(crate) struct AccessDialogArgs {
    pub handle: zvariant::ObjectPath<'static>,
    pub app_id: String,
    pub parent_window: String,
    pub title: String,
    pub subtitle: String,
    pub body: String,
    pub options: AccessDialogOptions,
    pub active_choices: HashMap<String, String>,
    pub choice_labels: Vec<Vec<String>>,
    pub tx: Sender<PortalResponse<AccessDialogResult>>,
    pub access_id: window::Id,
}

impl AccessDialogArgs {
    pub(crate) fn get_surface(&mut self) -> cosmic::Task<Msg> {
        if self.options.modal.unwrap_or_default() {
            // create a modal surface
            let (id, task) = window::open(window::Settings {
                resizable: false,
                ..Default::default()
            });
            self.access_id = id;
            task.map(|_| Msg::Ignore)
        } else {
            // create a layer surface
            self.access_id = window::Id::unique();
            get_layer_surface(SctkLayerSurfaceSettings {
                id: self.access_id,
                layer: cosmic_client_toolkit::sctk::shell::wlr_layer::Layer::Top,
                keyboard_interactivity:
                    cosmic_client_toolkit::sctk::shell::wlr_layer::KeyboardInteractivity::OnDemand,
                pointer_interactivity: true,
                anchor: cosmic_client_toolkit::sctk::shell::wlr_layer::Anchor::empty(),
                output: IcedOutput::Active,
                namespace: "access portal".to_string(),
                ..Default::default()
            })
        }
    }

    pub(crate) fn destroy_surface(&self) -> cosmic::Task<Msg> {
        if self.options.modal.unwrap_or_default() {
            window::close(self.access_id)
        } else {
            destroy_layer_surface(self.access_id)
        }
    }
}

pub(crate) fn view(portal: &CosmicPortal) -> cosmic::Element<'_, Msg> {
    let spacing = portal.core.system_theme().cosmic().spacing;
    let Some(args) = portal.access_args.as_ref() else {
        return text("Oops, no access dialog args").into();
    };

    let choices = &args.options.choices.as_deref().unwrap_or(&[]);
    let mut options = Vec::with_capacity(choices.len());
    for (i, ((id, label, choices, initial), choice_labels)) in
        choices.iter().zip(&args.choice_labels).enumerate()
    {
        let label = text(label);
        let active_choice = args
            .active_choices
            .get(id)
            .and_then(|choice_id| choices.iter().position(|(x, _)| x == choice_id));
        let dropdown = dropdown(choice_labels, active_choice, move |j| Msg::Choice(i, j));
        options.push(row![label, dropdown].into());
    }

    let options = Column::with_children(options)
        .spacing(spacing.space_xxs as f32) // space_l
        .align_x(Alignment::Center);

    let icon = icon::Icon::from(
        icon::from_name(
            args.options
                .icon
                .as_ref()
                .map_or("image-missing", |name| name.as_str()),
        )
        .size(64),
    );

    let control = column![text(args.body.as_str()), options].spacing(spacing.space_m as f32);

    let cancel_button = button::text(
        args.options
            .deny_label
            .clone()
            .unwrap_or_else(|| fl!("cancel")),
    )
    .on_press(Msg::Cancel);

    let allow_button = button::text(
        args.options
            .grant_label
            .clone()
            .unwrap_or_else(|| fl!("allow")),
    )
    .on_press(Msg::Allow)
    .class(cosmic::theme::Button::Suggested);

    let content = KeyboardWrapper::new(
        widget::dialog()
            .title(&args.title)
            .body(&args.subtitle)
            .control(control)
            .icon(icon)
            .secondary_action(cancel_button)
            .primary_action(allow_button),
        |key| match key {
            Key::Named(Named::Enter) => Some(Msg::Allow),
            Key::Named(Named::Escape) => Some(Msg::Cancel),
            _ => None,
        },
    );

    autosize(content, Id::new(args.app_id.clone()))
        .min_width(1.)
        .min_height(1.)
        .into()
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Task<crate::app::Msg> {
    match msg {
        Msg::Allow => {
            let args = portal.access_args.take().unwrap();
            let tx = args.tx.clone();
            let choices = args.active_choices.clone().into_iter().collect();
            tokio::spawn(async move {
                tx.send(PortalResponse::Success(AccessDialogResult { choices }))
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
            if let Some(choice) = args.options.choices.as_ref().and_then(|x| x.get(i))
                && let Some((option_id, _)) = choice.2.get(j) {
                    args.active_choices
                        .insert(choice.0.clone(), option_id.clone());
                }
            cosmic::iced::Task::none()
        }
        Msg::Ignore => cosmic::iced::Task::none(),
    }
    .map(crate::app::Msg::Access)
}
pub fn update_args(
    portal: &mut CosmicPortal,
    mut msg: AccessDialogArgs,
) -> cosmic::Task<crate::app::Msg> {
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
    cosmic::iced::Task::batch(cmds).map(crate::app::Msg::Access)
}
