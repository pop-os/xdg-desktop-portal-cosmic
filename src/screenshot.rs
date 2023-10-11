#![allow(dead_code, unused_variables)]

use cosmic_client_toolkit::sctk::output::OutputInfo;
use std::{collections::HashMap, io, path::PathBuf};
use wayland_client::protocol::wl_output;
use zbus::zvariant;

use crate::wayland::WaylandHelper;
use crate::PortalResponse;

// TODO save to /run/user/$UID/doc/ with document portal fuse filesystem?

#[derive(zvariant::DeserializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct ScreenshotOptions {
    modal: Option<bool>,
    interactive: Option<bool>,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct ScreenshotResult {
    uri: String,
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
struct PickColorResult {
    color: (f64, f64, f64), // (ddd)
}

#[derive(Clone, Copy, Debug, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

pub struct Screenshot {
    wayland_helper: WaylandHelper,
}

impl Screenshot {
    pub fn new(wayland_helper: WaylandHelper) -> Self {
        Self { wayland_helper }
    }

    async fn screenshot_inner(
        &self,
        outputs: Vec<(wl_output::WlOutput, (i32, i32))>,
        app_id: &str,
    ) -> anyhow::Result<PathBuf> {
        use ashpd::documents::Permission;

        let wayland_helper = self.wayland_helper.clone();
        let (file, path) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let mut bounds_opt: Option<Rect> = None;
            let mut frames = Vec::with_capacity(outputs.len());
            for (output, (output_x, output_y)) in outputs {
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

#[zbus::dbus_interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl Screenshot {
    async fn screenshot(
        &self,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        option: ScreenshotOptions,
    ) -> PortalResponse<ScreenshotResult> {
        // connection.object_server().at(&handle, Request);

        // TODO create handle, show dialog

        let mut outputs = Vec::new();
        for output in self.wayland_helper.outputs() {
            match self.wayland_helper.output_info(&output) {
                Some(output_info) => match output_info.logical_position {
                    Some((x, y)) => {
                        outputs.push((output, (x, y)));
                    }
                    None => {
                        log::warn!("Output {:?} has no logical position", output);
                    }
                },
                None => {
                    log::warn!("Output {:?} has no info", output);
                }
            }
        }
        if outputs.is_empty() {
            log::error!("No output");
            return PortalResponse::Other;
        };

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
}
