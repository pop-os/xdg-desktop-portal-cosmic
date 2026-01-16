#![allow(dead_code, unused_variables)]

use cosmic::cosmic_config::CosmicConfigEntry;
use cosmic::iced::clipboard::mime::AsMimeTypes;
use cosmic::iced::keyboard::{Key, key::Named};
use cosmic::iced::{Limits, window};
use cosmic::iced_core::Length;
use cosmic::iced_runtime::clipboard;
use cosmic::iced_runtime::platform_specific::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced_winit::commands::layer_surface::{destroy_layer_surface, get_layer_surface};
use cosmic::widget::horizontal_space;
use cosmic_client_toolkit::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use futures::stream::{FuturesUnordered, StreamExt};
use image::RgbaImage;
use rustix::fd::AsFd;
use std::borrow::Cow;
use std::num::NonZeroU32;
use std::{collections::HashMap, io, path::PathBuf};
use tokio::sync::mpsc::Sender;

use wayland_client::protocol::wl_output::WlOutput;
use zbus::zvariant;

use crate::app::{CosmicPortal, OutputState};
use crate::config::{self, screenshot::ImageSaveLocation};
use crate::wayland::{CaptureSource, ShmImage, WaylandHelper};
use crate::widget::{keyboard_wrapper::KeyboardWrapper, rectangle_selection::DragState};
use crate::{PortalResponse, fl, subscription};

#[derive(Clone, Debug)]
pub struct ScreenshotImage {
    pub rgba: RgbaImage,
    pub handle: cosmic::widget::image::Handle,
}

impl ScreenshotImage {
    fn new<T: AsFd>(img: ShmImage<T>) -> anyhow::Result<Self> {
        let rgba = img.image_transformed()?;
        let handle = cosmic::widget::image::Handle::from_rgba(
            rgba.width(),
            rgba.height(),
            rgba.clone().into_vec(),
        );
        Ok(Self { rgba, handle })
    }

    pub fn width(&self) -> u32 {
        self.rgba.width()
    }

    pub fn height(&self) -> u32 {
        self.rgba.height()
    }
}

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

struct ScreenshotBytes {
    bytes: Vec<u8>,
}

impl ScreenshotBytes {
    fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }
}

impl AsMimeTypes for ScreenshotBytes {
    fn available(&self) -> std::borrow::Cow<'static, [String]> {
        Cow::Owned(vec!["image/png".to_string()])
    }

    fn as_bytes(&self, mime_type: &str) -> Option<std::borrow::Cow<'static, [u8]>> {
        Some(Cow::Owned(self.bytes.clone()))
    }
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct PickColorResult {
    color: (f64, f64, f64), // (ddd)
}

/// Logical Size and Position of a rectangle
#[derive(Clone, Copy, Debug, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    fn intersect(&self, other: Rect) -> Option<Rect> {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right.min(other.right);
        let bottom = self.bottom.min(other.bottom);
        if left < right && top < bottom {
            Some(Rect {
                left,
                top,
                right,
                bottom,
            })
        } else {
            None
        }
    }

    fn translate(&self, x: i32, y: i32) -> Rect {
        Rect {
            left: self.left + x,
            top: self.top + y,
            right: self.right + x,
            bottom: self.bottom + y,
        }
    }

    fn width(&self) -> i32 {
        self.right - self.left
    }

    fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn dimensions(self) -> Option<RectDimension> {
        let width = NonZeroU32::new((self.width()).unsigned_abs())?;
        let height = NonZeroU32::new((self.height()).unsigned_abs())?;
        Some(RectDimension { width, height })
    }
}

#[derive(Clone, Copy)]
pub struct RectDimension {
    width: NonZeroU32,
    height: NonZeroU32,
}

pub struct Screenshot {
    wayland_helper: WaylandHelper,
    tx: Sender<subscription::Event>,
}

impl Screenshot {
    pub fn new(wayland_helper: WaylandHelper, tx: Sender<subscription::Event>) -> Self {
        Self { wayland_helper, tx }
    }

