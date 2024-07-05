// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Screenshot {
    pub save_location: ImageSaveLocation,
    pub choice: Choice,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum ImageSaveLocation {
    Clipboard,
    #[default]
    Pictures,
    Documents,
    // Custom(PathBuf), // TODO
}

// TODO: Use type from screenshot directly?
#[derive(Debug, Default, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub enum Choice {
    #[default]
    Output,
    Rectangle,
    Window,
}
