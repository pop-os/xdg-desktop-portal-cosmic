use crate::{access, file_chooser, fl, screenshot, subscription};
use cosmic::iced::keyboard;
use cosmic::iced_core::event::wayland::OutputEvent;
use cosmic::iced_core::keyboard::key::Named;
use cosmic::widget::dropdown;
use cosmic::Command;
use cosmic::{
    app,
    iced::window,
    iced_futures::{event::listen_with, Subscription},
};
use wayland_client::protocol::wl_output::WlOutput;

pub(crate) fn run() -> cosmic::iced::Result {
    let settings = cosmic::app::Settings::default()
        .no_main_window(true)
        .exit_on_close(false);
    cosmic::app::run::<CosmicPortal>(settings, ())
}

// run iced app with no main surface
pub struct CosmicPortal {
    pub core: app::Core,
    pub tx: Option<tokio::sync::mpsc::Sender<subscription::Event>>,

    pub access_args: Option<access::AccessDialogArgs>,
    pub access_choices: Vec<(Option<usize>, Vec<String>)>,

    pub file_chooser_args: Option<file_chooser::Args>,
    pub file_chooser_dialog: Option<file_chooser::Dialog>,

    pub screenshot_args: Option<screenshot::Args>,
    pub location_options: Vec<String>,
    pub prev_rectangle: Option<screenshot::Rect>,
    pub wayland_helper: crate::wayland::WaylandHelper,

    pub outputs: Vec<OutputState>,
    pub active_output: Option<WlOutput>,
}

#[derive(Debug, Clone)]
pub struct OutputState {
    pub output: WlOutput,
    pub id: window::Id,
    pub name: String,
    pub logical_size: (u32, u32),
    pub logical_pos: (i32, i32),
    pub has_pointer: bool,
    pub bg_source: Option<cosmic_bg_config::Source>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Access(access::Msg),
    FileChooser(file_chooser::Msg),
    Screenshot(screenshot::Msg),
    Portal(subscription::Event),
    Output(OutputEvent, WlOutput),
}

impl cosmic::Application for CosmicPortal {
    type Executor = cosmic::executor::Default;

    type Flags = ();

    type Message = Msg;

    const APP_ID: &'static str = "org.freedesktop.portal.desktop.cosmic";

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(
        core: app::Core,
        _flags: Self::Flags,
    ) -> (Self, cosmic::iced::Command<app::Message<Self::Message>>) {
        let mut model = cosmic::widget::dropdown::multi::model();
        model.insert(dropdown::multi::list(
            Some(fl!("save-to")),
            vec![
                (
                    fl!("save-to", "clipboard"),
                    screenshot::ImageSaveLocation::Clipboard,
                ),
                (
                    fl!("save-to", "pictures"),
                    screenshot::ImageSaveLocation::Pictures,
                ),
                (
                    fl!("save-to", "documents"),
                    screenshot::ImageSaveLocation::Documents,
                ),
            ],
        ));
        model.selected = Some(screenshot::ImageSaveLocation::default());
        let wayland_conn = crate::wayland::connect_to_wayland();
        let wayland_helper = crate::wayland::WaylandHelper::new(wayland_conn);
        (
            Self {
                core,
                access_args: Default::default(),
                access_choices: Default::default(),
                file_chooser_args: Default::default(),
                file_chooser_dialog: Default::default(),
                screenshot_args: Default::default(),
                location_options: Vec::new(),
                prev_rectangle: Default::default(),
                outputs: Default::default(),
                active_output: Default::default(),
                wayland_helper,
                tx: None,
            },
            cosmic::iced::Command::none(),
        )
    }

    fn view(&self) -> cosmic::prelude::Element<Self::Message> {
        unimplemented!()
    }

    fn view_window(&self, id: window::Id) -> cosmic::prelude::Element<Self::Message> {
        if id == *access::ACCESS_ID {
            access::view(self).map(Msg::Access)
        } else if self.outputs.iter().any(|o| o.id == id) {
            screenshot::view(self, id).map(Msg::Screenshot)
        } else {
            file_chooser::view(self, id)
        }
    }

