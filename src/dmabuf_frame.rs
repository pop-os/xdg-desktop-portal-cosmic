use cosmic_protocols::export_dmabuf::v1::client::zcosmic_export_dmabuf_frame_v1;
use smithay::{
    backend::{
        allocator::{
            dmabuf::{Dmabuf, DmabufFlags},
            Fourcc, Modifier,
        },
        drm::node::{CreateDrmNodeError, DrmNode},
        renderer::{
            gles2::Gles2Texture,
            multigpu::{egl::EglGlesBackend, GpuManager},
            Bind, ExportMem,
        },
    },
    utils::{Point, Rectangle, Size},
};
use std::{error::Error, fmt, io::Write, os::unix::io::RawFd};
use wayland_client::WEnum;

#[derive(Debug)]
pub enum DmabufError {
    Cancelled(WEnum<zcosmic_export_dmabuf_frame_v1::CancelReason>),
    Missing(&'static str),
    CreateDrmNode(CreateDrmNodeError),
}

impl fmt::Display for DmabufError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled(WEnum::Value(reason)) => {
                write!(f, "frame cancelled with reason '{:?}'", reason)
            }
            Self::Cancelled(WEnum::Unknown(value)) => {
                write!(f, "frame cancelled with unknown reason '{}'", value)
            }
            Self::Missing(name) => write!(f, "frame missing '{}'", name),
            Self::CreateDrmNode(err) => write!(f, "failed to create drm node for frame: {}", err),
        }
    }
}

impl Error for DmabufError {}

#[derive(Debug, Default)]
pub struct Object {
    pub fd: RawFd, // TODO use `OwnedFd`
    pub index: u32,
    pub offset: u32,
    pub stride: u32,
    pub plane_index: u32,
}

#[derive(Debug, Default)]
pub struct DmabufFrame {
    pub node: Option<DrmNode>,
    pub width: u32,
    pub height: u32,
    pub objects: Vec<Object>,
    pub modifier: Option<Modifier>,
    pub format: Option<Fourcc>,
    pub flags: Option<DmabufFlags>,
    pub ready: bool,
}

impl DmabufFrame {
    pub fn write_to_png<T: Write>(
        &self,
        gpu_manager: &mut GpuManager<EglGlesBackend>,
        file: T,
    ) -> anyhow::Result<()> {
        let mut builder = Dmabuf::builder(
            (self.width as i32, self.height as i32),
            self.format.ok_or(DmabufError::Missing("format"))?,
            self.flags.ok_or(DmabufError::Missing("flags"))?,
        );
        for object in &self.objects {
            builder.add_plane(
                object.fd,
                object.index,
                object.offset,
                object.stride,
                self.modifier.ok_or(DmabufError::Missing("modifier"))?,
            );
        }
        let dmabuf = builder.build().ok_or(DmabufError::Missing("planes"))?;

        let drm_node = self.node.as_ref().ok_or(DmabufError::Missing("drm_node"))?;
        let mut renderer = gpu_manager.renderer::<Gles2Texture>(drm_node, drm_node)?;
        renderer.bind(dmabuf)?;
        let rectangle = Rectangle {
            loc: Point::default(),
            size: Size::from((self.width as i32, self.height as i32)),
        };
        let mapping = renderer.copy_framebuffer(rectangle)?;
        let data = renderer.map_texture(&mapping)?;

        let mut encoder = png::Encoder::new(file, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&data)?;

        Ok(())
    }
}
