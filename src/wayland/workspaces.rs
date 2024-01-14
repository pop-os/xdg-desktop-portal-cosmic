use cosmic_client_toolkit::workspace::{WorkspaceHandler, WorkspaceState};

use super::AppData;

impl WorkspaceHandler for AppData {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }

    fn done(&mut self) {
        self.update_output_toplevels()
    }
}

cosmic_client_toolkit::delegate_workspace!(AppData);