    fn update(
        &mut self,
        message: Self::Message,
    ) -> cosmic::iced::Command<app::Message<Self::Message>> {
        match message {
            Msg::Access(m) => access::update_msg(self, m).map(cosmic::app::Message::App),
            Msg::FileChooser(m) => file_chooser::update_msg(self, m),
            Msg::Portal(e) => match e {
                subscription::Event::Access(args) => {
                    access::update_args(self, args).map(cosmic::app::Message::App)
                }
                subscription::Event::FileChooser(args) => file_chooser::update_args(self, args),
                subscription::Event::Screenshot(args) => {
                    screenshot::update_args(self, args).map(cosmic::app::Message::App)
                }
                subscription::Event::Accent(_)
                | subscription::Event::IsDark(_)
                | subscription::Event::HighContrast(_) => cosmic::iced::Command::none(),
                subscription::Event::Init(tx) => {
                    self.tx = Some(tx);
                    Command::none()
                }
            },
            Msg::Screenshot(m) => screenshot::update_msg(self, m).map(cosmic::app::Message::App),
            Msg::Output(o_event, wl_output) => {
                match o_event {
                    OutputEvent::Created(Some(info))
                        if info.name.is_some()
                            && info.logical_size.is_some()
                            && info.logical_position.is_some() =>
                    {
                        self.outputs.push(OutputState {
                            output: wl_output,
                            id: window::Id::unique(),
                            name: info.name.unwrap(),
                            logical_size: info
                                .logical_size
                                .map(|(w, h)| (w as u32, h as u32))
                                .unwrap(),
                            logical_pos: info.logical_position.unwrap(),
                            has_pointer: false,
                            bg_source: None,
                        })
                    }
                    OutputEvent::Removed => self.outputs.retain(|o| o.output != wl_output),
                    OutputEvent::InfoUpdate(info)
                        if info.name.is_some()
                            && info.logical_size.is_some()
                            && info.logical_position.is_some() =>
                    {
                        if let Some(state) = self.outputs.iter_mut().find(|o| o.output == wl_output)
                        {
                            state.name = info.name.unwrap();
                            state.logical_size = info
                                .logical_size
                                .map(|(w, h)| (w as u32, h as u32))
                                .unwrap();
                            state.logical_pos = info.logical_position.unwrap();
                        } else {
                            log::warn!("Updated output {:?} not found", wl_output);
                            self.outputs.push(OutputState {
                                output: wl_output,
                                id: window::Id::unique(),
                                name: info.name.unwrap(),
                                logical_size: info
                                    .logical_size
                                    .map(|(w, h)| (w as u32, h as u32))
                                    .unwrap(),
                                logical_pos: info.logical_position.unwrap(),
                                has_pointer: false,
                                bg_source: None,
                            });
                        }
                    }
                    e => {
                        log::warn!("Unhandled output event: {:?} {e:?}", wl_output);
                    }
                };

                if self.prev_rectangle.is_none() {
                    let mut rect = screenshot::Rect::default();
                    for output in &self.outputs {
                        rect.left = rect.left.min(output.logical_pos.0);
                        rect.top = rect.top.min(output.logical_pos.1);
                        rect.right = rect
                            .right
                            .max(output.logical_pos.0 + output.logical_size.0 as i32);
                        rect.bottom = rect
                            .bottom
                            .max(output.logical_pos.1 + output.logical_size.1 as i32);
                    }
                    self.prev_rectangle = Some(rect);
                }

                cosmic::iced::Command::none()
            }
        }
    }

    #[allow(clippy::collapsible_match)]
    fn subscription(&self) -> cosmic::iced_futures::Subscription<Self::Message> {
        Subscription::batch(vec![
            subscription::portal_subscription(self.wayland_helper.clone()).map(Msg::Portal),
            listen_with(|e, _| match e {
                cosmic::iced_core::Event::PlatformSpecific(
                    cosmic::iced_core::event::PlatformSpecific::Wayland(w_e),
                ) => match w_e {
                    cosmic::iced_core::event::wayland::Event::Output(o_event, wl_output) => {
                        Some(Msg::Output(o_event, wl_output))
                    }
                    _ => None,
                },
                cosmic::iced_core::Event::Keyboard(
                    cosmic::iced_core::keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(Named::Escape),
                        ..
                    },
                ) => Some(Msg::Screenshot(screenshot::Msg::Cancel)),
                _ => None,
            }),
        ])
    }

    fn system_theme_mode_update(
        &mut self,
        _keys: &[&'static str],
        new_theme: &cosmic::cosmic_theme::ThemeMode,
    ) -> app::Command<Self::Message> {
        let old = self.core.system_is_dark();
        let new = new_theme.is_dark;
        if new != old {
            if let Some(tx) = self.tx.clone() {
                tokio::spawn(async move {
                    _ = tx.send(subscription::Event::IsDark(new)).await;
                });
            }
        }
        Command::none()
    }

    fn system_theme_update(
        &mut self,
        _keys: &[&'static str],
        new_theme: &cosmic::cosmic_theme::Theme,
    ) -> cosmic::iced::Command<app::Message<Self::Message>> {
        let old = self.core.system_theme().cosmic();
        let mut msgs = Vec::with_capacity(3);

        if old.is_dark != new_theme.is_dark {
            return Command::none();
        }

        if old.accent_color() != new_theme.accent_color() {
            msgs.push(subscription::Event::Accent(new_theme.accent_color()));
        }
        if old.is_high_contrast != new_theme.is_high_contrast {
            msgs.push(subscription::Event::HighContrast(
                new_theme.is_high_contrast,
            ));
        }
        {
            if let Some(tx) = self.tx.clone() {
                tokio::spawn(async move {
                    for msg in msgs {
                        _ = tx.send(msg).await;
                    }
                });
            }
        }
        Command::none()
    }
}
