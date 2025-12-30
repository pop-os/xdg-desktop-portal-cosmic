//! Implementation of the org.freedesktop.impl.portal.Background interface
//! 
//! This portal allows applications to request permission to run in the background
//! and optionally to be started automatically at login.

use cosmic::iced_runtime::platform_specific::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced_winit::commands::layer_surface::{destroy_layer_surface, get_layer_surface};
use cosmic::widget::autosize::autosize;
use cosmic::widget::{self, Column, Id, button, icon, text};
use cosmic::{
    iced::{
        keyboard::{Key, key::Named},
        window,
    },
    iced_core::Alignment,
};
use std::collections::HashMap;
use tokio::sync::mpsc::Sender;
use zbus::zvariant::{self, OwnedValue};

use crate::wayland::WaylandHelper;
use crate::widget::keyboard_wrapper::KeyboardWrapper;
use crate::{PortalResponse, subscription};
use crate::{app::CosmicPortal, fl};

/// Options passed to RequestBackground
#[derive(zvariant::DeserializeDict, zvariant::Type, Debug, Clone, Default)]
#[zvariant(signature = "a{sv}")]
pub struct BackgroundOptions {
    /// A string that will be used as the last element of the handle
    pub handle_token: Option<String>,
    /// User-visible reason for the request
    pub reason: Option<String>,
    /// Whether the app also wants to be started automatically at login
    pub autostart: Option<bool>,
    /// Commandline to use when autostarting at login
    pub commandline: Option<Vec<String>>,
    /// Whether to use D-Bus activation for autostart
    #[zvariant(rename = "dbus-activatable")]
    pub dbus_activatable: Option<bool>,
}

/// Result returned from RequestBackground
#[derive(zvariant::SerializeDict, zvariant::Type, Debug, Clone)]
#[zvariant(signature = "a{sv}")]
pub struct BackgroundResult {
    /// Whether the application is allowed to run in the background
    pub background: bool,
    /// Whether the application will be autostarted (always false in this implementation)
    pub autostart: bool,
}

/// Options passed to SetStatus
#[derive(zvariant::DeserializeDict, zvariant::Type, Debug, Clone, Default)]
#[zvariant(signature = "a{sv}")]
pub struct StatusOptions {
    /// Status message for the application (max 96 characters)
    pub message: Option<String>,
}

/// The Background portal implementation
pub struct Background {
    #[allow(dead_code)]
    wayland_helper: WaylandHelper,
    tx: Sender<subscription::Event>,
}

