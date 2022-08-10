use cosmic_protocols::export_dmabuf::v1::client::{
    zcosmic_export_dmabuf_frame_v1, zcosmic_export_dmabuf_manager_v1,
};
use futures::future::poll_fn;
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
use std::{collections::HashMap, fs, io, os::unix::io::RawFd, process};
use tokio::io::{unix::AsyncFd, Interest};
use wayland_client::{
    protocol::{wl_output, wl_registry},
    Connection, Dispatch, QueueHandle, WEnum,
};

struct DmabufExporter {
    event_queue: wayland_client::EventQueue<CaptureState>,
    manager: zcosmic_export_dmabuf_manager_v1::ZcosmicExportDmabufManagerV1,
}

impl DmabufExporter {
    fn capture_output(
        &mut self,
        output: &wl_output::WlOutput,
        overlay_cursor: bool,
    ) -> Result<DmaBufFrame, WEnum<zcosmic_export_dmabuf_frame_v1::CancelReason>> {
        // TODO: way to get cursor metadata?

        let overlay_cursor = if overlay_cursor { 1 } else { 0 };
        self.manager
            .capture_output(overlay_cursor, output, &self.event_queue.handle(), ())
            .unwrap();
        self.event_queue.flush().unwrap();

        let mut state = CaptureState::default();
        let res = loop {
            if let Some(res) = state.res {
                break res;
            }
            self.event_queue.blocking_dispatch(&mut state).unwrap();
        };

        res.and(Ok(state.frame))
    }
}

#[derive(Default)]
struct CaptureState {
    frame: DmaBufFrame,
    res: Option<Result<(), WEnum<zcosmic_export_dmabuf_frame_v1::CancelReason>>>,
}

// XXX
#[derive(Default)]
struct AppData {
    frames: HashMap<String, DmaBufFrame>,
}

#[derive(Debug, Default)]
struct Object {
    fd: RawFd,
    index: u32,
    offset: u32,
    stride: u32,
    plane_index: u32,
}

#[derive(Debug, Default)]
struct DmaBufFrame {
    node: Option<DrmNode>,
    width: u32,
    height: u32,
    objects: Vec<Object>,
    modifier: Option<Modifier>,
    format: Option<Fourcc>,
    flags: Option<DmabufFlags>,
    ready: bool,
}

struct Globals(std::collections::BTreeMap<u32, (String, u32)>);

impl Dispatch<wl_registry::WlRegistry, ()> for Globals {
    fn event(
        app_data: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Globals>,
    ) {
        println!("{:?}", event);
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                app_data.0.insert(name, (interface, version));
            }
            wl_registry::Event::GlobalRemove { name } => {
                app_data.0.remove(&name);
            }
            _ => {}
        }
    }
}

impl Dispatch<zcosmic_export_dmabuf_frame_v1::ZcosmicExportDmabufFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &zcosmic_export_dmabuf_frame_v1::ZcosmicExportDmabufFrameV1,
        event: zcosmic_export_dmabuf_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<CaptureState>,
    ) {
        let mut frame = &mut state.frame;

        match event {
            zcosmic_export_dmabuf_frame_v1::Event::Device { ref node } => {
                let node = u64::from_ne_bytes([
                    node[0], node[1], node[2], node[3], node[4], node[5], node[6], node[7],
                ]);
                frame.node = Some(DrmNode::from_dev_id(node).unwrap());
            }
            zcosmic_export_dmabuf_frame_v1::Event::Frame {
                width,
                height,
                mod_high,
                mod_low,
                format,
                flags,
                ..
            } => {
                frame.width = width;
                frame.height = height;
                frame.format = Some(Fourcc::try_from(format).unwrap());
                frame.modifier = Some(Modifier::from(((mod_high as u64) << 32) + mod_low as u64));
                frame.flags = Some(DmabufFlags::from_bits(u32::from(flags)).unwrap());
            }
            zcosmic_export_dmabuf_frame_v1::Event::Object {
                fd,
                index,
                offset,
                stride,
                plane_index,
                ..
            } => {
                assert!(plane_index == frame.objects.last().map_or(0, |x| x.plane_index + 1));
                frame.objects.push(Object {
                    fd,
                    index,
                    offset,
                    stride,
                    plane_index,
                });
            }
            zcosmic_export_dmabuf_frame_v1::Event::Ready { .. } => {
                state.res = Some(Ok(()));
            }
            zcosmic_export_dmabuf_frame_v1::Event::Cancel { reason } => {
                state.res = Some(Err(reason));
            }
            _ => {}
        }
    }
}

async fn read_event_task(connection: wayland_client::Connection) {
    // XXX unwrap
    let fd = connection.prepare_read().unwrap().connection_fd();
    let async_fd = AsyncFd::with_interest(fd, Interest::READABLE).unwrap();
    loop {
        let read_event_guard = connection.prepare_read().unwrap();
        let mut read_guard = async_fd.readable().await.unwrap();
        read_event_guard.read().unwrap();
        read_guard.clear_ready();
    }
}

// Connect to wayland and start task reading events from socket
pub fn connect_to_wayland() -> wayland_client::Connection {
    let wayland_connection = match wayland_client::Connection::connect_to_env() {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("Error: {}", err);
            process::exit(1)
        }
    };

    tokio::spawn(read_event_task(wayland_connection.clone()));

    wayland_connection
}

pub fn monitor_wayland_registry(connection: wayland_client::Connection) {
    // XXX unwrap
    let display = connection.display();
    let mut event_queue = connection.new_event_queue::<Globals>();
    let _registry = display.get_registry(&event_queue.handle(), ()).unwrap();
    let mut data = Globals(Default::default());
    event_queue.flush().unwrap();

    event_queue.roundtrip(&mut data).unwrap();

    tokio::spawn(async move {
        poll_fn(|cx| event_queue.poll_dispatch_pending(cx, &mut data)).await;
    });
}