    async fn interactive_toplevel_images(
        &self,
        outputs: &[Output],
    ) -> anyhow::Result<HashMap<String, Vec<ScreenshotImage>>> {
        let wayland_helper = self.wayland_helper.clone();
        Ok(outputs
            .iter()
            .map(move |Output { output, name, .. }| {
                let wayland_helper = wayland_helper.clone();
                async move {
                    let frame = wayland_helper
                        .capture_output_toplevels_shm(output, false)
                        .filter_map(|img| async { ScreenshotImage::new(img).ok() })
                        .collect()
                        .await;
                    (name.clone(), frame)
                }
            })
            .collect::<FuturesUnordered<_>>()
            .collect::<HashMap<String, _>>()
            .await)
    }

    async fn interactive_output_images(
        &self,
        outputs: &[Output],
        app_id: &str,
    ) -> anyhow::Result<HashMap<String, ScreenshotImage>> {
        // collect screenshots from each output

        let wayland_helper = self.wayland_helper.clone();

        let mut map = HashMap::with_capacity(outputs.len());
        for Output {
            output,
            logical_position: (output_x, output_y),
            name,
            ..
        } in outputs
        {
            let frame = wayland_helper
                .capture_source_shm(CaptureSource::Output(output.clone()), false)
                .await
                .ok_or_else(|| anyhow::anyhow!("shm screencopy failed"))?;
            map.insert(name.clone(), ScreenshotImage::new(frame)?);
        }

        Ok(map)
    }

    pub fn save_rgba(img: &RgbaImage, path: &PathBuf) -> anyhow::Result<()> {
        let mut file = std::fs::File::create(path)?;
        Ok(write_png(&mut file, img)?)
    }

    pub fn save_rgba_to_buffer(img: &RgbaImage, buffer: &mut Vec<u8>) -> anyhow::Result<()> {
        Ok(write_png(buffer, img)?)
    }

    pub fn get_img_path(location: ImageSaveLocation) -> Option<PathBuf> {
        let mut path = match location {
            ImageSaveLocation::Pictures => {
                // First check for XDG_SCREENSHOTS_DIR environment variable
                std::env::var_os("XDG_SCREENSHOTS_DIR")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .or_else(|| {
                        // Fall back to XDG_PICTURES_DIR/Screenshots or ~/Pictures/Screenshots
                        dirs::picture_dir()
                            .or_else(|| dirs::home_dir().map(|h| h.join("Pictures")))
                            .map(|p| p.join("Screenshots"))
                    })
            }
            ImageSaveLocation::Documents => {
                dirs::document_dir().or_else(|| dirs::home_dir().map(|h| h.join("Documents")))
            }
            ImageSaveLocation::Clipboard => None,
        }?;

        // Ensure the directory exists
        if let Err(err) = std::fs::create_dir_all(&path) {
            log::error!("Failed to create screenshot directory {:?}: {}", path, err);
            return None;
        }

        let name = chrono::Local::now()
            .format("Screenshot_%Y-%m-%d_%H-%M-%S.png")
            .to_string();
        path.push(name);

        Some(path)
    }

    async fn screenshot_inner(&self, outputs: &[Output], app_id: &str) -> anyhow::Result<PathBuf> {
        let wayland_helper = self.wayland_helper.clone();

        let mut bounds_opt: Option<Rect> = None;
        let mut frames = Vec::with_capacity(outputs.len());
        for Output {
            output,
            logical_position: (output_x, output_y),
            logical_size: (output_w, output_h),
            ..
        } in outputs
        {
            let frame = wayland_helper
                .capture_source_shm(CaptureSource::Output(output.clone()), false)
                .await
                .ok_or_else(|| anyhow::anyhow!("shm screencopy failed"))?;
            let frame_image = frame.image_transformed()?;
            let rect = Rect {
                left: *output_x,
                top: *output_y,
                right: output_x.saturating_add(*output_w),
                bottom: output_y.saturating_add(*output_h),
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
            frames.push((frame_image, rect));
        }

        let (file, path) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let image = combined_image(bounds_opt.unwrap_or_default(), frames);

            let mut file = tempfile::Builder::new()
                .prefix("screenshot-")
                .suffix(".png")
                .tempfile()?;
            {
                write_png(&mut file, &image)?;
            }
            Ok(file.keep()?)
        })
        .await??;

        Ok(path)
    }
}

