// contains the subscription which sends portal events and response channels to iced.

use std::any::TypeId;

use anyhow::Context;
use cosmic::{cosmic_theme::palette::Srgba, iced::Subscription};
use futures::{future, SinkExt};
use tokio::sync::mpsc::Receiver;
use zbus::{zvariant, Connection};

use crate::{
    access::Access, background::Background, config, file_chooser::FileChooser,
    screencast::ScreenCast, screenshot::Screenshot, wayland, ColorScheme, Contrast, Settings,
    ACCENT_COLOR_KEY, APPEARANCE_NAMESPACE, COLOR_SCHEME_KEY, CONTRAST_KEY, DBUS_NAME, DBUS_PATH,
};

#[derive(Clone, Debug)]
pub enum Event {
    Access(crate::access::AccessDialogArgs),
    FileChooser(crate::file_chooser::Args),
    Screenshot(crate::screenshot::Args),
    Screencast(crate::screencast_dialog::Args),
    CancelScreencast(zvariant::ObjectPath<'static>),
    Background(crate::background::Args),
    BackgroundToplevels,
    Accent(Srgba),
    IsDark(bool),
    HighContrast(bool),
    Config(config::Config),
    Init {
        tx: tokio::sync::mpsc::Sender<Event>,
        tx_conf: tokio::sync::watch::Sender<config::Config>,
        handler: Option<cosmic::cosmic_config::Config>,
    },
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
    struct WaylandHelperSubscription;
    let helper_portal = helper.clone();
    Subscription::batch([
        Subscription::run_with_id(
            TypeId::of::<PortalSubscription>(),
            cosmic::iced_futures::stream::channel(10, |mut output| async move {
                let mut state = State::Init;
                loop {
                    if let Err(err) = process_changes(&mut state, &mut output, &helper_portal).await
                    {
                        log::debug!("Portal Subscription Error: {:?}", err);
                        future::pending::<()>().await;
                    }
                }
            }),
        ),
        Subscription::run_with_id(
            TypeId::of::<WaylandHelperSubscription>(),
            helper.subscription(),
        )
        .map(|wl_event| match wl_event {
            wayland::Event::ToplevelsUpdated => Event::BackgroundToplevels,
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
                .name(DBUS_NAME)?
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
            _ = output
                .send(Event::Init {
                    tx,
                    tx_conf,
                    handler,
                })
                .await;
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
                    Event::BackgroundToplevels => {
                        log::debug!(
                            "Emitting RunningApplicationsChanged in response to toplevel updates"
                        );
                        let background = conn
                            .object_server()
                            .interface::<_, Background>(DBUS_PATH)
                            .await
                            .context("Connecting to Background portal D-Bus interface")?;
                        Background::running_applications_changed(background.signal_emitter())
                            .await
                            .context(
                                "Emitting RunningApplicationsChanged for the Background portal",
                            )?;
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
                }
            }
        }
    };
    Ok(())
}
