#![allow(dead_code, unused_variables)]

use cosmic::cosmic_config::CosmicConfigEntry;
use cosmic::iced::clipboard::mime::AsMimeTypes;
use cosmic::iced::keyboard::Key;
use cosmic::iced::keyboard::key::Named;
use cosmic::iced::platform_specific::shell::commands::layer_surface::destroy_layer_surface;
use cosmic::iced::runtime::clipboard;
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::{Length, Limits, Point, window};
use cosmic::widget::space;
use cosmic_client_toolkit::sctk::output::OutputInfo;
use cosmic_client_toolkit::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use cosmic_client_toolkit::toplevel_info::ToplevelInfo;
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1;
use futures::stream::{self, StreamExt};
use image::RgbaImage;
use rustix::fd::AsFd;
use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::Sender;

use wayland_client::protocol::wl_output::{self, WlOutput};
use zbus::zvariant;

use crate::app::{CosmicPortal, OutputState};
use crate::config::screenshot::ImageSaveLocation;
use crate::config::{self};
use crate::wayland::{CaptureSource, ShmImage, WaylandHelper};
use crate::widget::keyboard_wrapper::KeyboardWrapper;
use crate::widget::rectangle_selection::DragState;
use crate::{PortalResponse, fl, subscription};

#[derive(Clone, Debug)]
pub struct ScreenshotImage {
    pub rgba: RgbaImage,
    pub handle: cosmic::widget::image::Handle,
}

impl ScreenshotImage {
    fn new<T: AsFd>(img: ShmImage<T>) -> anyhow::Result<Self> {
        Ok(Self::from_rgba(img.image_transformed()?))
    }

    fn from_rgba(rgba: RgbaImage) -> Self {
        let handle = cosmic::widget::image::Handle::from_rgba(
            rgba.width(),
            rgba.height(),
            rgba.clone().into_vec(),
        );
        Self { rgba, handle }
    }

    fn placeholder((width, height): (u32, u32)) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let scale = (640.0 / width.max(height) as f32).min(1.0);
        let width = (width as f32 * scale).round().max(1.0) as u32;
        let height = (height as f32 * scale).round().max(1.0) as u32;
        Self::from_rgba(RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([38, 38, 38, 255]),
        ))
    }

    pub fn width(&self) -> u32 {
        self.rgba.width()
    }

    pub fn height(&self) -> u32 {
        self.rgba.height()
    }
}

#[derive(Clone, Debug)]
pub struct ToplevelImage {
    pub image: ScreenshotImage,
    pub geometry: Option<Rect>,
    pub preview_dimensions: (u32, u32),
    pub activated: bool,
}

#[derive(Clone, Debug)]
pub struct PointerSnapshot {
    pub output: String,
    pub position: Point,
}

#[derive(Clone, Debug)]
pub struct ToplevelImageUpdate {
    pub handle: zvariant::ObjectPath<'static>,
    pub images: HashMap<String, Vec<(usize, ScreenshotImage)>>,
}

#[derive(Clone)]
struct ToplevelCapture {
    output: WlOutput,
    output_name: String,
    logical_size: (i32, i32),
    index: usize,
    info: ToplevelInfo,
}

const OUTPUT_CAPTURE_DEADLINE: Duration = Duration::from_millis(500);
const TOPLEVEL_CAPTURE_DEADLINE: Duration = Duration::from_millis(750);
const OUTPUT_CAPTURE_CONCURRENCY: usize = 4;
const TOPLEVEL_CAPTURE_CONCURRENCY: usize = 8;

fn logical_pointer_position(
    (x, y): (i32, i32),
    (buffer_width, buffer_height): (u32, u32),
    (logical_width, logical_height): (i32, i32),
) -> Option<Point> {
    let x = u32::try_from(x).ok()?;
    let y = u32::try_from(y).ok()?;
    let logical_width = u32::try_from(logical_width).ok()?;
    let logical_height = u32::try_from(logical_height).ok()?;
    if buffer_width == 0
        || buffer_height == 0
        || logical_width == 0
        || logical_height == 0
        || x >= buffer_width
        || y >= buffer_height
    {
        return None;
    }

    Some(Point::new(
        x as f32 * logical_width as f32 / buffer_width as f32,
        y as f32 * logical_height as f32 / buffer_height as f32,
    ))
}