fn combined_image(bounds: Rect, frames: Vec<(RgbaImage, Rect)>) -> RgbaImage {
    // If we have only one image, crop without scaling
    if frames.len() == 1 {
        let (frame_image, rect) = &frames[0];

        // TODO Don't have explicit scale factor; how to ensure pixel perfect scaling?
        let width_scale = frame_image.width() as f64 / rect.width() as f64;
        let height_scale = frame_image.height() as f64 / rect.height() as f64;

        let width = (bounds.width() as f64 * width_scale).max(0.) as u32;
        let height = (bounds.height() as f64 * height_scale).max(0.) as u32;
        let x = ((bounds.left - rect.left) as f64 * width_scale).max(0.) as u32;
        let y = ((bounds.top - rect.top) as f64 * height_scale).max(0.) as u32;

        return image::imageops::crop_imm(frame_image, x, y, width, height).to_image();
    }

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
    for (mut frame_image, rect) in frames {
        let width = rect.width() as u32;
        let height = rect.height() as u32;
        if frame_image.dimensions() != (width, height) {
            frame_image = image::imageops::resize(
                &frame_image,
                width,
                height,
                image::imageops::FilterType::Lanczos3,
            );
        };
        let x = i64::from(rect.left) - i64::from(bounds.left);
        let y = i64::from(rect.top) - i64::from(bounds.top);
        image::imageops::overlay(&mut image, &frame_image, x, y);
    }
    image
}

fn write_png<W: io::Write>(w: W, image: &RgbaImage) -> Result<(), png::EncodingError> {
    let mut encoder = png::Encoder::new(w, image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(image.as_raw())
}

#[derive(Debug, Clone)]
pub enum Msg {
    Capture,
    Cancel,
    Choice(Choice),
    OutputChanged(WlOutput),
    WindowChosen(String, usize),
    Location(usize),
}

#[derive(Debug, Clone)]
pub enum Choice {
    Output(String),
    Rectangle(Rect, DragState),
    Window(String, Option<usize>),
}

impl From<&Choice> for config::screenshot::Choice {
    // Using a reference here to avoid requiring a temporary `Choice` that's only consumed
    fn from(value: &Choice) -> Self {
        match value {
            Choice::Window(..) => config::screenshot::Choice::Window,
            Choice::Rectangle(..) => config::screenshot::Choice::Rectangle,
            Choice::Output(output) => config::screenshot::Choice::Output(Some(output.clone())),
        }
    }
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

#[derive(Clone, Debug)]
pub struct Args {
    pub handle: zvariant::ObjectPath<'static>,
    pub app_id: String,
    pub parent_window: String,
    pub options: ScreenshotOptions,
    pub output_images: HashMap<String, ScreenshotImage>,
    pub toplevel_images: HashMap<String, Vec<ScreenshotImage>>,
    pub tx: Sender<PortalResponse<ScreenshotResult>>,
    pub choice: Choice,
    pub location: ImageSaveLocation,
    pub action: Action,
}

struct Output {
    output: WlOutput,
    logical_position: (i32, i32),
    logical_size: (i32, i32),
    name: String,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Screenshot")]
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

        // The screenshot handler is created when the portal is launched, but requests are
        // handled on demand. The handler does not store extra state such as a reference to the
        // portal. Storing a copy of the config is unideal because it would remain out of date.
        //
        // The most straightforward solution is to load the screenshot config here
        let config = config::Config::load().0.screenshot;

        // TODO create handle, show dialog
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
            let Some(logical_position) = info.logical_position else {
                log::warn!("Output {:?} has no position", output);
                continue;
            };
            let Some(logical_size) = info.logical_size else {
                log::warn!("Output {:?} has no size", output);
                continue;
            };
            outputs.push(Output {
                output,
                logical_position,
                logical_size,
                name,
            });
        }
        if outputs.is_empty() {
            log::error!("No output");
            return PortalResponse::Other;
        };

        // if interactive, send image to be used by screenshot editor & await response via channel
        if options.interactive.unwrap_or_default() {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let first_output = &*outputs[0].name;
            let output_images = self
                .interactive_output_images(&outputs, app_id)
                .await
                .unwrap_or_default();
            let toplevel_images = self
                .interactive_toplevel_images(&outputs)
                .await
                .unwrap_or_default();
            // TODO: Maybe replace config's Choice with Choice from this file
            let choice = match config.choice {
                config::screenshot::Choice::Output(Some(output))
                    if outputs.iter().any(|Output { name, .. }| output == *name) =>
                {
                    Choice::Output(output)
                }
                config::screenshot::Choice::Output(_) => Choice::Output(first_output.into()),
                config::screenshot::Choice::Rectangle => {
                    // Use saved rectangle from config if available
                    let rect = config
                        .last_rectangle
                        .map(|r| Rect {
                            left: r.left,
                            top: r.top,
                            right: r.right,
                            bottom: r.bottom,
                        })
                        .unwrap_or_default();
                    Choice::Rectangle(rect, DragState::default())
                }
                config::screenshot::Choice::Window => Choice::Window(first_output.into(), None),
            };
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
                    output_images,
                    toplevel_images,
                    tx,
                    location: config.save_location,
                    // TODO cover all outputs at start of rectangle?
                    choice,
                    // will be updated
                }))
                .await
            {
                log::error!("Failed to send screenshot event, {}", err);
                return PortalResponse::Other;
            }
            if let Some(res) = rx.recv().await {
                return res;
            } else {
                return PortalResponse::Cancelled::<ScreenshotResult>;
            }
        }

        let doc_path = match self.screenshot_inner(&outputs, app_id).await {
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
        // XXX implement
        PortalResponse::Other
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }
}

