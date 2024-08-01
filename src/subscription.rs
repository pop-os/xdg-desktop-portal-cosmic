// contains the subscription which sends portal events and response channels to iced.

use std::{
    any::TypeId,
    fmt::{Debug, Formatter},
};

use cosmic::{cosmic_theme::palette::Srgba, iced::subscription};
use futures::{future, SinkExt};
use tokio::sync::mpsc::Receiver;
use zbus::{zvariant, Connection};

use crate::{
    access::Access, config, file_chooser::FileChooser, screencast::ScreenCast,
    screenshot::Screenshot, wayland, ColorScheme, Contrast, Settings, ACCENT_COLOR_KEY,
    APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, CONTRAST_KEY, DBUS_NAME, DBUS_PATH,
};

#[derive(Clone)]
pub enum Event {
    Access(crate::access::AccessDialogArgs),
    FileChooser(crate::file_chooser::Args),
    Screenshot(crate::screenshot::Args),
    Screencast(Option<crate::screencast_dialog::Args>),
    Accent(Srgba),
    IsDark(bool),
    HighContrast(bool),
    Config(config::Config),
    Init(tokio::sync::mpsc::Sender<Event>),
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
            Event::FileChooser(args) => f
                .debug_struct("FileChooser")
                .field("handle", &args.handle)
                .field("app_id", &args.app_id)
                .field("parent_window", &args.parent_window)
                .field("title", &args.title)
                .field("options", &args.options)
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
            Event::Screencast(_) => todo!(),
            Event::Accent(a) => a.fmt(f),
            Event::IsDark(t) => t.fmt(f),
            Event::HighContrast(c) => c.fmt(f),
            Event::Config(c) => c.fmt(f),
            Event::Init(tx) => tx.fmt(f),
        }
    }
}

pub enum State {
    Init,
    Waiting(Connection, Receiver<Event>),
}

pub(crate) fn portal_subscription(
    helper: wayland::WaylandHelper,
) -> cosmic::iced::Subscription<Event> {
    struct PortalSubscription;
    struct ConfigSubscription;
    subscription::Subscription::batch([
        subscription::channel(
            TypeId::of::<PortalSubscription>(),
            10,
            |mut output| async move {
                let mut state = State::Init;
                loop {
                    if let Err(err) = process_changes(&mut state, &mut output, &helper).await {
                        log::debug!("Portal Subscription Error: {:?}", err);
                        future::pending::<()>().await;
                    }
                }
            },
        ),
        cosmic_config::config_subscription(
            TypeId::of::<ConfigSubscription>(),
            config::APP_ID.into(),
            config::CONFIG_VERSION,
        )
        .map(|update| {
            for error in update.errors {
                log::warn!("Error updating config: {:?}", error);
            }

            Event::Config(update.config)
        }),
    ])
}

pub(crate) async fn process_changes(
    state: &mut State,
    output: &mut futures::channel::mpsc::Sender<Event>,
    wayland_helper: &wayland::WaylandHelper,
) -> anyhow::Result<()> {
    match state {
        State::Init => {
            let (tx, rx) = tokio::sync::mpsc::channel(10);

            let connection = zbus::ConnectionBuilder::session()?
                .name(DBUS_NAME)?
                .serve_at(DBUS_PATH, Access::new(wayland_helper.clone(), tx.clone()))?
                .serve_at(DBUS_PATH, FileChooser::new(tx.clone()))?
                .serve_at(
                    DBUS_PATH,
                    Screenshot::new(wayland_helper.clone(), tx.clone()),
                )?
                .serve_at(
                    DBUS_PATH,
                    ScreenCast::new(wayland_helper.clone(), tx.clone()),
                )?
                .serve_at(DBUS_PATH, Settings::new())?
                .build()
                .await?;
            _ = output.send(Event::Init(tx)).await;
            *state = State::Waiting(connection, rx);
        }
        State::Waiting(conn, rx) => {
            while let Some(event) = rx.recv().await {
                match event {
                    Event::Access(args) => {
                        if let Err(err) = output.send(Event::Access(args)).await {
                            log::error!("Error sending access event: {:?}", err);
                        };
                    }
                    Event::FileChooser(args) => {
                        if let Err(err) = output.send(Event::FileChooser(args)).await {
                            log::error!("Error sending access event: {:?}", err);
                        };
                    }
                    Event::Screenshot(args) => {
                        if let Err(err) = output.send(Event::Screenshot(args)).await {
                            log::error!("Error sending screenshot event: {:?}", err);
                        };
                    }
                    Event::Screencast(args) => {
                        if let Err(err) = output.send(Event::Screencast(args)).await {
                            log::error!("Error sending screencast event: {:?}", err);
                        };
                    }
                    Event::Accent(a) => {
                        let object_server = conn.object_server();
                        let iface_ref = object_server.interface::<_, Settings>(DBUS_PATH).await?;
                        let mut iface = iface_ref.get_mut().await;
                        iface.accent = a.into_format();
                        iface
                            .setting_changed(
                                iface_ref.signal_context(),
                                APPEARANCE_NAMESPACE,
                                ACCENT_COLOR_KEY,
                                zvariant::Array::from(
                                    [iface.accent.red, iface.accent.green, iface.accent.blue]
                                        .as_slice(),
                                )
                                .into(),
                            )
                            .await?;
                    }
                    Event::IsDark(is_dark) => {
                        let object_server = conn.object_server();
                        let iface_ref = object_server.interface::<_, Settings>(DBUS_PATH).await?;
                        let mut iface = iface_ref.get_mut().await;
                        iface.color_scheme = if is_dark {
                            ColorScheme::PreferDark
                        } else {
                            ColorScheme::PreferLight
                        };
                        iface
                            .setting_changed(
                                iface_ref.signal_context(),
                                APPEARANCE_NAMESPACE,
                                COLOR_SCHEME_KEY,
                                zvariant::Value::from(iface.color_scheme as u32),
                            )
                            .await?;
                    }
                    Event::HighContrast(is_high_contrast) => {
                        let object_server = conn.object_server();
                        let iface_ref = object_server.interface::<_, Settings>(DBUS_PATH).await?;
                        let mut iface = iface_ref.get_mut().await;
                        iface.contrast = if is_high_contrast {
                            Contrast::High
                        } else {
                            Contrast::NoPreference
                        };

                        iface
                            .setting_changed(
                                iface_ref.signal_context(),
                                APPEARANCE_NAMESPACE,
                                CONTRAST_KEY,
                                zvariant::Value::from(iface.contrast as u32),
                            )
                            .await?;
                    }
                    Event::Config(config) => {
                        if let Err(err) = output.send(Event::Config(config)).await {
                            log::error!("Error sending config update: {:?}", err)
                        }
                    }
                    Event::Init(_) => {}
                }
            }
        }
    };
    Ok(())
}
