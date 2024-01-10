#![allow(dead_code, unused_variables)]

use cosmic::iced::wayland::actions::layer_surface::{IcedOutput, SctkLayerSurfaceSettings};
use cosmic::iced::{window, Limits};
use cosmic::iced_core::Length;
use cosmic::iced_sctk::commands::layer_surface::{destroy_layer_surface, get_layer_surface};
use cosmic::iced_widget::graphics::text::cosmic_text::rustybuzz::ttf_parser::name;
use cosmic::widget::divider::horizontal;
use cosmic::widget::horizontal_space;
use cosmic_client_toolkit::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use image::RgbaImage;
use std::sync::Arc;
use std::{collections::HashMap, path::PathBuf};
use tokio::sync::mpsc::Sender;
use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_surface::WlSurface;
use zbus::zvariant;

use crate::app::{CosmicPortal, OutputState};
use crate::wayland::WaylandHelper;
use crate::{subscription, PortalResponse};

// TODO save to /run/user/$UID/doc/ with document portal fuse filesystem?

#[derive(zvariant::DeserializeDict, zvariant::Type, Clone, Debug)]
#[zvariant(signature = "a{sv}")]
pub struct ScreenshotOptions {
    modal: Option<bool>,
    interactive: Option<bool>,
    /// Custom value allowing the client to request the screenshot destination to be chosen.
    ///
    /// Defaults to false
    choose_destination: Option<bool>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
pub struct ScreenshotResult {
    uri: String,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct PickColorResult {
    color: (f64, f64, f64), // (ddd)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

pub struct Screenshot {
    wayland_helper: WaylandHelper,
    tx: Sender<subscription::Event>,
}

impl Screenshot {
    pub fn new(wayland_helper: WaylandHelper, tx: Sender<subscription::Event>) -> Self {
        Self { wayland_helper, tx }
    }

    async fn interactive_screenshot_inner(
        &self,
        outputs: Vec<(wl_output::WlOutput, (i32, i32), String)>,
        app_id: &str,
    ) -> anyhow::Result<HashMap<String, Arc<RgbaImage>>> {
        // collect screenshots from each output

        let wayland_helper = self.wayland_helper.clone();

        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let mut map = HashMap::with_capacity(outputs.len());
            for (output, _, name) in outputs {
                let frame = wayland_helper
                    .capture_output_shm(&output, false)
                    .ok_or_else(|| anyhow::anyhow!("shm screencopy failed"))?;
                map.insert(name, Arc::new(frame.image()?));
            }

            Ok(map)
        })
        .await?
    }

    async fn screenshot_inner(
        &self,
        outputs: Vec<(wl_output::WlOutput, (i32, i32), String)>,
        app_id: &str,
    ) -> anyhow::Result<PathBuf> {
        use ashpd::documents::Permission;

        let wayland_helper = self.wayland_helper.clone();
        let (file, path) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let mut bounds_opt: Option<Rect> = None;
            let mut frames = Vec::with_capacity(outputs.len());
            for (output, (output_x, output_y), _) in outputs {
                let frame = wayland_helper
                    .capture_output_shm(&output, false)
                    .ok_or_else(|| anyhow::anyhow!("shm screencopy failed"))?;
                let rect = Rect {
                    left: output_x,
                    top: output_y,
                    right: output_x.saturating_add(frame.width.try_into().unwrap_or_default()),
                    bottom: output_y.saturating_add(frame.height.try_into().unwrap_or_default()),
                };
                bounds_opt = Some(match bounds_opt.take() {
                    Some(bounds) => Rect {
                        left: bounds.left.min(rect.left),
                        top: bounds.top.min(rect.top),
                        right: bounds.right.max(rect.right),
                        bottom: bounds.bottom.max(rect.bottom),
                    },
                    None => rect,
                });
                frames.push((frame, rect));
            }

            let bounds = bounds_opt.unwrap_or_default();
            let width = bounds
                .right
                .saturating_sub(bounds.left)
                .try_into()
                .unwrap_or_default();
            let height = bounds
                .bottom
                .saturating_sub(bounds.top)
                .try_into()
                .unwrap_or_default();
            let mut image = image::RgbaImage::new(width, height);
            for (frame, rect) in frames {
                let frame_image = frame.image()?;
                image::imageops::overlay(
                    &mut image,
                    &frame_image,
                    rect.left.into(),
                    rect.top.into(),
                );
            }

            let mut file = tempfile::Builder::new()
                .prefix("screenshot-")
                .suffix(".png")
                .tempfile()?;
            {
                let mut encoder = png::Encoder::new(&mut file, image.width(), image.height());
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header()?;
                writer.write_image_data(image.as_raw())?;
            }
            Ok(file.keep()?)
        })
        .await??;

