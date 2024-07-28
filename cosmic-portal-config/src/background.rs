// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Background {
    /// App ID and allowed status
    pub apps: HashMap<String, bool>,
}
