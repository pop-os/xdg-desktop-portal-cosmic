use futures::future::poll_fn;
use std::process;
use tokio::io::{unix::AsyncFd, Interest};
use wayland_client::{
    protocol::{wl_output, wl_registry},
    Connection, Dispatch, QueueHandle,
};

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

    tokio::spawn(async move {
        poll_fn(|cx| event_queue.poll_dispatch_pending(cx, &mut data)).await;
    });
}