        let documents = ashpd::documents::Documents::new().await?;
        let mount_point = documents.mount_point().await?;
        let app_id = if app_id.is_empty() {
            None
        } else {
            Some(app_id.try_into()?)
        };
        let (doc_ids, _) = documents
            .add_full(
                &[&file],
                Default::default(),
                app_id,
                &[
                    Permission::Read,
                    Permission::Write,
                    Permission::GrantPermissions,
                    Permission::Delete,
                ],
            )
            .await?;
        let doc_id = doc_ids.get(0).unwrap();

        let mut doc_path = mount_point.as_ref().to_path_buf();
        doc_path.push(&**doc_id);
        doc_path.push(path.file_name().unwrap());

        Ok(doc_path)
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Capture,
    Cancel,
    Choice(Choice),
    OutputChanged(WlOutput),
}

#[derive(Debug, Clone)]
pub enum Choice {
    Output(String),
    Rectangle(Rect),
    Window(WlSurface),
}

#[derive(Debug, Clone, Default)]
pub enum Action {
    #[default]
    ReturnPath,
    SaveToClipboard,
    SaveToPictures,
    SaveToDocuments,
    ChooseFolder, // TODO use document portal to choose folder
    Choice(Choice),
}

#[derive(Clone)]
pub struct Args {
    pub handle: zvariant::ObjectPath<'static>,
    pub app_id: String,
    pub parent_window: String,
    pub options: ScreenshotOptions,
    pub images: HashMap<String, Arc<RgbaImage>>,
    pub window_imgs: HashMap<String, PathBuf>,
    pub tx: Sender<PortalResponse<ScreenshotResult>>,
    pub choice: Choice,
    pub action: Action,
}

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl Screenshot {
    async fn screenshot(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        options: ScreenshotOptions,
    ) -> PortalResponse<ScreenshotResult> {
        // connection.object_server().at(&handle, Request);

        // TODO create handle, show dialog
        eprintln!("screenshot Request");
        let mut outputs = Vec::new();
        for output in self.wayland_helper.outputs() {
            let Some(info) = self.wayland_helper.output_info(&output) else {
                log::warn!("Output {:?} has no info", output);
                continue;
            };
            let Some(name) = info.name.clone() else {
                log::warn!("Output {:?} has no name", output);
                continue;
            };
            let Some(pos) = info.logical_position else {
                log::warn!("Output {:?} has no position", output);
                continue;
            };
            outputs.push((output, pos, name));
        }
        if outputs.is_empty() {
            log::error!("No output");
            return PortalResponse::Other;
        };

        // if interactive, send image to be used by screenshot editor & await response via channel
        if options.interactive.unwrap_or_default() {
            eprintln!("sending request to subscription");
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let first_output = outputs[0].2.clone();
            let images = self
                .interactive_screenshot_inner(outputs, app_id)
                .await
                .unwrap_or_default();
            if let Err(err) = self
                .tx
                .send(subscription::Event::Screenshot(Args {
                    handle: handle.to_owned(),
                    app_id: app_id.to_string(),
                    parent_window: parent_window.to_string(),
                    action: if options.choose_destination.unwrap_or_default() {
                        Action::SaveToClipboard
                    } else {
                        Action::ReturnPath
                    },
                    options,
                    images,
                    window_imgs: HashMap::new(),
                    tx,
                    // TODO get last choice
                    // Could maybe be stored using cosmic config state?
                    // TODO cover all outputs at start of rectangle?
                    choice: Choice::Output(first_output), // will be updated
                }))
                .await
            {
                log::error!("Failed to send screenshot event, {}", err);
                return PortalResponse::Other;
            }
            eprintln!("sent msg to subscription and awaiting response");
            if let Some(res) = rx.recv().await {
                return res;
            } else {
                return PortalResponse::Cancelled::<ScreenshotResult>;
            }
        }

        let doc_path = match self.screenshot_inner(outputs, app_id).await {
            Ok(res) => res,
            Err(err) => {
                log::error!("Failed to capture screenshot: {}", err);
                return PortalResponse::Other;
            }
        };

        // connection.object_server().remove::<Request, _>(&handle);
        PortalResponse::Success(ScreenshotResult {
            uri: format!("file:///{}", doc_path.display()),
        })
    }

    async fn pick_color(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        option: HashMap<String, zvariant::Value<'_>>,
    ) -> PortalResponse<PickColorResult> {
        // TODO create handle
        // XXX
        PortalResponse::Success(PickColorResult {
            color: (1., 1., 1.),
        })
    }

    #[dbus_interface(property)]
    fn version(&self) -> u32 {
        2
    }
}