pub(crate) fn view(portal: &CosmicPortal, id: window::Id) -> cosmic::Element<'_, Msg> {
    let Some((i, output)) = portal.outputs.iter().enumerate().find(|(i, o)| o.id == id) else {
        return horizontal_space().width(Length::Fixed(1.0)).into();
    };
    let Some(args) = portal.screenshot_args.as_ref() else {
        return horizontal_space().width(Length::Fixed(1.0)).into();
    };

    let Some(img) = args.output_images.get(&output.name) else {
        return horizontal_space().width(Length::Fixed(1.0)).into();
    };
    let theme = portal.core.system_theme().cosmic();
    KeyboardWrapper::new(
        crate::widget::screenshot::ScreenshotSelection::new(
            args.choice.clone(),
            img,
            Msg::Capture,
            Msg::Cancel,
            output,
            id,
            Msg::OutputChanged,
            Msg::Choice,
            &args.toplevel_images,
            Msg::WindowChosen,
            &portal.location_options,
            args.location as usize,
            Msg::Location,
            theme.spacing,
            i as u128,
        ),
        |key| match key {
            Key::Named(Named::Enter) => Some(Msg::Capture),
            Key::Named(Named::Escape) => Some(Msg::Cancel),
            _ => None,
        },
    )
    .into()
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Task<crate::app::Msg> {
    match msg {
        Msg::Capture => {
            let mut cmds: Vec<cosmic::Task<crate::app::Msg>> = portal
                .outputs
                .iter()
                .map(|o| destroy_layer_surface(o.id))
                .collect();
            let Some(args) = portal.screenshot_args.take() else {
                log::error!("Failed to find screenshot Args for Capture message.");
                return cosmic::Task::batch(cmds);
            };
            let outputs = portal.outputs.clone();
            let Args {
                tx,
                choice,
                output_images: mut images,
                location,
                ..
            } = args;

            let mut success = true;
            let image_path = Screenshot::get_img_path(location);

            match choice {
                Choice::Output(name) => {
                    if let Some(img) = images.remove(&name) {
                        if let Some(ref image_path) = image_path {
                            if let Err(err) = Screenshot::save_rgba(&img.rgba, image_path) {
                                log::error!("Failed to capture screenshot: {:?}", err);
                            };
                        } else {
                            let mut buffer = Vec::new();
                            if let Err(e) = Screenshot::save_rgba_to_buffer(&img.rgba, &mut buffer)
                            {
                                log::error!("Failed to save screenshot to buffer: {:?}", e);
                                success = false;
                            } else {
                                cmds.push(clipboard::write_data(ScreenshotBytes::new(buffer)))
                            };
                        }
                    } else {
                        log::error!("Failed to find output {}", name);
                        success = false;
                    }
                }
                Choice::Rectangle(r, s) => {
                    if let Some(RectDimension { width, height }) = r.dimensions() {
                        // Construct Rgba image with size of rect
                        // then overlay the part of each image that intersects with the rect
                        //let mut img = RgbaImage::new(width.get(), height.get());

                        let frames = images
                            .into_iter()
                            .filter_map(|(name, raw_img)| {
                                let output = outputs.iter().find(|o| o.name == name)?;
                                let pos = output.logical_pos;
                                let output_rect = Rect {
                                    left: pos.0,
                                    top: pos.1,
                                    right: pos.0 + output.logical_size.0 as i32,
                                    bottom: pos.1 + output.logical_size.1 as i32,
                                };

                                let intersect = r.intersect(output_rect)?;

                                Some((raw_img.rgba, output_rect))
                            })
                            .collect::<Vec<_>>();
                        let img = combined_image(r, frames);

                        if let Some(ref image_path) = image_path {
                            if let Err(err) = Screenshot::save_rgba(&img, image_path) {
                                success = false;
                            }
                        } else {
                            let mut buffer = Vec::new();
                            if let Err(e) = Screenshot::save_rgba_to_buffer(&img, &mut buffer) {
                                log::error!("Failed to save screenshot to buffer: {:?}", e);
                                success = false;
                            } else {
                                cmds.push(clipboard::write_data(ScreenshotBytes::new(buffer)))
                            };
                        }
                    } else {
                        success = false;
                    }
                }
                Choice::Window(output, Some(window_i)) => {
                    if let Some(img) = args
                        .toplevel_images
                        .get(&output)
                        .and_then(|imgs| imgs.get(window_i))
                    {
                        if let Some(ref image_path) = image_path {
                            if let Err(err) = Screenshot::save_rgba(&img.rgba, image_path) {
                                log::error!("Failed to capture screenshot: {:?}", err);
                                success = false;
                            }
                        } else {
                            let mut buffer = Vec::new();
                            if let Err(e) = Screenshot::save_rgba_to_buffer(&img.rgba, &mut buffer)
                            {
                                log::error!("Failed to save screenshot to buffer: {:?}", e);
                                success = false;
                            } else {
                                cmds.push(clipboard::write_data(ScreenshotBytes::new(buffer)))
                            };
                        }
                    } else {
                        success = false;
                    }
                }
                _ => {
                    success = false;
                }
            }

            let response = if success && image_path.is_some() {
                PortalResponse::Success(ScreenshotResult {
                    uri: format!("file:///{}", image_path.unwrap().display()),
                })
            } else if success && image_path.is_none() {
                PortalResponse::Success(ScreenshotResult {
                    uri: "clipboard:///".to_string(),
                })
            } else {
                PortalResponse::Other
            };

            tokio::spawn(async move {
                if let Err(err) = tx.send(response).await {
                    log::error!("Failed to send screenshot event");
                }
            });
            cosmic::Task::batch(cmds)
        }
        Msg::Cancel => {
            let cmds = portal.outputs.iter().map(|o| destroy_layer_surface(o.id));
            let Some(args) = portal.screenshot_args.take() else {
                log::error!("Failed to find screenshot Args for Cancel message.");
                return cosmic::Task::batch(cmds);
            };
            let Args { tx, .. } = args;
            tokio::spawn(async move {
                if let Err(err) = tx.send(PortalResponse::Cancelled).await {
                    log::error!("Failed to send screenshot event");
                }
            });

            cosmic::Task::batch(cmds)
        }
        Msg::Choice(c) => {
            let choice = (&c).into();
            let last_rect = if let Choice::Rectangle(r, _) = &c {
                portal.prev_rectangle = Some(*r);
                Some(config::screenshot::Rect {
                    left: r.left,
                    top: r.top,
                    right: r.right,
                    bottom: r.bottom,
                })
            } else {
                portal.config.screenshot.last_rectangle
            };

            if let Some(args) = portal.screenshot_args.as_mut() {
                args.choice = c;
            } else {
                log::error!("Failed to find screenshot Args for Choice message.");
            }
            cosmic::task::message(crate::app::Msg::ConfigSetScreenshot(
                config::screenshot::Screenshot {
                    choice,
                    last_rectangle: last_rect,
                    ..portal.config.screenshot
                },
            ))
        }
        Msg::OutputChanged(wl_output) => {
            if let (Some(args), Some(o)) = (
                portal.screenshot_args.as_mut(),
                portal
                    .outputs
                    .iter()
                    .find(|o| o.output == wl_output)
                    .map(|o| o.name.clone()),
            ) {
                args.choice = Choice::Output(o);
            } else {
                log::error!(
                    "Failed to find output for OutputChange message: {:?}",
                    wl_output
                );
            }
            portal.active_output = Some(wl_output);
            cosmic::Task::none()
        }
        Msg::WindowChosen(name, i) => {
            if let Some(args) = portal.screenshot_args.as_mut() {
                args.choice = Choice::Window(name, Some(i));
            } else {
                log::error!("Failed to find screenshot Args for WindowChosen message.");
            }
            update_msg(portal, Msg::Capture)
        }
        Msg::Location(loc) => {
            if let Some(args) = portal.screenshot_args.as_mut() {
                let loc = match loc {
                    loc if loc == ImageSaveLocation::Clipboard as usize => {
                        ImageSaveLocation::Clipboard
                    }
                    loc if loc == ImageSaveLocation::Pictures as usize => {
                        ImageSaveLocation::Pictures
                    }
                    loc if loc == ImageSaveLocation::Documents as usize => {
                        ImageSaveLocation::Documents
                    }
                    _ => args.location,
                };
                args.location = loc;
                cosmic::task::message(crate::app::Msg::ConfigSetScreenshot(
                    config::screenshot::Screenshot {
                        save_location: loc,
                        choice: (&mut portal.config.screenshot.choice).into(),
                        last_rectangle: portal.config.screenshot.last_rectangle,
                    },
                ))
            } else {
                log::error!("Failed to find screenshot Args for Location message.");
                cosmic::Task::none()
            }
        }
    }
}

