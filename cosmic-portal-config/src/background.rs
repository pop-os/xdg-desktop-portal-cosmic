// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Background {
    /// App ID and allowed status
    pub apps: HashMap<String, bool>,
    /// Default preference for NotifyBackground's dialog
    pub default_perm: PermissionDialog,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum PermissionDialog {
    /// Grant apps permission to run in the background
    Allow,
    /// Deny apps permission to run in the background
    Deny,
    /// Always ask if new apps should be granted background permissions
    #[default]
    Ask,
}