pub(crate) fn view(portal: &CosmicPortal, id: window::Id) -> cosmic::Element<Msg> {
    let output = portal.outputs.iter().find(|o| o.id == id).unwrap();
    let Some(args) = portal.screenshot_args.as_ref() else {
        return horizontal_space(Length::Fixed(1.0)).into();
    };
    let name = output.info.name.clone().unwrap_or_default();

    let raw_image = args.images.get(&name).unwrap();
    crate::widget::screenshot::ScreenshotSelection::new(
        args.choice.clone(),
        raw_image.clone(),
        Msg::Capture,
        Msg::Cancel,
        output.output.clone(),
        Msg::OutputChanged,
    )
    .into()
}
pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Command<crate::app::Msg> {
    match msg {
        Msg::Capture => {
            eprintln!("Capturing screenshot");
            let cmds = portal.outputs.iter().map(|o| destroy_layer_surface(o.id));
            let Some(args) = portal.screenshot_args.take() else {
                return cosmic::Command::batch(cmds);
            };
            // TODO process screenshot using choice
            tokio::spawn(async move {
                let Args {
                    tx,
                    choice,
                    mut images,
                    ..
                } = args;
                // TODO process based on choice

                // TODO cleanup
                let image_path = tempfile::Builder::new()
                    .prefix("screenshot-")
                    .suffix(".png")
                    .tempfile()
                    .unwrap()
                    .into_temp_path();

                let mut success = true;
                match choice {
                    Choice::Output(name) => {
                        eprintln!("name: {}", name);
                        if let Some(img) = images.remove(&name) {
                            let mut encoder = png::Encoder::new(
                                std::fs::File::create(&image_path).unwrap(),
                                img.width(),
                                img.height(),
                            );
                            encoder.set_color(png::ColorType::Rgba);
                            encoder.set_depth(png::BitDepth::Eight);
                            if let Ok(mut writer) = encoder.write_header() {
                                if let Err(err) = writer.write_image_data(img.as_raw()) {
                                    log::error!("Failed to write screenshot: {}", err);
                                    success = false;
                                };
                            } else {
                                log::error!("Failed to write screenshot");
                                success = false;
                            };
                        } else {
                            log::error!("Failed to find screenshot");
                            success = false;
                        }
                    }
                    Choice::Rectangle(_) => todo!(),
                    Choice::Window(_) => todo!(),
                }

                let (success, image_path) = if success {
                    let image_path = image_path.keep();
                    (image_path.is_ok(), image_path.unwrap_or_default())
                } else {
                    (false, PathBuf::default())
                };

                let response = if success {
                    PortalResponse::Success(ScreenshotResult {
                        uri: format!("file:///{}", image_path.display()),
                    })
                } else {
                    PortalResponse::Other
                };

                if let Err(err) = tx.send(response).await {
                    log::error!("Failed to send screenshot event, {}", err);
                }
            });
            cosmic::Command::batch(cmds)
        }
        Msg::Cancel => {
            eprintln!("Canceling screenshot");
            let cmds = portal.outputs.iter().map(|o| destroy_layer_surface(o.id));
            let Some(args) = portal.screenshot_args.take() else {
                return cosmic::Command::batch(cmds);
            };
            tokio::spawn(async move {
                let Args { tx, .. } = args;
                if let Err(err) = tx.send(PortalResponse::Cancelled).await {
                    log::error!("Failed to send screenshot event, {}", err);
                }
            });
            cosmic::Command::batch(cmds)
        }
        Msg::Choice(c) => {
            if let Some(args) = portal.screenshot_args.as_mut() {
                args.choice = c;
                if let Choice::Rectangle(r) = &args.choice {
                    portal.prev_rectangle = Some(*r);
                }
            }
            cosmic::Command::none()
        }
        Msg::OutputChanged(wl_output) => {
            if let (Some(args), Some(o)) = (
                portal.screenshot_args.as_mut(),
                portal
                    .outputs
                    .iter()
                    .find(|o| o.output == wl_output)
                    .and_then(|o| o.info.name.clone()),
            ) {
                args.choice = Choice::Output(o);
            }
            portal.active_output = Some(wl_output);
            cosmic::Command::none()
        }
    }
}

pub fn update_args(portal: &mut CosmicPortal, msg: Args) -> cosmic::Command<crate::app::Msg> {
    match msg {
        args => {
            let Args {
                handle,
                app_id,
                parent_window,
                options,
                images,
                window_imgs,
                tx,
                choice,
                action,
            } = &args;
            // iterate over outputs and create a layer surface for each
            let cmds: Vec<_> = portal
                .outputs
                .iter()
                .map(
                    |OutputState {
                         output, id, info, ..
                     }| {
                        let logical_size = info.logical_size.unwrap_or_else(|| {
                            log::warn!(
                                "Output {} has no logical size",
                                info.name.clone().unwrap_or_default()
                            );
                            (1920, 1080)
                        });
                        get_layer_surface(SctkLayerSurfaceSettings {
                            id: *id,
                            layer: Layer::Overlay,
                            keyboard_interactivity: KeyboardInteractivity::OnDemand,
                            pointer_interactivity: true,
                            anchor: Anchor::all(),
                            output: IcedOutput::Output(output.clone()),
                            namespace: "screenshot".to_string(),
                            size: Some((None, None)),
                            exclusive_zone: -1,
                            size_limits: Limits::NONE.min_height(1.0).min_width(1.0),
                            ..Default::default()
                        })
                    },
                )
                .collect();
            portal.screenshot_args = Some(args);
            eprintln!("sending commands for layer surfaces");
            cosmic::Command::batch(cmds)
        }
    }
}