fn transformed_output_size(info: &OutputInfo) -> Option<(u32, u32)> {
    let (width, height) = info.modes.iter().find(|mode| mode.current)?.dimensions;
    let width = u32::try_from(width).ok()?;
    let height = u32::try_from(height).ok()?;
    Some(
        if matches!(
            info.transform,
            wl_output::Transform::_90
                | wl_output::Transform::_270
                | wl_output::Transform::Flipped90
                | wl_output::Transform::Flipped270
        ) {
            (height, width)
        } else {
            (width, height)
        },
    )
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

    fn contains(&self, point: Point) -> bool {
        point.x >= self.left as f32
            && point.x < self.right as f32
            && point.y >= self.top as f32
            && point.y < self.bottom as f32
    }

    fn dimensions_or_default(self) -> (u32, u32) {
        (
            self.right.saturating_sub(self.left).unsigned_abs().max(1),
            self.bottom.saturating_sub(self.top).unsigned_abs().max(1),
        )
    }

    pub fn dimensions(self) -> Option<RectDimension> {
        let width = NonZeroU32::new((self.width()).unsigned_abs())?;
        let height = NonZeroU32::new((self.height()).unsigned_abs())?;
        Some(RectDimension { width, height })
    }
}

fn toplevel_geometry(info: &ToplevelInfo, output: &WlOutput) -> Option<Rect> {
    info.geometry.get(output).map(|geometry| {
        let right = geometry.x.saturating_add(geometry.width);
        let bottom = geometry.y.saturating_add(geometry.height);
        Rect {
            left: geometry.x.min(right),
            top: geometry.y.min(bottom),
            right: geometry.x.max(right),
            bottom: geometry.y.max(bottom),
        }
    })
}

