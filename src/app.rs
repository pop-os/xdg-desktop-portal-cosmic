use crate::{access, fl, screenshot, subscription};
use cosmic::iced_core::event::wayland::OutputEvent;
use cosmic::widget::dropdown;
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

    pub access_args: Option<access::AccessDialogArgs>,
    pub access_choices: Vec<(Option<usize>, Vec<String>)>,

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
                screenshot_args: Default::default(),
                location_options: Vec::new(),
                prev_rectangle: Default::default(),
                outputs: Default::default(),
                active_output: Default::default(),
                wayland_helper,
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
            panic!("Unknown window id {:?}", id);
        }
    }

    fn update(
        &mut self,
        message: Self::Message,
    ) -> cosmic::iced::Command<app::Message<Self::Message>> {
        match message {
            Msg::Access(m) => access::update_msg(self, m).map(cosmic::app::Message::App),
            Msg::Portal(e) => match e {
                subscription::Event::Access(args) => {
                    access::update_args(self, args).map(cosmic::app::Message::App)
                }
                subscription::Event::Screenshot(args) => {
                    screenshot::update_args(self, args).map(cosmic::app::Message::App)
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

    fn subscription(&self) -> cosmic::iced_futures::Subscription<Self::Message> {
        Subscription::batch(vec![
            subscription::portal_subscription(self.wayland_helper.clone()).map(|e| Msg::Portal(e)),
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
                        key_code: cosmic::iced_core::keyboard::KeyCode::Escape,
                        ..
                    },
                ) => Some(Msg::Screenshot(screenshot::Msg::Cancel)),
                _ => None,
            }),
        ])
    }
}
