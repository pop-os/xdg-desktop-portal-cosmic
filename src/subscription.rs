// contains the subscription which sends portal events and response channels to iced.

use std::{
    any::TypeId,
    fmt::{Debug, Formatter},
};

use cosmic::iced::subscription;
use futures::{future, SinkExt};
use tokio::sync::mpsc::Receiver;
use zbus::Connection;

use crate::{
    access::Access, screencast::ScreenCast, screenshot::Screenshot, wayland, DBUS_NAME, DBUS_PATH,
};

#[derive(Clone)]
pub enum Event {
    Access(crate::access::AccessDialogArgs),
    Screenshot(crate::screenshot::Args),
}

impl Debug for Event {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Access(args) => f
                .debug_struct("Access")
                .field("title", &args.title)
                .field("subtitle", &args.subtitle)
                .field("body", &args.body)
                .field("options", &args.options)
                .field("app_id", &args.app_id)
                .field("parent_window", &args.parent_window)
                .field("handle", &args.handle)
                .finish(),
            Event::Screenshot(crate::screenshot::Args {
                handle,
                app_id,
                parent_window,
                options,
                output_images: images,
                choice,
                action,
                location,
                tx: _tx,
                toplevel_images,
            }) => f
                .debug_struct("Screenshot")
                .field("handle", handle)
                .field("app_id", app_id)
                .field("parent_window", parent_window)
                .field("images", &images.keys().collect::<Vec<_>>())
                .field("options", options)
                .field("choice", choice)
                .field("action", action)
                .field("location", location)
                .field("toplevel_images", toplevel_images)
                .finish(),
        }
    }
}

pub enum State {
    Init,
    Waiting(Connection, Receiver<Event>),
}

pub(crate) fn portal_subscription() -> cosmic::iced::Subscription<Event> {
    subscription::channel(TypeId::of::<Event>(), 10, |mut output| async move {
        let mut state = State::Init;
        loop {
            if let Err(err) = process_changes(&mut state, &mut output).await {
                log::debug!("Portal Subscription Error: {:?}", err);
                future::pending::<()>().await;
            }
        }
    })
}

pub(crate) async fn process_changes(
    state: &mut State,
    output: &mut futures::channel::mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    match state {
        State::Init => {
            let (tx, rx) = tokio::sync::mpsc::channel(10);
            let wayland_connection = wayland::connect_to_wayland();
            let wayland_helper = wayland::WaylandHelper::new(wayland_connection);

            let connection = zbus::ConnectionBuilder::session()?
                .name(DBUS_NAME)?
                .serve_at(DBUS_PATH, Access::new(wayland_helper.clone(), tx.clone()))?
                .serve_at(
                    DBUS_PATH,
                    Screenshot::new(wayland_helper.clone(), tx.clone()),
                )?
                .serve_at(DBUS_PATH, ScreenCast::new(wayland_helper))?
                .build()
                .await?;
            *state = State::Waiting(connection, rx);
        }
        State::Waiting(_, rx) => {
            while let Some(event) = rx.recv().await {
                match event {
                    Event::Access(args) => {
                        if let Err(err) = output.send(Event::Access(args)).await {
                            log::error!("Error sending access event: {:?}", err);
                        };
                    }
                    Event::Screenshot(args) => {
                        if let Err(err) = output.send(Event::Screenshot(args)).await {
                            log::error!("Error sending screenshot event: {:?}", err);
                        };
                    }
                }
            }
        }
    };
    Ok(())
}
