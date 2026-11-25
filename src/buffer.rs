use std::os::fd::{AsFd, OwnedFd};

pub struct Plane<Fd: AsFd> {
    pub fd: Fd,
    pub offset: u32,
    pub stride: u32,
}

pub struct Dmabuf<Fd: AsFd> {
    pub format: gbm::Format,
    pub modifier: gbm::Modifier,
    pub width: u32,
    pub height: u32,
    pub planes: Vec<Plane<Fd>>,
}

pub fn create_memfd(width: u32, height: u32) -> rustix::io::Result<OwnedFd> {
    // TODO: BSD support using shm_open
    let name = c"pipewire-screencopy";
    let fd = rustix::fs::memfd_create(name, rustix::fs::MemfdFlags::CLOEXEC)?;
    rustix::fs::ftruncate(&fd, (width * height * 4) as _)?;
    Ok(fd)
}

pub fn create_dmabuf<T: AsFd>(
    device: &gbm::Device<T>,
    format: gbm::Format,
    modifier: gbm::Modifier,
    width: u32,
    height: u32,
) -> anyhow::Result<Dmabuf<OwnedFd>> {
    let buffer = if modifier != gbm::Modifier::Invalid {
        device.create_buffer_object_with_modifiers2::<()>(
            width,
            height,
            format,
            [modifier].into_iter(),
            gbm::BufferObjectFlags::empty(),
        )?
    } else {
        device.create_buffer_object::<()>(width, height, format, gbm::BufferObjectFlags::empty())?
    };
    Ok(Dmabuf {
        format,
        modifier,
        width,
        height,
        planes: (0..buffer.plane_count() as i32)
            .map(|i| {
                Ok(Plane {
                    fd: buffer.fd_for_plane(i)?,
                    offset: buffer.offset(i),
                    stride: buffer.stride_for_plane(i),
                })
            })
            .collect::<anyhow::Result<_>>()?,
    })
}
