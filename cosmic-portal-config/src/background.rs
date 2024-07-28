// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Background {
    /// Default preference for NotifyBackground's dialog
    pub default_perm: PermissionDialog,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum PermissionDialog {
    /// Grant apps permission to run in the background
    Allow,
    /// Deny apps permission to run in the background
    Deny,
    /// Always ask if new apps should be granted background permissions
    #[default]
    Ask,
}
