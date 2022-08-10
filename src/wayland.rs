use cosmic_protocols::export_dmabuf::v1::client::{
    zcosmic_export_dmabuf_frame_v1, zcosmic_export_dmabuf_manager_v1,
};
use futures::future::poll_fn;
use smithay::backend::{
    allocator::{dmabuf::DmabufFlags, Fourcc, Modifier},
    drm::node::DrmNode,
};
use std::{
    process,
    sync::{Arc, Mutex},
};
use tokio::io::{unix::AsyncFd, Interest};
use wayland_client::{
    protocol::{wl_output, wl_registry},
    Connection, Dispatch, QueueHandle, WEnum,
};

use crate::dmabuf_frame::{DmabufFrame, Object};

struct WaylandHelperInner {
    connection: wayland_client::Connection,
    export_dmabuf_manager: Option<zcosmic_export_dmabuf_manager_v1::ZcosmicExportDmabufManagerV1>,
    outputs: Vec<wl_output::WlOutput>,
}

#[derive(Clone)]
pub struct WaylandHelper {
    inner: Arc<Mutex<WaylandHelperInner>>,
}

impl WaylandHelper {
    pub fn new(connection: wayland_client::Connection) -> Self {
        // XXX unwrap
        let display = connection.display();
        let mut event_queue = connection.new_event_queue();
        let _registry = display.get_registry(&event_queue.handle(), ()).unwrap();
        let mut data = WaylandHelper {
            inner: Arc::new(Mutex::new(WaylandHelperInner {
                connection,
                export_dmabuf_manager: None,
                outputs: Vec::new(),
            })),
        };
        event_queue.flush().unwrap();

        event_queue.roundtrip(&mut data).unwrap();

        let mut data_clone = data.clone();
        tokio::spawn(async move {
            poll_fn(|cx| event_queue.poll_dispatch_pending(cx, &mut data_clone)).await;
        });

        data
    }

    pub fn dmabuf_exporter(&self) -> Option<DmabufExporter> {
        let inner = self.inner.lock().unwrap();
        Some(DmabufExporter {
            event_queue: inner.connection.new_event_queue(),
            manager: inner.export_dmabuf_manager.clone()?,
        })
    }

    pub fn outputs(&self) -> Vec<wl_output::WlOutput> {
        // TODO Good way to avoid clone?
        self.inner.lock().unwrap().outputs.clone()
    }
}

pub struct DmabufExporter {
    event_queue: wayland_client::EventQueue<CaptureState>,
    manager: zcosmic_export_dmabuf_manager_v1::ZcosmicExportDmabufManagerV1,
}

impl DmabufExporter {
    pub fn capture_output(
        &mut self,
        output: &wl_output::WlOutput,
        overlay_cursor: bool,
    ) -> Result<DmabufFrame, WEnum<zcosmic_export_dmabuf_frame_v1::CancelReason>> {
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
            // Does this work properly if used on multiple threads for multiple event queues?
            self.event_queue.blocking_dispatch(&mut state).unwrap();
        };

        res.and(Ok(state.frame))
    }
}

#[derive(Default)]
struct CaptureState {
    frame: DmabufFrame,
    res: Option<Result<(), WEnum<zcosmic_export_dmabuf_frame_v1::CancelReason>>>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandHelper {
    fn event(
        app_data: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        println!("{:?}", event);
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "zcosmic_export_dmabuf_manager_v1" => {
                    app_data.inner.lock().unwrap().export_dmabuf_manager = Some(
                            registry.bind::<zcosmic_export_dmabuf_manager_v1::ZcosmicExportDmabufManagerV1, _, _>(
                                name,
                                1,
                                qh,
                                (),
                            )
                            .unwrap());
                }
                "wl_output" => {
                    app_data.inner.lock().unwrap().outputs.push(
                        registry
                            .bind::<wl_output::WlOutput, _, _>(name, 4, qh, ())
                            .unwrap(),
                    );
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                // XXX remove output
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for WaylandHelper {
    fn event(
        app_data: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zcosmic_export_dmabuf_manager_v1::ZcosmicExportDmabufManagerV1, ()>
    for WaylandHelper
{
    fn event(
        _: &mut Self,
        _: &zcosmic_export_dmabuf_manager_v1::ZcosmicExportDmabufManagerV1,
        _: zcosmic_export_dmabuf_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
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
