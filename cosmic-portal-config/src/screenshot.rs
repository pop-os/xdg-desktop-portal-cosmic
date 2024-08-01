// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Screenshot {
    pub save_location: ImageSaveLocation,
    pub choice: Choice,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub enum ImageSaveLocation {
    Clipboard,
    #[default]
    Pictures,
    Documents,
    // Custom(PathBuf), // TODO
}

// TODO: Use type from screenshot directly?
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub enum Choice {
    Output(Option<String>),
    Rectangle,
    Window,
}

impl From<&mut Choice> for Choice {
    fn from(value: &mut Choice) -> Self {
        // Convenience implementation to move Choice so that the borrow checker doesn't complain
        // about partial moves
        match value {
            Choice::Output(output) => Choice::Output(output.take()),
            Choice::Rectangle => Choice::Rectangle,
            Choice::Window => Choice::Window,
        }
    }
}

impl Default for Choice {
    fn default() -> Self {
        Choice::Output(None)
    }
}