impl Background {
    pub fn new(wayland_helper: WaylandHelper, tx: Sender<subscription::Event>) -> Self {
        Self { wayland_helper, tx }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Background")]
impl Background {
    /// RequestBackground method
    /// 
    /// Requests that the application is allowed to run in the background.
    async fn request_background(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        options: BackgroundOptions,
    ) -> PortalResponse<BackgroundResult> {
        log::debug!(
            "Background request from {app_id} (parent: {parent_window}), options: {options:?}"
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        // Send event to create background permission dialog
        if let Err(err) = self
            .tx
            .send(subscription::Event::Background(BackgroundDialogArgs {
                handle: handle.to_owned(),
                app_id: app_id.to_string(),
                parent_window: parent_window.to_string(),
                options,
                tx,
                background_id: window::Id::NONE,
            }))
            .await
        {
            log::error!("Failed to send background dialog event: {err}");
            return PortalResponse::Other;
        }

        // Wait for user response
        if let Some(res) = rx.recv().await {
            res
        } else {
            PortalResponse::Cancelled::<BackgroundResult>
        }
    }

    /// SetStatus method (added in version 2)
    /// 
    /// Sets the status of the application running in background.
    async fn set_status(&self, options: HashMap<String, OwnedValue>) {
        // Extract message from options if present
        if let Some(message) = options.get("message") {
            if let Ok(msg) = <&str>::try_from(message) {
                log::debug!("Background status set: {msg}");
                // TODO: In the future, this could be displayed in a system tray or notification
            }
        }
    }

    /// Version property
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

/// Message types for the background permission dialog
#[derive(Debug, Clone)]
pub enum Msg {
    /// User allowed background activity
    Allow,
    /// User denied background activity  
    Cancel,
    /// Ignore (used for window events)
    Ignore,
}

/// Arguments for creating a background permission dialog
#[derive(Clone, Debug)]
pub struct BackgroundDialogArgs {
    /// D-Bus object path handle
    pub handle: zvariant::ObjectPath<'static>,
    /// Application ID requesting permission
    pub app_id: String,
    /// Parent window identifier
    pub parent_window: String,
    /// Request options
    pub options: BackgroundOptions,
    /// Channel to send response back to D-Bus handler
    pub tx: Sender<PortalResponse<BackgroundResult>>,
    /// Window ID for the dialog
    pub background_id: window::Id,
}

impl BackgroundDialogArgs {
    /// Create the dialog surface
    pub fn get_surface(&mut self) -> cosmic::Task<Msg> {
        // Create a layer surface for the dialog
        self.background_id = window::Id::unique();
        get_layer_surface(SctkLayerSurfaceSettings {
            id: self.background_id,
            layer: cosmic_client_toolkit::sctk::shell::wlr_layer::Layer::Top,
            keyboard_interactivity:
                cosmic_client_toolkit::sctk::shell::wlr_layer::KeyboardInteractivity::Exclusive,
            input_zone: None,
            anchor: cosmic_client_toolkit::sctk::shell::wlr_layer::Anchor::empty(),
            output: IcedOutput::Active,
            namespace: "background portal".to_string(),
            ..Default::default()
        })
    }

    /// Destroy the dialog surface
    pub fn destroy_surface(&self) -> cosmic::Task<Msg> {
        destroy_layer_surface(self.background_id)
    }
}

/// Render the background permission dialog
pub fn view(portal: &CosmicPortal) -> cosmic::Element<'_, Msg> {
    let spacing = portal.core.system_theme().cosmic().spacing;
    let Some(args) = portal.background_args.as_ref() else {
        return text("Oops, no background dialog args").into();
    };

    // Build the dialog content
    let app_name = if args.app_id.is_empty() {
        fl!("unknown-application")
    } else {
        args.app_id.clone()
    };

    let mut content_items: Vec<cosmic::Element<'_, Msg>> = Vec::new();

    // Main description
    content_items.push(
        text(fl!("background-permission-body", app_id = app_name.clone())).into(),
    );

    // Show reason if provided
    if let Some(reason) = &args.options.reason {
        if !reason.is_empty() {
            content_items.push(
                text(fl!("background-permission-reason", reason = reason.clone()))
                    .size(14)
                    .into(),
            );
        }
    }

    // Note about autostart limitation
    if args.options.autostart.unwrap_or(false) {
        content_items.push(
            text("Note: Automatic startup at login is not yet supported.")
                .size(12)
                .into(),
        );
    }

    let control = Column::with_children(content_items)
        .spacing(spacing.space_xs as f32)
        .align_x(Alignment::Start);

    let icon = icon::Icon::from(icon::from_name("application-x-executable").size(64));

    let cancel_button = button::text(fl!("cancel")).on_press(Msg::Cancel);

    let allow_button = button::text(fl!("allow"))
        .on_press(Msg::Allow)
        .class(cosmic::theme::Button::Suggested);

    let content = KeyboardWrapper::new(
        widget::dialog()
            .title(fl!("background-permission-title"))
            .body(fl!("background-permission-subtitle"))
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

/// Handle messages from the background dialog
pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Task<crate::app::Msg> {
    match msg {
        Msg::Allow => {
            let args = portal.background_args.take().unwrap();
            let tx = args.tx.clone();
            
            // Grant background permission, but autostart is not implemented
            let result = BackgroundResult {
                background: true,
                autostart: false, // TODO: Implement autostart support
            };
            
            tokio::spawn(async move {
                let _ = tx.send(PortalResponse::Success(result)).await;
            });

            args.destroy_surface()
        }
        Msg::Cancel => {
            let args = portal.background_args.take().unwrap();
            let tx = args.tx.clone();
            
            tokio::spawn(async move {
                let _ = tx.send(PortalResponse::Cancelled::<BackgroundResult>).await;
            });

            args.destroy_surface()
        }
        Msg::Ignore => cosmic::iced::Task::none(),
    }
    .map(crate::app::Msg::Background)
}

/// Handle new background dialog arguments
pub fn update_args(
    portal: &mut CosmicPortal,
    mut args: BackgroundDialogArgs,
) -> cosmic::Task<crate::app::Msg> {
    let mut cmds = Vec::with_capacity(2);

    // If there's an existing dialog, close it first
    if let Some(old_args) = portal.background_args.take() {
        cmds.push(old_args.destroy_surface());
        // Send cancelled response to the old request
        tokio::spawn(async move {
            let _ = old_args
                .tx
                .send(PortalResponse::Cancelled::<BackgroundResult>)
                .await;
        });
    }

    // Create the new dialog surface
    cmds.push(args.get_surface());
    portal.background_args = Some(args);
    
    cosmic::iced::Task::batch(cmds).map(crate::app::Msg::Background)
}
