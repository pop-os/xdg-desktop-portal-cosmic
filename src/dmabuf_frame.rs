use smithay::{
    backend::{
        allocator::{
            dmabuf::{Dmabuf, DmabufFlags},
            Fourcc, Modifier,
        },
        drm::node::DrmNode,
        renderer::{
            gles2::Gles2Texture,
            multigpu::{egl::EglGlesBackend, GpuManager},
            Bind, ExportMem,
        },
    },
    utils::{Point, Rectangle, Size},
};
use std::{io::Write, os::unix::io::RawFd};

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
    pub fn write_to_png<T: Write>(&self, gpu_manager: &mut GpuManager<EglGlesBackend>, file: T) {
        // XXX unwrap

        let mut builder = Dmabuf::builder(
            (self.width as i32, self.height as i32),
            self.format.unwrap(),
            self.flags.unwrap(),
        );
        for object in &self.objects {
            builder.add_plane(
                object.fd,
                object.index,
                object.offset,
                object.stride,
                self.modifier.unwrap(),
            );
        }
        let dmabuf = builder.build().unwrap();

        let drm_node = self.node.as_ref().unwrap();
        let mut renderer = gpu_manager
            .renderer::<Gles2Texture>(drm_node, drm_node)
            .unwrap();
        renderer.bind(dmabuf).unwrap();
        let rectangle = Rectangle {
            loc: Point::default(),
            size: Size::from((self.width as i32, self.height as i32)),
        };
        let mapping = renderer.copy_framebuffer(rectangle).unwrap();
        let data = renderer.map_texture(&mapping).unwrap();

        let mut encoder = png::Encoder::new(file, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&data).unwrap();
    }
}