fn crop_output_image(
    output_image: &ScreenshotImage,
    geometry: Rect,
    (logical_width, logical_height): (i32, i32),
) -> Option<ScreenshotImage> {
    if logical_width <= 0 || logical_height <= 0 {
        return None;
    }
    let visible = geometry.intersect(Rect {
        left: 0,
        top: 0,
        right: logical_width,
        bottom: logical_height,
    })?;
    let image_width = output_image.width();
    let image_height = output_image.height();
    if image_width == 0 || image_height == 0 {
        return None;
    }

    let scale_x = image_width as f64 / logical_width as f64;
    let scale_y = image_height as f64 / logical_height as f64;
    let left = (visible.left as f64 * scale_x).floor().max(0.0) as u32;
    let top = (visible.top as f64 * scale_y).floor().max(0.0) as u32;
    let right = (visible.right as f64 * scale_x)
        .ceil()
        .clamp(0.0, image_width as f64) as u32;
    let bottom = (visible.bottom as f64 * scale_y)
        .ceil()
        .clamp(0.0, image_height as f64) as u32;
    let width = right.saturating_sub(left);
    let height = bottom.saturating_sub(top);
    if width == 0 || height == 0 {
        return None;
    }

    Some(ScreenshotImage::from_rgba(
        image::imageops::crop_imm(&output_image.rgba, left, top, width, height).to_image(),
    ))
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

    fn toplevel_captures(&self, outputs: &[Output]) -> Vec<ToplevelCapture> {
        outputs
            .iter()
            .flat_map(|output| {
                self.wayland_helper
                    .output_toplevels(&output.output)
                    .into_iter()
                    .enumerate()
                    .map(|(index, info)| ToplevelCapture {
                        output: output.output.clone(),
                        output_name: output.name.clone(),
                        logical_size: output.logical_size,
                        index,
                        info,
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn fallback_toplevel_images(
        &self,
        captures: &[ToplevelCapture],
        output_images: &HashMap<String, ScreenshotImage>,
    ) -> HashMap<String, Vec<ToplevelImage>> {
        let mut images = HashMap::<String, Vec<ToplevelImage>>::new();
        for capture in captures {
            let geometry = toplevel_geometry(&capture.info, &capture.output);
            let preview_dimensions = geometry.map(Rect::dimensions_or_default).unwrap_or((16, 9));
            let image = output_images
                .get(&capture.output_name)
                .and_then(|output_image| {
                    geometry.and_then(|geometry| {
                        crop_output_image(output_image, geometry, capture.logical_size)
                    })
                })
                .unwrap_or_else(|| ScreenshotImage::placeholder(preview_dimensions));
            let output_images = images.entry(capture.output_name.clone()).or_default();
            debug_assert_eq!(capture.index, output_images.len());
            output_images.push(ToplevelImage {
                image,
                geometry,
                preview_dimensions,
                activated: capture
                    .info
                    .state
                    .contains(&zcosmic_toplevel_handle_v1::State::Activated),
            });
        }
        images
    }

    async fn interactive_toplevel_images(
        &self,
        captures: &[ToplevelCapture],
        pointer: Option<&PointerSnapshot>,
    ) -> HashMap<String, Vec<(usize, ScreenshotImage)>> {
        let mut capture_order = (0..captures.len()).collect::<Vec<_>>();
        capture_order.sort_by(|&a, &b| {
            let a = &captures[a];
            let b = &captures[b];
            let a_hovered = pointer.is_some_and(|pointer| {
                pointer.output == a.output_name
                    && toplevel_geometry(&a.info, &a.output)
                        .is_some_and(|geometry| geometry.contains(pointer.position))
            });
            let b_hovered = pointer.is_some_and(|pointer| {
                pointer.output == b.output_name
                    && toplevel_geometry(&b.info, &b.output)
                        .is_some_and(|geometry| geometry.contains(pointer.position))
            });
            let a_activated = a
                .info
                .state
                .contains(&zcosmic_toplevel_handle_v1::State::Activated);
            let b_activated = b
                .info
                .state
                .contains(&zcosmic_toplevel_handle_v1::State::Activated);

            b_hovered
                .cmp(&a_hovered)
                .then_with(|| b_activated.cmp(&a_activated))
                .then_with(|| a.index.cmp(&b.index))
        });

        let wayland_helper = self.wayland_helper.clone();
        let mut pending = stream::iter(capture_order.into_iter().map(|capture_i| {
            let wayland_helper = wayland_helper.clone();
            let capture = captures[capture_i].clone();
            async move {
                let source = CaptureSource::Toplevel(capture.info.foreign_toplevel.clone());
                let image = wayland_helper
                    .capture_source_shm(source, false)
                    .await
                    .and_then(|image| ScreenshotImage::new(image).ok());
                (capture.output_name, capture.index, image)
            }
        }))
        .buffer_unordered(TOPLEVEL_CAPTURE_CONCURRENCY);
        let deadline = tokio::time::Instant::now() + TOPLEVEL_CAPTURE_DEADLINE;
        let mut images = HashMap::<String, Vec<(usize, ScreenshotImage)>>::new();

        loop {
            match tokio::time::timeout_at(deadline, pending.next()).await {
                Ok(Some((output, index, Some(image)))) => {
                    images.entry(output).or_default().push((index, image));
                }
                Ok(Some((output, index, None))) => {
                    tracing::debug!("Failed to capture window {index} on output {output}");
                }
                Ok(None) => break,
                Err(_) => {
                    tracing::warn!(
                        "Window preview capture exceeded {:?}; keeping fast fallback previews",
                        TOPLEVEL_CAPTURE_DEADLINE
                    );
                    break;
                }
            }
        }

        for output_images in images.values_mut() {
            output_images.sort_by_key(|(index, _)| *index);
        }
        images
    }

    async fn interactive_output_images(
        &self,
        outputs: &[Output],
        app_id: &str,
    ) -> (HashMap<String, ScreenshotImage>, Option<PointerSnapshot>) {
        // collect screenshots from each output

        let wayland_helper = self.wayland_helper.clone();
        // Cursor sessions report the current position relative to their capture source. Create
        // them before requesting the output frames: by the time a frame is ready, the initial
        // cursor metadata queued before it has also been dispatched on the Wayland event queue.
        let cursor_sessions = outputs
            .iter()
            .filter_map(|Output { output, name, .. }| {
                wayland_helper
                    .capture_source_cursor_session(CaptureSource::Output(output.clone()))
                    .map(|(session, _stream)| (name.clone(), session))
            })
            .collect::<HashMap<_, _>>();

        let mut map = HashMap::with_capacity(outputs.len());
        let output_requests = outputs
            .iter()
            .map(|output| output.output.clone())
            .collect::<Vec<_>>();
        let mut pending = stream::iter(output_requests.into_iter().map(|output| {
            let wayland_helper = wayland_helper.clone();
            let result_output = output.clone();
            async move {
                let image = wayland_helper
                    .capture_source_shm(CaptureSource::Output(output), false)
                    .await
                    .and_then(|image| ScreenshotImage::new(image).ok());
                (result_output, image)
            }
        }))
        .buffer_unordered(OUTPUT_CAPTURE_CONCURRENCY);
        let deadline = tokio::time::Instant::now() + OUTPUT_CAPTURE_DEADLINE;
        let mut pointer = None;

        loop {
            match tokio::time::timeout_at(deadline, pending.next()).await {
                Ok(Some((output, Some(image)))) => {
                    let Some(output_state) = outputs.iter().find(|item| item.output == output)
                    else {
                        continue;
                    };
                    if pointer.is_none()
                        && let Some(session) = cursor_sessions.get(&output_state.name)
                        && session.cursor_entered()
                        && let Some(position) = logical_pointer_position(
                            session.cursor_position(),
                            (image.width(), image.height()),
                            output_state.logical_size,
                        )
                    {
                        pointer = Some(PointerSnapshot {
                            output: output_state.name.clone(),
                            position,
                        });
                    }
                    map.insert(output_state.name.clone(), image);
                }
                Ok(Some((output, None))) => {
                    tracing::warn!("Failed to capture output {output:?}; using a placeholder");
                }
                Ok(None) => break,
                Err(_) => {
                    tracing::warn!(
                        "Output capture exceeded {:?}; showing the screenshot UI with fallbacks",
                        OUTPUT_CAPTURE_DEADLINE
                    );
                    break;
                }
            }
        }

        if pointer.is_none() {
            pointer = outputs.iter().find_map(|output| {
                let session = cursor_sessions.get(&output.name)?;
                if !session.cursor_entered() {
                    return None;
                }
                let buffer_size = self
                    .wayland_helper
                    .output_info(&output.output)
                    .as_ref()
                    .and_then(transformed_output_size)?;
                logical_pointer_position(
                    session.cursor_position(),
                    buffer_size,
                    output.logical_size,
                )
                .map(|position| PointerSnapshot {
                    output: output.name.clone(),
                    position,
                })
            });
        }

        for output in outputs {
            map.entry(output.name.clone()).or_insert_with(|| {
                ScreenshotImage::placeholder((
                    u32::try_from(output.logical_size.0).unwrap_or(1),
                    u32::try_from(output.logical_size.1).unwrap_or(1),
                ))
            });
        }

        (map, pointer)
    }

    pub fn save_rgba(img: &RgbaImage, path: Option<&Path>) -> anyhow::Result<Vec<u8>> {
        // Write to the buffer first since the image data will always be copied to the clipboard.
        // This skips encoding the PNG twice.
        let mut buffer = Vec::new();
        write_png(&mut buffer, img)?;

        if let Some(path) = path {
            std::fs::write(path, &buffer)?;
        }

        Ok(buffer)
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
            tracing::error!("Failed to create screenshot directory {:?}: {}", path, err);
            return None;
        }

        let name = jiff::Zoned::now()
            .strftime("Screenshot_%Y-%m-%d_%H-%M-%S.png")
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
    CaptureWithLocation(ImageSaveLocation),
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
    pub toplevel_images: HashMap<String, Vec<ToplevelImage>>,
    pub initial_pointer: Option<PointerSnapshot>,
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
                tracing::warn!("Output {:?} has no info", output);
                continue;
            };
            let Some(name) = info.name.clone() else {
                tracing::warn!("Output {:?} has no name", output);
                continue;
            };
            let Some(logical_position) = info.logical_position else {
                tracing::warn!("Output {:?} has no position", output);
                continue;
            };
            let Some(logical_size) = info.logical_size else {
                tracing::warn!("Output {:?} has no size", output);
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
            tracing::error!("No output");
            return PortalResponse::Other;
        };

        // if interactive, send image to be used by screenshot editor & await response via channel
        if options.interactive.unwrap_or_default() {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let first_output = &*outputs[0].name;
            let toplevel_captures = self.toplevel_captures(&outputs);
            let (output_images, initial_pointer) =
                self.interactive_output_images(&outputs, app_id).await;
            let toplevel_images = self.fallback_toplevel_images(&toplevel_captures, &output_images);
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
                    initial_pointer: initial_pointer.clone(),
                    tx,
                    location: config.save_location,
                    // TODO cover all outputs at start of rectangle?
                    choice,
                    // will be updated
                }))
                .await
            {
                tracing::error!("Failed to send screenshot event, {}", err);
                return PortalResponse::Other;
            }

            let toplevel_images =
                self.interactive_toplevel_images(&toplevel_captures, initial_pointer.as_ref());
            tokio::pin!(toplevel_images);
            tokio::select! {
                biased;
                response = rx.recv() => {
                    return response.unwrap_or(PortalResponse::Cancelled);
                }
                images = &mut toplevel_images => {
                    if !images.is_empty()
                        && let Err(err) = self.tx.try_send(
                            subscription::Event::ScreenshotToplevels(ToplevelImageUpdate {
                                handle: handle.to_owned(),
                                images,
                            })
                        )
                    {
                        tracing::warn!("Failed to update screenshot window previews: {err}");
                    }
                }
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
                tracing::error!("Failed to capture screenshot: {}", err);
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
        return space::horizontal().width(Length::Fixed(1.0)).into();
    };
    let Some(args) = portal.screenshot_args.as_ref() else {
        return space::horizontal().width(Length::Fixed(1.0)).into();
    };

    let Some(img) = args.output_images.get(&output.name) else {
        return space::horizontal().width(Length::Fixed(1.0)).into();
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
        |key, modifiers| {
            if modifiers.control() {
                match key {
                    Key::Named(Named::Copy) => {
                        return Some(Msg::CaptureWithLocation(ImageSaveLocation::Clipboard));
                    }
                    Key::Named(Named::Save) => {
                        return Some(Msg::CaptureWithLocation(ImageSaveLocation::Pictures));
                    }
                    Key::Character(ref value) => {
                        let value = value.as_str();
                        if value.eq_ignore_ascii_case("c") {
                            return Some(Msg::CaptureWithLocation(ImageSaveLocation::Clipboard));
                        } else if value.eq_ignore_ascii_case("s") {
                            return Some(Msg::CaptureWithLocation(ImageSaveLocation::Pictures));
                        }
                    }
                    _ => {}
                }
            }

            match key {
                Key::Named(Named::Enter) => Some(Msg::Capture),
                Key::Named(Named::Escape) => Some(Msg::Cancel),
                _ => None,
            }
        },
    )
    .into()
}

pub fn update_msg(
    portal: &mut CosmicPortal,
    msg: Msg,
) -> cosmic::Task<cosmic::Action<crate::app::Msg>> {
    match msg {
        Msg::Capture => {
            let mut cmds: Vec<cosmic::Task<cosmic::Action<crate::app::Msg>>> = portal
                .outputs
                .iter()
                .map(|o| destroy_layer_surface(o.id))
                .collect();
            let Some(args) = portal.screenshot_args.take() else {
                tracing::error!("Failed to find screenshot Args for Capture message.");
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
                        if let Ok(buffer) = Screenshot::save_rgba(&img.rgba, image_path.as_deref())
                            .inspect_err(|err| {
                                tracing::error!("Failed to capture screenshot: {:?}", err);
                                success = false;
                            })
                        {
                            cmds.push(clipboard::write_data(ScreenshotBytes::new(buffer)));
                        }
                    } else {
                        tracing::error!("Failed to find output {}", name);
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

                        if let Ok(buffer) = Screenshot::save_rgba(&img, image_path.as_deref())
                            .inspect_err(|err| {
                                tracing::error!("Failed to capture screenshot: {:?}", err);
                                success = false;
                            })
                        {
                            cmds.push(clipboard::write_data(ScreenshotBytes::new(buffer)));
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
                        if let Ok(buffer) =
                            Screenshot::save_rgba(&img.image.rgba, image_path.as_deref())
                                .inspect_err(|err| {
                                    tracing::error!("Failed to capture screenshot: {:?}", err);
                                    success = false;
                                })
                        {
                            cmds.push(clipboard::write_data(ScreenshotBytes::new(buffer)));
                        }
                    } else {
                        success = false;
                    }
                }
                _ => {
                    success = false;
                }
            }

            let response = if success && let Some(image_path) = image_path {
                PortalResponse::Success(ScreenshotResult {
                    uri: format!("file:///{}", image_path.display()),
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
                    tracing::error!("Failed to send screenshot event");
                }
            });
            cosmic::Task::batch(cmds)
        }
        Msg::CaptureWithLocation(location) => {
            if let Some(args) = portal.screenshot_args.as_mut() {
                args.location = location;
            } else {
                tracing::error!("Failed to find screenshot Args for CaptureWithLocation message.");
                return cosmic::Task::none();
            }
            update_msg(portal, Msg::Capture)
        }
        Msg::Cancel => {
            let cmds = portal.outputs.iter().map(|o| destroy_layer_surface(o.id));
            let Some(args) = portal.screenshot_args.take() else {
                tracing::error!("Failed to find screenshot Args for Cancel message.");
                return cosmic::Task::batch(cmds);
            };
            let Args { tx, .. } = args;
            tokio::spawn(async move {
                if let Err(err) = tx.send(PortalResponse::Cancelled).await {
                    tracing::error!("Failed to send screenshot event");
                }
            });

            cosmic::Task::batch(cmds)
        }
        Msg::Choice(c) => {
            let was_window = portal
                .screenshot_args
                .as_ref()
                .is_some_and(|args| matches!(args.choice, Choice::Window(..)));
            match &c {
                Choice::Window(name, _) if !was_window => {
                    for output in &mut portal.outputs {
                        output.window_pointer_anchor = None;
                    }
                    if let Some(output) = portal
                        .outputs
                        .iter_mut()
                        .find(|output| output.name == *name && output.has_pointer)
                    {
                        output.window_pointer_anchor = output.pointer_position;
                    }
                }
                Choice::Window(..) => {}
                _ => {
                    for output in &mut portal.outputs {
                        output.window_pointer_anchor = None;
                    }
                }
            }

            let choice = (&c).into();
            // Only save config when drag is finished to avoid disk writes on every mouse motion
            let should_save_config =
                !matches!(&c, Choice::Rectangle(_, s) if *s != DragState::None);
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
                tracing::error!("Failed to find screenshot Args for Choice message.");
            }
            if should_save_config {
                cosmic::task::message(crate::app::Msg::ConfigSetScreenshot(
                    config::screenshot::Screenshot {
                        choice,
                        last_rectangle: last_rect,
                        ..portal.config.screenshot
                    },
                ))
            } else {
                cosmic::Task::none()
            }
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
                tracing::error!(
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
                tracing::error!("Failed to find screenshot Args for WindowChosen message.");
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
                tracing::error!("Failed to find screenshot Args for Location message.");
                cosmic::Task::none()
            }
        }
    }
}

