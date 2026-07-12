use cosmic::iced::keyboard::Key;
use cosmic::iced::keyboard::key::Named;
use cosmic::iced::platform_specific::shell::commands::layer_surface::destroy_layer_surface;
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::SctkLayerSurfaceSettings;
use cosmic::iced::window::{self, Id};
use cosmic::widget::{self, autosize};
use cosmic::{Element, Task};
use cosmic_client_toolkit::sctk::shell::wlr_layer;

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock};
use tokio::sync::{Mutex, mpsc::Sender};
use zbus::zvariant;

use crate::PortalResponse;
use crate::app::CosmicPortal;
use crate::print_dialog::{PrintDialog, apply_xdg_hints, build_xdg_response, sync_print_models};
use crate::subscription;
use crate::widget::keyboard_wrapper::KeyboardWrapper;

pub static PRINT_ID: LazyLock<window::Id> = LazyLock::new(window::Id::unique);
pub static PRINT_WIDGET_ID: LazyLock<widget::Id> =
    LazyLock::new(|| widget::Id::new("print-dialog"));

#[derive(Clone, Debug)]
pub struct CachedPrintSettings {
    pub app_id: String,
    pub printed_id: String,
    pub backend: String,
    pub cpdb_settings: Vec<(String, String)>,
}

type TokenMap = Arc<Mutex<HashMap<u32, CachedPrintSettings>>>;

pub struct Print {
    tx: Sender<subscription::Event>,
    next_token: Arc<AtomicU32>,
    tokens: TokenMap,
}