pub fn update_args(portal: &mut CosmicPortal, args: Args) -> cosmic::Task<crate::app::Msg> {
    let Args {
        handle,
        app_id,
        parent_window,
        options,
        output_images: images,
        tx,
        choice,
        action,
        location,
        toplevel_images,
    } = &args;

    if portal.outputs.len() != images.len() {
        log::error!(
            "Screenshot output count mismatch: {} != {}",
            portal.outputs.len(),
            images.len()
        );
        log::warn!("Screenshot outputs: {:?}", portal.outputs);
        log::warn!("Screenshot images: {:?}", images.keys().collect::<Vec<_>>());
        return cosmic::Task::none();
    }

    // update output bg sources
    if let Ok(c) = cosmic::cosmic_config::Config::new_state(
        cosmic_bg_config::NAME,
        cosmic_bg_config::state::State::version(),
    ) {
        let bg_state = match cosmic_bg_config::state::State::get_entry(&c) {
            Ok(state) => state,
            Err((err, s)) => {
                log::error!("Failed to get bg config state: {:?}", err);
                s
            }
        };
        for o in &mut portal.outputs {
            let source = bg_state.wallpapers.iter().find(|s| s.0 == o.name);
            o.bg_source = Some(source.cloned().map(|s| s.1).unwrap_or_else(|| {
                cosmic_bg_config::Source::Path(
                    "/usr/share/backgrounds/pop/kate-hazen-COSMIC-desktop-wallpaper.png".into(),
                )
            }));
        }
    } else {
        log::error!("Failed to get bg config state");
        for o in &mut portal.outputs {
            o.bg_source = Some(cosmic_bg_config::Source::Path(
                "/usr/share/backgrounds/pop/kate-hazen-COSMIC-desktop-wallpaper.png".into(),
            ));
        }
    }
    portal.location_options = vec![
        fl!("save-to", "clipboard"),
        fl!("save-to", "pictures"),
        fl!("save-to", "documents"),
    ];

    if portal.screenshot_args.replace(args).is_none() {
        // iterate over outputs and create a layer surface for each
        let cmds: Vec<_> = portal
            .outputs
            .iter()
            .map(
                |OutputState {
                     output, id, name, ..
                 }| {
                    get_layer_surface(SctkLayerSurfaceSettings {
                        id: *id,
                        layer: Layer::Overlay,
                        keyboard_interactivity: KeyboardInteractivity::Exclusive,
                        input_zone: None,
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
        cosmic::Task::batch(cmds)
    } else {
        log::info!("Existing screenshot args updated");
        cosmic::Task::none()
    }
}
