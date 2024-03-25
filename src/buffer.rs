use std::{
    ffi::CStr,
    os::fd::{AsFd, OwnedFd},
};

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

pub fn create_memfd(width: u32, height: u32) -> OwnedFd {
    // TODO: BSD support using shm_open
    let name = unsafe { CStr::from_bytes_with_nul_unchecked(b"pipewire-screencopy\0") };
    let fd = rustix::fs::memfd_create(name, rustix::fs::MemfdFlags::CLOEXEC).unwrap(); // XXX
    rustix::fs::ftruncate(&fd, (width * height * 4) as _).unwrap();
    fd
}

pub fn create_dmabuf<T: AsFd>(
    device: &gbm::Device<T>,
    modifier: gbm::Modifier,
    width: u32,
    height: u32,
) -> Dmabuf<OwnedFd> {
    let buffer = if modifier != gbm::Modifier::Invalid {
        device
            .create_buffer_object_with_modifiers2::<()>(
                width,
                height,
                gbm::Format::Abgr8888,
                [modifier].into_iter(),
                gbm::BufferObjectFlags::empty(),
            )
            .unwrap()
    } else {
        device
            .create_buffer_object::<()>(
                width,
                height,
                gbm::Format::Abgr8888,
                gbm::BufferObjectFlags::empty(),
            )
            .unwrap()
    };
    Dmabuf {
        format: gbm::Format::Abgr8888,
        modifier,
        width,
        height,
        planes: (0..buffer.plane_count().unwrap() as i32)
            .map(|i| Plane {
                fd: buffer.fd_for_plane(i).unwrap(), // XXX
                offset: buffer.offset(i).unwrap(),
                stride: buffer.stride_for_plane(i).unwrap(),
            })
            .collect(),
    }
}
