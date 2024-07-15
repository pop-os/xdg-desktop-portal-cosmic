use cosmic::iced::{window, Limits};

use crate::app::{CosmicPortal, OutputState};

#[derive(Clone)]
enum Msg {
}

pub fn update_msg(portal: &mut CosmicPortal, msg: Msg) -> cosmic::Command<crate::app::Msg> {
    todo!()
}

pub(crate) fn view(portal: &CosmicPortal, id: window::Id) -> cosmic::Element<Msg> {
    // WIP
    cosmic::widget::dialog("Screencast").into()
}
