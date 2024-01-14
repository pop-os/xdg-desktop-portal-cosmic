use cosmic_client_toolkit::{
    cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1,
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    wayland_client::{Connection, QueueHandle},
};

use super::AppData;

// TODO any indication when we have all toplevels?
impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }

    fn new_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _toplevel: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    ) {
        self.update_output_toplevels()
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _toplevel: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    ) {
        self.update_output_toplevels()
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _toplevel: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    ) {
        self.update_output_toplevels()
    }
}

cosmic_client_toolkit::delegate_toplevel_info!(AppData);
