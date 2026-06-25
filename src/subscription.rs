// contains the subscription which sends portal events and response channels to iced.

use std::any::TypeId;
use std::hash::Hash;

use anyhow::Context;
use cosmic::cosmic_theme::palette::Srgba;
use cosmic::iced::Subscription;
use futures::{SinkExt, StreamExt, future};
use tokio::sync::broadcast;
use tokio::sync::mpsc::Receiver;
use zbus::{Connection, fdo, zvariant};

use crate::access::Access;
use crate::background::Background;
use crate::file_chooser::FileChooser;
use crate::screencast::ScreenCast;
use crate::screenshot::Screenshot;
use crate::{
    ACCENT_COLOR_KEY, APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, CONTRAST_KEY, ColorScheme, Contrast,
    DBUS_NAME, DBUS_PATH, Settings, config, wayland,
};

#[derive(Clone, Debug)]
pub enum Event {
    Access(crate::access::AccessDialogArgs),
    FileChooser(crate::file_chooser::Args),
    Screenshot(crate::screenshot::Args),
    Screencast(crate::screencast_dialog::Args),
    CancelScreencast(zvariant::ObjectPath<'static>),
    Background(crate::background::Args),
    CancelBackground(cosmic::iced::window::Id),
    Accent(Srgba),
    IsDark(bool),
    HighContrast(bool),
    Config(config::Config),
    Init {
        tx: tokio::sync::mpsc::Sender<Event>,
        tx_conf: tokio::sync::watch::Sender<config::Config>,
        handler: Option<cosmic::cosmic_config::Config>,
    },
    NameLost,
}

pub enum State {
    Init,
    Waiting(
        Connection,
        Receiver<Event>,
        broadcast::Receiver<wayland::Event>,
    ),
}

pub(crate) fn portal_subscription(
    helper: wayland::WaylandHelper,
) -> cosmic::iced::Subscription<Event> {
    struct ConfigSubscription;
    struct Wrapper {
        helper: wayland::WaylandHelper,
    }
    impl Hash for Wrapper {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            std::any::TypeId::of::<wayland::WaylandHelper>().hash(state);
        }
    }
    Subscription::batch([
        Subscription::run_with(Wrapper { helper }, |Wrapper { helper }| {
            let helper = helper.clone();
            cosmic::iced::stream::channel(10, |mut output| async move {
                let mut state = State::Init;
                loop {
                    if let Err(err) = process_changes(&mut state, &mut output, &helper).await {
                        log::debug!("Portal Subscription Error: {:?}", err);
                        future::pending::<()>().await;
                    }
                }
            })
        }),
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
            let (config, handler) = config::Config::load();
            let (tx_conf, rx_conf) = tokio::sync::watch::channel(config);

            let connection = zbus::connection::Builder::session()?
                .serve_at(DBUS_PATH, Access::new(wayland_helper.clone(), tx.clone()))?
                .serve_at(
                    DBUS_PATH,
                    Background::new(wayland_helper.clone(), tx.clone(), rx_conf.clone()),
                )?
                .serve_at(DBUS_PATH, FileChooser::new(tx.clone()))?
                .serve_at(
                    DBUS_PATH,
                    Screenshot::new(wayland_helper.clone(), tx.clone(), rx_conf.clone()),
                )?
                .serve_at(
                    DBUS_PATH,
                    ScreenCast::new(wayland_helper.clone(), tx.clone()),
                )?
                .serve_at(DBUS_PATH, Settings::new())?
                .build()
                .await?;

            // Create name lost stream before requesting name
            let dbus = fdo::DBusProxy::new(&connection).await?;
            tokio::spawn(name_lost_task(
                dbus.receive_name_lost().await?,
                output.clone(),
            ));

            connection.request_name(DBUS_NAME).await?;

            let wl_rx = wayland_helper.subscribe();
            _ = output
                .send(Event::Init {
                    tx,
                    tx_conf,
                    handler,
                })
                .await;
            *state = State::Waiting(connection, rx, wl_rx);
        }
        State::Waiting(conn, rx, wl_rx) => {
            loop {
                let event = tokio::select! {
                    event = rx.recv() => match event {
                        Some(event) => event,
                        None => break,
                    },
                    wl_event = wl_rx.recv() => {
                        match wl_event {
                            Ok(wayland::Event::ToplevelsUpdated) => {
                                // Coalesce a burst of updates (e.g. the initial toplevel
                                // enumeration) into a single signal.
                                while !matches!(
                                    wl_rx.try_recv(),
                                    Err(broadcast::error::TryRecvError::Empty
                                        | broadcast::error::TryRecvError::Closed)
                                ) {}
                                emit_running_applications_changed(conn).await?;
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                        continue;
                    }
                };
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
                    Event::CancelScreencast(handle) => {
                        if let Err(err) = output.send(Event::CancelScreencast(handle)).await {
                            log::error!("Error sending screencast cancel: {:?}", err);
                        };
                    }
                    Event::Background(args) => {
                        if let Err(err) = output.send(Event::Background(args)).await {
                            log::error!("Error sending background event: {:?}", err);
                        }
                    }
                    Event::CancelBackground(id) => {
                        if let Err(err) = output.send(Event::CancelBackground(id)).await {
                            log::error!("Error sending background cancel: {:?}", err);
                        }
                    }
                    Event::Accent(a) => {
                        let object_server = conn.object_server();
                        let iface_ref = object_server.interface::<_, Settings>(DBUS_PATH).await?;
                        let mut iface = iface_ref.get_mut().await;
                        iface.accent = a.into_format();
                        iface
                            .setting_changed(
                                iface_ref.signal_emitter(),
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
                                iface_ref.signal_emitter(),
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
                                iface_ref.signal_emitter(),
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
                    Event::Init { .. } => {}
                    Event::NameLost => {}
                }
            }

            // The loop only exits at shutdown once all senders drop. Park instead of
            // returning so the outer subscription loop doesn't busy-loop back into Init.
            future::pending::<()>().await;
        }
    };
    Ok(())
}

/// Emit the Background portal's `RunningApplicationsChanged` signal.
async fn emit_running_applications_changed(conn: &Connection) -> anyhow::Result<()> {
    let background = conn
        .object_server()
        .interface::<_, Background>(DBUS_PATH)
        .await
        .context("Connecting to Background portal D-Bus interface")?;
    Background::running_applications_changed(background.signal_emitter())
        .await
        .context("Emitting RunningApplicationsChanged for the Background portal")
}

async fn name_lost_task(
    mut name_lost_stream: fdo::NameLostStream,
    mut output: futures::channel::mpsc::Sender<Event>,
) {
    while let Some(name_lost) = name_lost_stream.next().await {
        let Ok(args) = name_lost.args() else {
            return;
        };
        if args.name == DBUS_NAME {
            let _ = output.send(Event::NameLost).await;
        }
    }
}