impl Print {
    pub fn new(tx: Sender<subscription::Event>) -> Self {
        Self {
            tx,
            next_token: Arc::new(AtomicU32::new(1)),
            tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    Dialog(crate::print_dialog::Msg),
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
        #[zbus(connection)] connection: &zbus::Connection,
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

        let tx = self.tx.clone();
        let handle_clone = handle.to_owned();
        let on_cancel = move || {
            let tx = tx.clone();
            let handle_clone = handle_clone.clone();
            async move {
                let _ = tx
                    .send(subscription::Event::CancelPrint(handle_clone))
                    .await;
            }
        };

        crate::Request::run(connection, &handle, on_cancel, async {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let modal = options.modal.unwrap_or(true);

            if let Err(err) = self
                .tx
                .send(subscription::Event::Print(Box::new(PrintArgs {
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
                    token_counter: Arc::clone(&self.next_token),
                    token_map: Arc::clone(&self.tokens),
                })))
                .await
            {
                log::error!("Failed to send print portal request: {err}");
                return PortalResponse::Other;
            }

            rx.recv().await.unwrap_or(PortalResponse::Cancelled)
        })
        .await
    }

    // TODO: need to add save-to-file flow for print call
    #[allow(clippy::too_many_arguments)]
    async fn print(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        fd: zvariant::Fd<'_>,
        options: PrintOptions,
    ) -> PortalResponse<PrintResult> {
        log::debug!(
            "Print: app_id={app_id} parent_window={parent_window} \
            title={title} options={options:?} fd={fd:?}"
        );

        let tx = self.tx.clone();
        let handle_clone = handle.to_owned();
        let on_cancel = move || {
            let tx = tx.clone();
            let handle_clone = handle_clone.clone();
            async move {
                let _ = tx
                    .send(subscription::Event::CancelPrint(handle_clone))
                    .await;
            }
        };

        crate::Request::run(connection, &handle, on_cancel, async {
            if let Some(token) = options.token {
                let mut map = self.tokens.lock().await;
                if let Some(saved) = map.get(&token).filter(|s| s.app_id == app_id) {
                    let saved = saved.clone();
                    map.remove(&token);
                    drop(map);

                    let owned_fd = match fd.as_fd().try_clone_to_owned() {
                        Ok(f) => f,
                        Err(e) => {
                            log::error!("Failed to clone file descriptor: {e}");
                            return PortalResponse::Other;
                        }
                    };

                    return crate::print_dialog::do_print_execution(
                        saved.printed_id,
                        saved.backend,
                        saved.cpdb_settings,
                        title.to_string(),
                        Arc::new(owned_fd),
                    )
                    .await;
                };
            }

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
                .send(subscription::Event::Print(Box::new(PrintArgs {
                    handle: handle.to_owned(),
                    app_id: app_id.to_string(),
                    title: title.to_string(),
                    modal,
                    window_id: Id::NONE,
                    dialog: PrintDialog::default(),
                    request: RequestKind::Print(PrintRequest {
                        fd: Arc::new(owned_fd),
                        tx,
                    }),
                    token_counter: Arc::clone(&self.next_token),
                    token_map: Arc::clone(&self.tokens),
                })))
                .await
            {
                log::error!("Failed to send print portal request: {err}");
                return PortalResponse::Other;
            }

            rx.recv().await.unwrap_or(PortalResponse::Cancelled)
        })
        .await
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
    pub token_counter: Arc<AtomicU32>,
    pub token_map: TokenMap,
}

fn create_dialog(modal: bool) -> (window::Id, cosmic::Task<cosmic::Action<crate::app::Msg>>) {
    if modal {
        let (id, task) = window::open(window::Settings {
            resizable: false,
            ..Default::default()
        });
        (id, task.discard())
    } else {
        let id = *PRINT_ID;

        let task = cosmic::surface::surface_task::<crate::app::Msg>(
            cosmic::surface::action::simple_layer_shell::<crate::app::Msg>(
                Default::default,
                move || SctkLayerSurfaceSettings {
                    id,
                    keyboard_interactivity: wlr_layer::KeyboardInteractivity::Exclusive,
                    namespace: "print".into(),
                    layer: wlr_layer::Layer::Top,
                    size: None,
                    ..Default::default()
                },
                None::<fn() -> cosmic::Element<'static, cosmic::Action<crate::app::Msg>>>,
            ),
        );
        (id, task)
    }
}

pub fn update_args(
    portal: &mut CosmicPortal,
    mut args: PrintArgs,
) -> Task<cosmic::Action<crate::app::Msg>> {
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
        apply_xdg_hints(
            &mut args.dialog,
            &req.settings,
            &req.page_setup,
            req.options.accept_label.clone(),
        );
    }

    args.window_id = window_id;
    portal.print_args = Some(args);
    sync_print_models(portal);

    command
}

pub fn cancel(
    portal: &mut CosmicPortal,
    handle: zvariant::ObjectPath<'static>,
) -> Task<cosmic::Action<crate::app::Msg>> {
    if let Some(args) = &portal.print_args
        && args.handle == handle
    {
        let args = portal.print_args.take().unwrap();
        let window_id = args.window_id;
        let modal = args.modal;
        tokio::spawn(async move {
            args.request.send_cancel_response().await;
        });
        if modal {
            return window::close(window_id);
        } else {
            return destroy_layer_surface(window_id);
        }
    }
    Task::none()
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> Task<cosmic::Action<crate::app::Msg>> {
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
                    return task;
                }
                Task::none()
            }
            crate::print_dialog::Msg::Confirm => {
                if let Some(args) = portal.print_args.take() {
                    let window_id = args.window_id;
                    let modal = args.modal;

                    // Build response maps using XDG portal key names
                    let (settings, page_setup) = build_xdg_response(&args.dialog);

                    let selected_printer = args
                        .dialog
                        .selected_printer_index
                        .and_then(|idx| args.dialog.printers.get(idx));
                    let printer_id = selected_printer.map(|p| p.id.clone()).unwrap_or_default();
                    let print_backend = selected_printer
                        .map(|p| p.backend.clone())
                        .unwrap_or_default();

                    match args.request {
                        RequestKind::PreparePrint(req) => {
                            let token = args.token_counter.fetch_add(1, Ordering::Relaxed);
                            let map = Arc::clone(&args.token_map);
                            let app_id = args.app_id.clone();
                            let cpdb_settings =
                                crate::print_dialog::build_cpdb_settings(&args.dialog);

                            tokio::spawn(async move {
                                let mut lock = map.lock().await;
                                lock.insert(
                                    token,
                                    CachedPrintSettings {
                                        app_id,
                                        printed_id: printer_id,
                                        backend: print_backend,
                                        cpdb_settings,
                                    },
                                );
                                // cleanup the cached settings after 5 mins if still unused
                                drop(lock);

                                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                                map.lock().await.remove(&token);
                            });

                            let result = PreparePrintResult {
                                settings,
                                page_setup,
                                token,
                            };
                            tokio::spawn(async move {
                                let _ = req.tx.send(PortalResponse::Success(result)).await;
                            });
                        }
                        RequestKind::Print(req) => {
                            let cpdb_settings =
                                crate::print_dialog::build_cpdb_settings(&args.dialog);
                            let title = args.title.clone();
                            tokio::spawn(async move {
                                let result = crate::print_dialog::do_print_execution(
                                    printer_id,
                                    print_backend,
                                    cpdb_settings,
                                    title,
                                    req.fd,
                                )
                                .await;
                                let _ = req.tx.send(result).await;
                            });
                        }
                    }

                    let task = if modal {
                        window::close(window_id)
                    } else {
                        destroy_layer_surface(window_id)
                    };
                    return task;
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
                cmd.map(|m| cosmic::Action::App(crate::app::Msg::Print(Msg::Dialog(m))))
            }
        },
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