pub fn update_toplevel_images(
    portal: &mut CosmicPortal,
    update: ToplevelImageUpdate,
) -> cosmic::Task<cosmic::Action<crate::app::Msg>> {
    let Some(args) = portal
        .screenshot_args
        .as_mut()
        .filter(|args| args.handle == update.handle)
    else {
        tracing::debug!("Ignoring stale screenshot window preview update");
        return cosmic::Task::none();
    };

    for (output, images) in update.images {
        let Some(toplevels) = args.toplevel_images.get_mut(&output) else {
            continue;
        };
        for (index, image) in images {
            if let Some(toplevel) = toplevels.get_mut(index) {
                toplevel.image = image;
            }
        }
    }

    cosmic::Task::none()
}

pub fn update_args(
    portal: &mut CosmicPortal,
    mut args: Args,
) -> cosmic::Task<cosmic::Action<crate::app::Msg>> {
    for output in &portal.outputs {
        args.output_images
            .entry(output.name.clone())
            .or_insert_with(|| ScreenshotImage::placeholder(output.logical_size));
        args.toplevel_images.entry(output.name.clone()).or_default();
    }

    let Args {
        handle,
        app_id,
        parent_window,
        options,
        output_images: images,
        initial_pointer,
        tx,
        choice,
        action,
        location,
        toplevel_images,
    } = &args;

    if portal.outputs.len() != images.len() {
        tracing::warn!(
            "Screenshot output count mismatch: {} != {}",
            portal.outputs.len(),
            images.len()
        );
    }

    for output in &mut portal.outputs {
        output.has_pointer = false;
        output.pointer_position = None;
        output.window_pointer_anchor = None;

        if let Some(pointer) = initial_pointer
            && pointer.output == output.name
        {
            output.has_pointer = true;
            output.pointer_position = Some(pointer.position);
            if matches!(choice, Choice::Window(..)) {
                output.window_pointer_anchor = Some(pointer.position);
            }
        }
    }

    // update output bg sources
    if let Ok(c) = cosmic::cosmic_config::Config::new_state(
        cosmic_bg_config::NAME,
        cosmic_bg_config::state::State::version(),
    ) {
        let bg_state = match cosmic_bg_config::state::State::get_entry(&c) {
            Ok(state) => state,
            Err((err, s)) => {
                tracing::error!("Failed to get bg config state: {:?}", err);
                s
            }
        };
        for o in &mut portal.outputs {
            let source = bg_state.wallpapers.iter().find(|s| s.0 == o.name);
            o.bg_source = Some(source.cloned().map(|s| s.1).unwrap_or_else(|| {
                cosmic_bg_config::Source::Path(
                    "/usr/share/backgrounds/cosmic/orion_nebula_nasa_heic0601a.jpg".into(),
                )
            }));
        }
    } else {
        tracing::error!("Failed to get bg config state");
        for o in &mut portal.outputs {
            o.bg_source = Some(cosmic_bg_config::Source::Path(
                "/usr/share/backgrounds/cosmic/orion_nebula_nasa_heic0601a.jpg".into(),
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
                    let id = *id;
                    let output = output.clone();
                    let name = name.clone();
                    cosmic::surface::surface_task::<crate::app::Msg>(
                cosmic::surface::action::simple_layer_shell::<crate::app::Msg>(
                    Default::default,
                        move || {
                            SctkLayerSurfaceSettings {
                                    id,
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
                                }
                            },
                        None::<fn() -> cosmic::Element<'static, cosmic::Action<crate::app::Msg>>>,
                        ),
                    )
                },
            )
            .collect();
        cosmic::Task::batch(cmds)
    } else {
        tracing::info!("Existing screenshot args updated");
        cosmic::Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_buffer_position_is_converted_to_logical_coordinates() {
        assert_eq!(
            logical_pointer_position((1500, 500), (3000, 2000), (1500, 1000)),
            Some(Point::new(750.0, 250.0))
        );
    }

    #[test]
    fn cursor_hotspot_outside_output_is_ignored() {
        assert_eq!(
            logical_pointer_position((-1, 500), (3000, 2000), (1500, 1000)),
            None
        );
        assert_eq!(
            logical_pointer_position((3000, 500), (3000, 2000), (1500, 1000)),
            None
        );
    }

    #[test]
    fn fallback_window_preview_uses_logical_output_scaling() {
        let output = ScreenshotImage::from_rgba(RgbaImage::new(200, 100));
        let preview = crop_output_image(
            &output,
            Rect {
                left: 25,
                top: 25,
                right: 75,
                bottom: 75,
            },
            (100, 100),
        )
        .unwrap();

        assert_eq!((preview.width(), preview.height()), (100, 50));
    }

    #[test]
    fn placeholder_preserves_window_aspect_ratio() {
        let placeholder = ScreenshotImage::placeholder((1600, 200));

        assert_eq!((placeholder.width(), placeholder.height()), (640, 80));
    }
}
