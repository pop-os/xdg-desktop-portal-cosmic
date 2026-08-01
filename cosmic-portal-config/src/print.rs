use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrintPreset {
    pub id: String,
    pub name: String,
    pub is_builtin: bool,
    pub color_mode: String,
    pub orientation: String,
    pub duplex_index: Option<usize>,
    pub copies: u32,
    pub collate: bool,
    pub pages_per_sheet_index: Option<usize>,
    pub layout_direction: String,
    pub margins: String,
    pub scaling: String,
    pub custom_scaling_input: u32,
    #[serde(default = "default_page_selection")]
    pub page_selection: String,
    #[serde(default)]
    pub custom_range_input: String,
}

impl PrintPreset {
    fn builtin(id: &str, name: &str, color_mode: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            is_builtin: true,
            color_mode: color_mode.to_string(),
            orientation: "Portrait".to_string(),
            duplex_index: Some(0),
            copies: 1,
            collate: false,
            pages_per_sheet_index: Some(0),
            layout_direction: "LRTB".to_string(),
            margins: "Default".to_string(),
            scaling: "Auto".to_string(),
            custom_scaling_input: 100,
            page_selection: default_page_selection(),
            custom_range_input: String::new(),
        }
    }

    pub fn default_preset() -> Self {
        Self::builtin("builtin-default", "Default", "Color")
    }

    pub fn color_preset() -> Self {
        Self::builtin("builtin-color", "Color", "Color")
    }

    pub fn bw_preset() -> Self {
        Self::builtin("builtin-bw", "Black and White", "Monochrome")
    }

    pub const BUILTIN_PRESETS: [fn() -> Self; 3] =
        [Self::default_preset, Self::bw_preset, Self::color_preset];
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Print {
    pub custom_presets: Vec<PrintPreset>,
    pub last_used_preset_id: Option<String>,
}

impl Print {
    pub fn all_presets(&self) -> Vec<PrintPreset> {
        let mut list: Vec<PrintPreset> = PrintPreset::BUILTIN_PRESETS.iter().map(|f| f()).collect();
        list.extend(self.custom_presets.clone());
        list
    }
}

fn default_page_selection() -> String {
    "All".to_string()
}
