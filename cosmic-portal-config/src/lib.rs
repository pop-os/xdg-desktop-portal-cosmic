// SPDX-License-Identifier: GPL-3.0-only

pub mod background;
pub mod screenshot;

use cosmic_config::{cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};
use serde::{Deserialize, Serialize};

use background::Background;
use screenshot::Screenshot;

pub const APP_ID: &str = "com.system76.CosmicPortal";
pub const CONFIG_VERSION: u64 = 1;

#[derive(Debug, Clone, Default, PartialEq, CosmicConfigEntry, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[version = 1]
#[id = "com.system76.CosmicPortal"]
pub struct Config {
    /// Interactive screenshot settings
    pub screenshot: Screenshot,
    /// Background portal settings
    pub background: Background,
}

impl Config {
    pub fn load() -> (Self, Option<cosmic_config::Config>) {
        match cosmic_config::Config::new(APP_ID, CONFIG_VERSION) {
            Ok(handler) => {
                let config = Config::get_entry(&handler)
                    .inspect_err(|(errors, _)| {
                        for err in errors {
                            log::error!("{err}")
                        }
                    })
                    .unwrap_or_else(|(_, config)| config);
                (config, Some(handler))
            }
            Err(e) => {
                log::error!("Failed to get settings for `{APP_ID}` (v {CONFIG_VERSION}): {e}");
                (Config::default(), None)
            }
        }
    }
}
