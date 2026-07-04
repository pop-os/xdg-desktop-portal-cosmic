use cosmic::iced::keyboard::Key;
use cosmic::iced::keyboard::key::Named;
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface,
};
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced::window::{self, Id};
use cosmic::widget::{self, autosize};
use cosmic::{Element, Task};
use cosmic_client_toolkit::sctk::shell::wlr_layer;

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, LazyLock};
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::PortalResponse;
use crate::app::CosmicPortal;
use crate::print_dialog::{PrintDialog, apply_xdg_hints, build_xdg_response, sync_print_models};
use crate::subscription;
use crate::widget::keyboard_wrapper::KeyboardWrapper;

pub static PRINT_ID: LazyLock<window::Id> = LazyLock::new(window::Id::unique);
pub static PRINT_WIDGET_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("print-dialog"));

pub struct Print {
    tx: Sender<subscription::Event>,
}

impl Print {
    pub fn new(tx: Sender<subscription::Event>) -> Self {
        Self { tx }
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    Dialog(crate::print_dialog::Msg),
    Ignore,
}

/// Portal response type for PreparePrint
#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
pub struct PreparePrintResult {
    pub settings: HashMap<String, zvariant::OwnedValue>,
    #[zvariant(rename = "page-setup")]
    pub page_setup: HashMap<String, zvariant::OwnedValue>,
    pub token: u32,
}

/// Portal response type for Print
#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
pub struct PrintResult {
    pub settings: HashMap<String, zvariant::OwnedValue>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Print")]
impl Print {
    #[allow(clippy::too_many_arguments)]
    async fn prepare_print(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        settings: HashMap<String, zvariant::OwnedValue>,
        page_setup: HashMap<String, zvariant::OwnedValue>,
        options: PreparePrintOptions,
    ) -> PortalResponse<PreparePrintResult> {
        log::debug!(
            "PreparePrint: app_id={app_id} parent_window={parent_window} title={title} \
            settings={settings:?} page_setup={page_setup:?} options={options:?}"
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let modal = options.modal.unwrap_or(true);

        if let Err(err) = self
            .tx
            .send(subscription::Event::Print(PrintArgs {
                handle: handle.to_owned(),
                app_id: app_id.to_string(),
                title: title.to_string(),
                modal,
                window_id: Id::NONE,
                dialog: PrintDialog::default(),
                request: RequestKind::PreparePrint(PreparePrintRequest {
                    settings,
                    page_setup,
                    options,
                    tx,
                }),
            }))
            .await
        {
            log::error!("Failed to send print portal request: {err}");
            return PortalResponse::Other;
        }

        rx.recv().await.unwrap_or(PortalResponse::Cancelled)
    }

    // TODO: need to add token verification, full print-flow and save-to-file flow.
    async fn print(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        fd: zvariant::Fd<'_>,
        options: PrintOptions,
    ) -> PortalResponse<PrintResult> {
        log::debug!(
            "PreparePrint: app_id={app_id} parent_window={parent_window} \
            title={title} options={options:?} fd={fd:?}"
        );

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let modal = options.modal.unwrap_or(true);

        let owned_fd = match fd.as_fd().try_clone_to_owned() {
            Ok(f) => f,
            Err(e) => {
                log::error!("Failed to clone file descriptor: {e}");
                return PortalResponse::Other;
            }
        };

        if let Err(err) = self
            .tx
            .send(subscription::Event::Print(PrintArgs {
                handle: handle.to_owned(),
                app_id: app_id.to_string(),
                title: title.to_string(),
                modal,
                window_id: Id::NONE,
                dialog: PrintDialog::default(),
                request: RequestKind::Print(PrintRequest {
                    fd: Arc::new(owned_fd),
                    options,
                    tx,
                }),
            }))
            .await
        {
            log::error!("Failed to send print portal request: {err}");
            return PortalResponse::Other;
        }

        rx.recv().await.unwrap_or(PortalResponse::Cancelled)
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

#[derive(Debug, Clone)]
pub enum RequestKind {
    PreparePrint(PreparePrintRequest),
    Print(PrintRequest),
}

#[derive(Debug, Clone)]
pub struct PreparePrintRequest {
    pub settings: HashMap<String, zvariant::OwnedValue>,
    pub page_setup: HashMap<String, zvariant::OwnedValue>,
    pub options: PreparePrintOptions,
    pub tx: Sender<PortalResponse<PreparePrintResult>>,
}

#[derive(Debug, Clone)]
pub struct PrintRequest {
    pub fd: Arc<OwnedFd>,
    pub options: PrintOptions,
    pub tx: Sender<PortalResponse<PrintResult>>,
}

#[derive(zvariant::DeserializeDict, zvariant::Type, Clone, Debug)]
#[zvariant(signature = "a{sv}")]
pub struct PreparePrintOptions {
    pub modal: Option<bool>,
    pub accept_label: Option<String>,
}

#[derive(zvariant::DeserializeDict, zvariant::Type, Clone, Debug)]
#[zvariant(signature = "a{sv}")]
pub struct PrintOptions {
    pub modal: Option<bool>,
    pub token: Option<u32>,
    pub supported_output_file_formats: Option<Vec<String>>,
}

impl RequestKind {
    pub async fn send_cancel_response(self) {
        match self {
            RequestKind::PreparePrint(req) => {
                let _ = req.tx.send(PortalResponse::Cancelled).await;
            }
            RequestKind::Print(req) => {
                let _ = req.tx.send(PortalResponse::Cancelled).await;
            }
        }
    }

    pub async fn send_accept_response(self, result: PreparePrintResult) {
        match self {
            RequestKind::PreparePrint(req) => {
                let _ = req.tx.send(PortalResponse::Success(result)).await;
            }
            RequestKind::Print(req) => {
                let _ = req
                    .tx
                    .send(PortalResponse::Success(PrintResult {
                        settings: HashMap::new(),
                    }))
                    .await;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct PrintArgs {
    pub handle: zvariant::ObjectPath<'static>,
    pub app_id: String,
    pub title: String,
    pub modal: bool,
    pub window_id: cosmic::iced::window::Id,
    pub dialog: PrintDialog,
    pub request: RequestKind,
}

fn create_dialog(modal: bool) -> (window::Id, cosmic::Task<Msg>) {
    if modal {
        let (id, task) = window::open(window::Settings {
            resizable: false,
            ..Default::default()
        });
        (id, task.map(|_| Msg::Ignore))
    } else {
        let id = *PRINT_ID;
        let task: cosmic::Task<()> = get_layer_surface(SctkLayerSurfaceSettings {
            id,
            keyboard_interactivity: wlr_layer::KeyboardInteractivity::Exclusive,
            namespace: "print".into(),
            layer: wlr_layer::Layer::Top,
            size: None,
            ..Default::default()
        });
        (id, task.map(|_| Msg::Ignore))
    }
}

pub fn update_args(portal: &mut CosmicPortal, mut args: PrintArgs) -> Task<crate::app::Msg> {
    // If dialog already open, cancel previous request
    let (window_id, command) = if let Some(prev) = portal.print_args.take() {
        let w_id = prev.window_id;
        tokio::spawn(async move {
            prev.request.send_cancel_response().await;
        });
        (w_id, Task::none())
    } else {
        create_dialog(args.modal)
    };

    // Pre-populate dialog state from app-provided XDG portal hints.
    if let RequestKind::PreparePrint(ref req) = args.request {
        apply_xdg_hints(&mut args.dialog, &req.settings, &req.page_setup);
    }

    args.window_id = window_id;
    portal.print_args = Some(args);
    sync_print_models(portal);

    command.map(crate::app::Msg::Print)
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> Task<crate::app::Msg> {
    let Some(args) = portal.print_args.as_mut() else {
        return Task::none();
    };
    match msg {
        Msg::Dialog(dialog_msg) => match dialog_msg {
            crate::print_dialog::Msg::EnterPressed => {
                if args.dialog.active_view == crate::print_dialog::ActiveView::PageSelection {
                    if matches!(
                        args.dialog.page_selection,
                        crate::print_dialog::PageSetSelection::Custom(_)
                    ) && !args.dialog.custom_range_valid
                    {
                        Task::none()
                    } else {
                        args.dialog.active_view = crate::print_dialog::ActiveView::Main;
                        Task::none()
                    }
                } else {
                    update_msg(portal, Msg::Dialog(crate::print_dialog::Msg::Confirm))
                }
            }
            crate::print_dialog::Msg::EscapePressed => {
                if args.dialog.active_view == crate::print_dialog::ActiveView::PageSelection {
                    if matches!(
                        args.dialog.page_selection,
                        crate::print_dialog::PageSetSelection::Custom(_)
                    ) && !args.dialog.custom_range_valid
                    {
                        Task::none()
                    } else {
                        args.dialog.active_view = crate::print_dialog::ActiveView::Main;
                        Task::none()
                    }
                } else {
                    update_msg(portal, Msg::Dialog(crate::print_dialog::Msg::Cancel))
                }
            }
            crate::print_dialog::Msg::Cancel => {
                if let Some(args) = portal.print_args.take() {
                    let window_id = args.window_id;
                    let modal = args.modal;
                    tokio::spawn(async move {
                        args.request.send_cancel_response().await;
                    });
                    let task = if modal {
                        window::close(window_id)
                    } else {
                        destroy_layer_surface(window_id)
                    };
                    return task.map(crate::app::Msg::Print);
                }
                Task::none()
            }
            crate::print_dialog::Msg::Confirm => {
                if let Some(args) = portal.print_args.take() {
                    let window_id = args.window_id;
                    let modal = args.modal;

                    // Build response maps using XDG portal key names
                    let (settings, page_setup) = build_xdg_response(&args.dialog);

                    let result = PreparePrintResult {
                        settings,
                        page_setup,
                        token: 1,
                    };

                    tokio::spawn(async move {
                        args.request.send_accept_response(result).await;
                    });
                    let task = if modal {
                        window::close(window_id)
                    } else {
                        destroy_layer_surface(window_id)
                    };
                    return task.map(crate::app::Msg::Print);
                }
                Task::none()
            }
            crate::print_dialog::Msg::PageSelectionModelActivated(entity) => {
                portal.print_page_selection_model.activate(entity);
                if let Some(selection) = portal
                    .print_page_selection_model
                    .active_data::<crate::print_dialog::PageSetSelection>()
                {
                    args.dialog.page_selection = selection.clone();
                }
                sync_print_models(portal);
                Task::none()
            }
            crate::print_dialog::Msg::ColorModelActivated(entity) => {
                portal.print_color_model.activate(entity);
                if let Some(&mode) = portal
                    .print_color_model
                    .active_data::<crate::print_dialog::ColorMode>()
                {
                    if mode == crate::print_dialog::ColorMode::Color && !args.dialog.color_supported
                    {
                        sync_print_models(portal);
                    } else {
                        args.dialog.color_mode = mode;
                    }
                }
                Task::none()
            }
            crate::print_dialog::Msg::OrientationModelActivated(entity) => {
                portal.print_orientation_model.activate(entity);
                if let Some(&orientation) = portal
                    .print_orientation_model
                    .active_data::<crate::print_dialog::Orientation>()
                {
                    args.dialog.orientation = orientation;
                }
                Task::none()
            }
            crate::print_dialog::Msg::LayoutDirectionModelActivated(entity) => {
                portal.print_layout_direction_model.activate(entity);
                if let Some(&layout_direction) = portal
                    .print_layout_direction_model
                    .active_data::<crate::print_dialog::LayoutDirection>(
                ) {
                    args.dialog.layout_direction = layout_direction;
                }
                Task::none()
            }
            other_msg => {
                let cmd = crate::print_dialog::update(&mut args.dialog, other_msg);
                sync_print_models(portal);
                cmd.map(|m| crate::app::Msg::Print(Msg::Dialog(m)))
            }
        },
        Msg::Ignore => Task::none(),
    }
}

pub fn view(portal: &CosmicPortal) -> Element<'_, Msg> {
    let Some(args) = portal.print_args.as_ref() else {
        return widget::text("No print dialog args").into();
    };

    let content = crate::print_dialog::view(
        &args.dialog,
        &portal.print_color_model,
        &portal.print_orientation_model,
        &portal.print_layout_direction_model,
        &portal.print_page_selection_model,
    )
    .map(Msg::Dialog);

    autosize::autosize(
        KeyboardWrapper::new(
            widget::dialog().title(&args.title).control(content),
            |key, _| match key {
                Key::Named(Named::Enter) => {
                    Some(Msg::Dialog(crate::print_dialog::Msg::EnterPressed))
                }
                Key::Named(Named::Escape) => {
                    Some(Msg::Dialog(crate::print_dialog::Msg::EscapePressed))
                }
                _ => None,
            },
        ),
        PRINT_WIDGET_ID.clone(),
    )
    .max_width(600.)
    .max_height(750.)
    .min_width(1.)
    .min_height(1.)
    .into()
}
