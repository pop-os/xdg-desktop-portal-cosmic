use ashpd::desktop::file_chooser;
use cosmic::iced::font::Weight;
use cosmic::iced::widget::Stack;
use cosmic::iced::{Alignment, Background, Color, Font, Length, Padding, Subscription};
use cosmic::theme::{Button, SegmentedButton, Text};
use cosmic::widget::dropdown::popup_dropdown;
use cosmic::widget::segmented_button::SingleSelect;
use cosmic::widget::{
    self, button, column, container, divider, icon, row, segmented_button, space, text, text_input,
    toggler,
};
use cosmic::{Element, Task, font, theme};
use cpdb_rs::client::CpdbClient;
use cpdb_rs::media::MediaCollection;
use cpdb_rs::options::{OptionInfo, OptionsCollection};
use cpdb_rs::types::PrinterState;
use cpdb_rs::{DiscoveryEvent, MediaInfo, PrinterSnapshot};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::copy;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use zbus::zvariant;

use crate::app::CosmicPortal;
use crate::print::PrintResult;
use crate::{PortalResponse, fl};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrinterDiscovery;

impl Hash for PrinterDiscovery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::any::TypeId::of::<Self>().hash(state);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActiveView {
    Main,
    PageSelection,
    SavePreset,
    EditPresets,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PageSetSelection {
    All,
    Current,
    Odd,
    Even,
    Custom(String),
}

impl PageSetSelection {
    pub fn id(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Current => "current",
            Self::Odd => "odd",
            Self::Even => "even",
            Self::Custom(_) => "custom",
        }
    }

    pub fn from_id(id: &str, custom_input: &str) -> Self {
        match id {
            "current" => Self::Current,
            "odd" => Self::Odd,
            "even" => Self::Even,
            "custom" => Self::Custom(custom_input.to_string()),
            _ => Self::All,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::All => fl!("page-set-all"),
            Self::Current => fl!("page-set-current"),
            Self::Odd => fl!("page-set-odd"),
            Self::Even => fl!("page-set-even"),
            Self::Custom(_) => fl!("page-set-custom"),
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Custom(val) if !val.trim().is_empty() => {
                fl!("page-set-custom-value", range = val.as_str())
            }
            other => other.label(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MarginOptions {
    pub top: Vec<u32>,
    pub bottom: Vec<u32>,
    pub left: Vec<u32>,
    pub right: Vec<u32>,
    /// True when all four sides include 0
    pub supports_borderless: bool,
}

impl MarginOptions {
    fn parse_side(opt: Option<&OptionInfo>) -> Vec<u32> {
        opt.map(|o| {
            let mut vals: Vec<u32> = o
                .supported_values
                .iter()
                .filter_map(|v| v.parse().ok())
                .collect();
            vals.sort_unstable();
            vals.dedup();
            vals
        })
        .unwrap_or_default()
    }

    pub fn from_options(opts: &OptionsCollection) -> Self {
        let top = Self::parse_side(opts.get("media-top-margin"));
        let bottom = Self::parse_side(opts.get("media-bottom-margin"));
        let left = Self::parse_side(opts.get("media-left-margin"));
        let right = Self::parse_side(opts.get("media-right-margin"));
        let supports_borderless = [&top, &bottom, &left, &right]
            .iter()
            .all(|v| v.contains(&0));
        Self {
            top,
            bottom,
            left,
            right,
            supports_borderless,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PrintDialog {
    pub window_id: cosmic::iced::window::Id,
    pub is_discovering: bool,
    pub printers: Vec<PrinterSnapshot>,
    pub selected_printer_index: Option<usize>,
    pub printer_options: Option<OptionsCollection>,
    pub printer_media: Option<MediaCollection>,
    pub translations: HashMap<String, String>,

    pub active_view: ActiveView,
    pub page_selection: PageSetSelection,
    pub custom_range_input: String,
    pub custom_range_valid: bool,

    pub copies: u32,
    pub collate: bool,
    pub selected_paper_size_index: Option<usize>,

    pub color_supported: bool,
    pub duplex_values: Vec<String>,
    pub duplex_index: Option<usize>,
    pub media_source_values: Vec<String>,
    pub paper_tray_index: Option<usize>,
    pub media_type_values: Vec<String>,
    pub paper_type_index: Option<usize>,
    pub print_quality_values: Vec<String>,
    pub print_quality_index: Option<usize>,

    // Primary togglers
    pub color_mode: ColorMode,
    pub orientation: Orientation,

    // Layout
    pub pages_per_sheet_index: Option<usize>,
    pub layout_direction: LayoutDirection,
    pub margins: Margins,
    pub margin_options: MarginOptions,
    pub custom_margins_vertical_index: Option<usize>,
    pub custom_margins_horizontal_index: Option<usize>,
    pub border: Border,
    pub scaling: ScalingMode,
    pub custom_scaling_input: u32,
    pub show_print_header_footer_toggle: bool,
    pub print_header_footer: bool,
    pub show_print_background_toggle: bool,
    pub print_background: bool,

    // Paper handling
    pub reverse_order: bool,
    pub accept_label: Option<String>,

    // Presets
    pub presets: Vec<cosmic_portal_config::print::PrintPreset>,
    pub selected_preset_index: Option<usize>,
    pub save_preset_name_input: String,
    pub editing_preset_row: Option<usize>,
    pub editing_preset_name_input: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Color,
    Monochrome,
}

impl ColorMode {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Monochrome => "monochrome",
        }
    }

    pub fn from_id(s: &str) -> Self {
        match s {
            "monochrome" => Self::Monochrome,
            _ => Self::Color,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Color => fl!("color-mode-color"),
            Self::Monochrome => fl!("color-mode-monochrome"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

impl Orientation {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::Landscape => "landscape",
        }
    }

    pub fn from_id(s: &str) -> Self {
        match s {
            "landscape" => Self::Landscape,
            _ => Self::Portrait,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Portrait => fl!("orientation-portrait"),
            Self::Landscape => fl!("orientation-landscape"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutDirection {
    LeftToRightTopToBottom,
    RightToLeftTopToBottom,
    TopToBottomLeftToRight,
    TopToBottomRightToLeft,
}

impl LayoutDirection {
    pub fn id(&self) -> &'static str {
        match self {
            Self::LeftToRightTopToBottom => "lrtb",
            Self::RightToLeftTopToBottom => "rltb",
            Self::TopToBottomLeftToRight => "tblr",
            Self::TopToBottomRightToLeft => "tbrl",
        }
    }

    pub fn from_id(s: &str) -> Self {
        match s {
            "rltb" => Self::RightToLeftTopToBottom,
            "tblr" => Self::TopToBottomLeftToRight,
            "tbrl" => Self::TopToBottomRightToLeft,
            _ => Self::LeftToRightTopToBottom,
        }
    }

    pub fn icon_name(&self) -> &'static str {
        match self {
            Self::LeftToRightTopToBottom => "left-to-right-symbolic",
            Self::RightToLeftTopToBottom => "right-to-left-symbolic",
            Self::TopToBottomLeftToRight => "top-bottom-right-symbolic",
            Self::TopToBottomRightToLeft => "top-bottom-left-symbolic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Margins {
    Default,
    None,
    Minimum,
    Custom,
}

impl Margins {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::None => "none",
            Self::Minimum => "minimum",
            Self::Custom => "custom",
        }
    }

    pub fn from_id(s: &str) -> Self {
        match s {
            "none" => Self::None,
            "minimum" => Self::Minimum,
            "custom" => Self::Custom,
            _ => Self::Default,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Default => fl!("margins-default"),
            Self::None => fl!("margins-none"),
            Self::Minimum => fl!("margins-minimum"),
            Self::Custom => fl!("margins-custom"),
        }
    }

    pub const ALL: [Self; 4] = [Self::Default, Self::None, Self::Minimum, Self::Custom];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Border {
    None,
    Single,
    Double,
}

impl Border {
    pub fn id(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Single => "single",
            Self::Double => "double",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::None => fl!("border-none"),
            Self::Single => fl!("border-single"),
            Self::Double => fl!("border-double"),
        }
    }

    pub const ALL: [Self; 3] = [Self::None, Self::Single, Self::Double];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingMode {
    Auto,
    AutoFit,
    Fit,
    Fill,
    Custom,
}

impl ScalingMode {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::AutoFit => "auto-fit",
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Custom => "custom",
        }
    }

    pub fn from_id(s: &str) -> Self {
        match s {
            "fit" => Self::Fit,
            "auto-fit" => Self::AutoFit,
            "fill" => Self::Fill,
            "custom" => Self::Custom,
            _ => Self::Auto,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Auto => fl!("scaling-auto"),
            Self::AutoFit => fl!("scaling-auto-fit"),
            Self::Fit => fl!("scaling-fit"),
            Self::Fill => fl!("scaling-fill"),
            Self::Custom => fl!("scaling-custom"),
        }
    }

    pub fn as_cpdb_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::AutoFit => "auto-fit",
            Self::Fit => "fit",
            Self::Fill => "fill",
            Self::Custom => "none",
        }
    }

    pub const ALL: [Self; 5] = [
        Self::Auto,
        Self::AutoFit,
        Self::Fit,
        Self::Fill,
        Self::Custom,
    ];
}

pub fn is_pdf_printer(printer: &PrinterSnapshot) -> bool {
    printer.id == "save-as-pdf" || printer.backend == "file"
}

pub fn is_pdf_printer_by_id(printer_id: &str, backend: &str) -> bool {
    printer_id == "save-as-pdf" || backend == "file"
}

pub fn save_as_pdf_printer() -> PrinterSnapshot {
    PrinterSnapshot {
        id: "save-as-pdf".to_string(),
        name: fl!("save-as-pdf"),
        info: fl!("save-output-to-pdf"),
        location: String::new(),
        make_model: "PDF Converter".to_string(),
        state: PrinterState::Idle,
        accepting_jobs: true,
        backend: "file".to_string(),
    }
}

pub fn default_pdf_media() -> MediaCollection {
    MediaCollection {
        media: vec![
            MediaInfo {
                name: "iso_a4_210x297mm".to_string(),
                width: 21000,
                length: 29700,
                margins: vec![],
            },
            MediaInfo {
                name: "na_letter_8.5x11in".to_string(),
                width: 21590,
                length: 27940,
                margins: vec![],
            },
            MediaInfo {
                name: "na_legal_8.5x14in".to_string(),
                width: 21590,
                length: 35560,
                margins: vec![],
            },
            MediaInfo {
                name: "iso_a3_297x420mm".to_string(),
                width: 29700,
                length: 42000,
                margins: vec![],
            },
            MediaInfo {
                name: "iso_a5_148x210mm".to_string(),
                width: 14800,
                length: 21000,
                margins: vec![],
            },
        ],
    }
}

impl Default for PrintDialog {
    fn default() -> Self {
        Self {
            window_id: cosmic::iced::window::Id::unique(),
            is_discovering: true,
            printers: vec![save_as_pdf_printer()],
            selected_printer_index: Some(0),
            printer_options: None,
            printer_media: Some(default_pdf_media()),
            translations: HashMap::new(),
            active_view: ActiveView::Main,
            page_selection: PageSetSelection::All,
            custom_range_input: String::new(),
            custom_range_valid: true,
            copies: 1,
            collate: false,
            selected_paper_size_index: Some(0),
            color_supported: true,
            duplex_values: Vec::new(),
            duplex_index: None,
            media_source_values: Vec::new(),
            paper_tray_index: None,
            media_type_values: Vec::new(),
            paper_type_index: None,
            print_quality_values: Vec::new(),
            print_quality_index: None,
            color_mode: ColorMode::Color,
            orientation: Orientation::Portrait,
            pages_per_sheet_index: Some(0),
            layout_direction: LayoutDirection::LeftToRightTopToBottom,
            margins: Margins::Default,
            margin_options: MarginOptions {
                top: vec![0, 500, 1000],
                bottom: vec![0, 500, 1000],
                left: vec![0, 500, 1000],
                right: vec![0, 500, 1000],
                supports_borderless: true,
            },
            custom_margins_vertical_index: Some(0),
            custom_margins_horizontal_index: Some(0),
            border: Border::None,
            scaling: ScalingMode::Auto,
            custom_scaling_input: 100,
            show_print_header_footer_toggle: false,
            print_header_footer: false,
            show_print_background_toggle: false,
            print_background: false,
            reverse_order: false,
            accept_label: None,
            presets: cosmic_portal_config::print::Print::default().all_presets(),
            selected_preset_index: Some(0),
            save_preset_name_input: String::new(),
            editing_preset_row: None,
            editing_preset_name_input: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    SurfaceAction(cosmic::surface::Action),
    PrintersLoaded(Vec<PrinterSnapshot>),
    DiscoveryEvent(DiscoveryEvent),
    PrinterSelected(usize),
    PrinterDetailsLoaded(OptionsCollection, MediaCollection, HashMap<String, String>),

    // PageSelection view navigation
    NavigateTo(ActiveView),
    PageSelectionModelActivated(segmented_button::Entity),
    CustomRangeInputChanged(String),

    // Options updates
    ColorModelActivated(segmented_button::Entity),
    OrientationModelActivated(segmented_button::Entity),
    IncrementCopies,
    DecrementCopies,
    ToggleCollate,
    PaperSizeSelected(usize),
    DuplexSelected(usize),

    // Layout
    PagesPerSheetSelected(usize),
    LayoutDirectionModelActivated(segmented_button::Entity),
    MarginsSelected(Margins),
    CustomMarginVtSelected(usize),
    CustomMarginHzSelected(usize),
    BorderSelected(Border),
    ScalingSelected(ScalingMode),
    IncrementScaling,
    DecrementScaling,
    TogglePrintHeaderFooter,
    TogglePrintBackground,

    // Paper handling
    ToggleReverseOrder,
    PaperTraySelected(usize),
    PaperTypeSelected(usize),
    PrintQualitySelected(usize),

    // Buttons
    Cancel,
    Confirm,
    EnterPressed,
    EscapePressed,

    // Presets
    PresetSelected(usize),
    OpenSavePresetDialog,
    SavePresetNameInputChanged(String),
    ConfirmSavePreset,
    OpenEditPresetsDialog,
    ToggleEditPresetRow(usize),
    PresetRowNameInputChanged(String),
    SavePresetRowName(usize),
    DeletePresetRow(usize),
    AddNewPresetInEditor,
    CommitEdit,
}

impl PrintDialog {
    pub fn is_pdf_selected(&self) -> bool {
        self.selected_printer_index
            .and_then(|i| self.printers.get(i))
            .map(is_pdf_printer)
            .unwrap_or(false)
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        if self.is_discovering {
            Subscription::run_with(PrinterDiscovery, |_| {
                cosmic::iced::stream::channel(100, |mut output: mpsc::Sender<Msg>| async move {
                    tracing::debug!("Starting CPDB printer discovery subscription stream");
                    let client = match CpdbClient::new().await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::error!("Failed to create CPDB client: {:?}", e);
                            return;
                        }
                    };

                    if let Ok(printers) = client.get_all_printers().await {
                        let _ = output.send(Msg::PrintersLoaded(printers)).await;
                    }

                    if let Ok(mut stream) = client.discovery_stream().await {
                        while let Some(event) = stream.next().await {
                            let _ = output.send(Msg::DiscoveryEvent(event)).await;
                        }
                    }
                })
            })
        } else {
            Subscription::none()
        }
    }

    pub fn sanitize_printer_options(&mut self) {
        if !self.color_supported {
            self.color_mode = ColorMode::Monochrome;
        }

        self.duplex_index = sanitize_index(self.duplex_index, self.duplex_values.len());
        self.paper_tray_index =
            sanitize_index(self.paper_tray_index, self.media_source_values.len());
        self.paper_type_index = sanitize_index(self.paper_type_index, self.media_type_values.len());
        self.print_quality_index =
            sanitize_index(self.print_quality_index, self.print_quality_values.len());

        let media_len = self.printer_media.as_ref().map(|m| m.len()).unwrap_or(0);
        self.selected_paper_size_index = sanitize_index(self.selected_paper_size_index, media_len);
    }

    pub fn apply_preset(&mut self, index: usize) {
        let Some(preset) = self.presets.get(index) else {
            return;
        };
        self.selected_preset_index = Some(index);
        self.color_mode = ColorMode::from_id(&preset.color_mode);
        self.orientation = Orientation::from_id(&preset.orientation);
        self.duplex_index = preset.duplex_index;
        self.copies = preset.copies.max(1);
        self.collate = preset.collate;
        self.pages_per_sheet_index = preset.pages_per_sheet_index;
        self.layout_direction = LayoutDirection::from_id(&preset.layout_direction);
        self.margins = Margins::from_id(&preset.margins);
        self.scaling = ScalingMode::from_id(&preset.scaling);
        self.custom_scaling_input = preset.custom_scaling_input;
        self.page_selection =
            PageSetSelection::from_id(&preset.page_selection, &preset.custom_range_input);
        self.custom_range_input = preset.custom_range_input.clone();
        if let PageSetSelection::Custom(_) = &self.page_selection {
            self.custom_range_valid = validate_page_range(&self.custom_range_input);
        }

        self.sanitize_printer_options();
    }

    pub fn build_current_preset(&self, name: String) -> cosmic_portal_config::print::PrintPreset {
        let id = format!(
            "custom-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        );

        cosmic_portal_config::print::PrintPreset {
            id,
            name,
            is_builtin: false,
            color_mode: self.color_mode.id().to_string(),
            orientation: self.orientation.id().to_string(),
            duplex_index: self.duplex_index,
            copies: self.copies,
            collate: self.collate,
            pages_per_sheet_index: self.pages_per_sheet_index,
            layout_direction: self.layout_direction.id().to_string(),
            margins: self.margins.id().to_string(),
            scaling: self.scaling.id().to_string(),
            custom_scaling_input: self.custom_scaling_input,
            page_selection: self.page_selection.id().to_string(),
            custom_range_input: self.custom_range_input.clone(),
        }
    }

    pub fn to_config_print(&self) -> cosmic_portal_config::print::Print {
        let custom_presets = self
            .presets
            .iter()
            .filter(|p| !p.is_builtin)
            .cloned()
            .collect();

        let last_used_preset_id = self
            .selected_preset_index
            .and_then(|idx| self.presets.get(idx))
            .map(|p| p.id.clone());

        cosmic_portal_config::print::Print {
            custom_presets,
            last_used_preset_id,
        }
    }
}

fn preset_display_name(preset: &cosmic_portal_config::print::PrintPreset) -> String {
    if preset.is_builtin {
        match preset.id.as_str() {
            "builtin-color" => fl!("preset-builtin-color"),
            "builtin-bw" => fl!("preset-builtin-bw"),
            _ => fl!("preset-builtin-default"),
        }
    } else {
        preset.name.clone()
    }
}

fn fetch_printer_details(printer: &PrinterSnapshot) -> Task<Msg> {
    if is_pdf_printer(printer) {
        return Task::perform(
            async move {
                Msg::PrinterDetailsLoaded(
                    OptionsCollection::default(),
                    default_pdf_media(),
                    HashMap::new(),
                )
            },
            |msg| msg,
        );
    }
    let printer_id = printer.id.clone();
    let backend = printer.backend.clone();
    Task::perform(
        async move {
            let client = match CpdbClient::new().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Failed to create client for details fetch: {:?}", e);
                    return Msg::PrinterDetailsLoaded(
                        OptionsCollection::default(),
                        MediaCollection::default(),
                        HashMap::new(),
                    );
                }
            };

            // Call get_all_printers first so the CPDB backend populates the printer list
            let _ = client.get_all_printers().await;

            let locale = crate::localize::posix_locale();
            let translations = client
                .get_translations(&printer_id, &backend, &locale)
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!("Failed to fetch CPDB translations for {locale}: {err}");
                    HashMap::new()
                });

            match client.get_printer_details(&printer_id, &backend).await {
                Ok((opts, media)) => Msg::PrinterDetailsLoaded(opts, media, translations),
                Err(e) => {
                    tracing::error!("Failed to fetch printer details: {:?}", e);
                    Msg::PrinterDetailsLoaded(
                        OptionsCollection::default(),
                        MediaCollection::default(),
                        translations,
                    )
                }
            }
        },
        |msg| msg,
    )
}

pub fn update(dialog: &mut PrintDialog, msg: Msg) -> Task<Msg> {
    match msg {
        Msg::PrintersLoaded(printers) => {
            let mut all_printers = vec![save_as_pdf_printer()];
            for p in printers {
                if p.id != "save-as-pdf" {
                    all_printers.push(p);
                }
            }
            dialog.printers = all_printers;
            let target_idx = if dialog.printers.len() > 1 { 1 } else { 0 };
            dialog.selected_printer_index = Some(target_idx);
            return fetch_printer_details(&dialog.printers[target_idx]);
        }
        Msg::DiscoveryEvent(event) => match event {
            DiscoveryEvent::PrinterAdded(snap) => {
                if snap.id != "save-as-pdf" {
                    if let Some(pos) = dialog
                        .printers
                        .iter()
                        .position(|p| p.id == snap.id && p.backend == snap.backend)
                    {
                        dialog.printers[pos] = snap;
                    } else {
                        dialog.printers.push(snap);
                    }
                }
                if (dialog.selected_printer_index.is_none()
                    || dialog.selected_printer_index == Some(0))
                    && dialog.printers.len() > 1
                {
                    dialog.selected_printer_index = Some(1);
                    return fetch_printer_details(&dialog.printers[1]);
                }
            }
            DiscoveryEvent::PrinterRemoved { id, backend } => {
                if id != "save-as-pdf" {
                    dialog
                        .printers
                        .retain(|p| p.id == "save-as-pdf" || !(p.id == id && p.backend == backend));
                    if let Some(sel) = dialog.selected_printer_index
                        && sel >= dialog.printers.len()
                    {
                        dialog.selected_printer_index = Some(0);
                        return fetch_printer_details(&dialog.printers[0]);
                    }
                }
            }
            DiscoveryEvent::PrinterStateChanged {
                id,
                backend,
                state,
                accepting_jobs,
            } => {
                if let Some(p) = dialog
                    .printers
                    .iter_mut()
                    .find(|p| p.id == id && p.backend == backend)
                {
                    p.state = state;
                    p.accepting_jobs = accepting_jobs;
                }
            }
            _ => {}
        },
        Msg::PrinterSelected(index) => {
            if index < dialog.printers.len() {
                dialog.selected_printer_index = Some(index);
                return fetch_printer_details(&dialog.printers[index]);
            }
        }
        Msg::PrinterDetailsLoaded(options, media, translations) => {
            let is_pdf = dialog.is_pdf_selected();
            dialog.translations = translations;

            if is_pdf {
                dialog.color_supported = true;
                dialog.duplex_values = Vec::new();
                dialog.duplex_index = None;
                dialog.media_source_values = Vec::new();
                dialog.paper_tray_index = None;
                dialog.media_type_values = Vec::new();
                dialog.paper_type_index = None;
                dialog.print_quality_values = Vec::new();
                dialog.print_quality_index = None;

                dialog.margin_options = MarginOptions {
                    top: vec![0, 500, 1000],
                    bottom: vec![0, 500, 1000],
                    left: vec![0, 500, 1000],
                    right: vec![0, 500, 1000],
                    supports_borderless: true,
                };
                if dialog.custom_margins_vertical_index.is_none() {
                    dialog.custom_margins_vertical_index = Some(0);
                }
                if dialog.custom_margins_horizontal_index.is_none() {
                    dialog.custom_margins_horizontal_index = Some(0);
                }
                dialog.selected_paper_size_index = if media.is_empty() {
                    None
                } else {
                    dialog.selected_paper_size_index.or(Some(0))
                };
                dialog.printer_options = Some(options);
                dialog.printer_media = Some(media);
            } else {
                dialog.color_supported = options
                    .get("print-color-mode")
                    .map(|o| o.supported_values.iter().any(|v| v == "color"))
                    .unwrap_or(false);
                if !dialog.color_supported {
                    dialog.color_mode = ColorMode::Monochrome;
                }

                // option parsing via helper
                (dialog.duplex_values, dialog.duplex_index) = load_option_values(&options, "sides");
                (dialog.media_source_values, dialog.paper_tray_index) =
                    load_option_values(&options, "media-source");
                (dialog.media_type_values, dialog.paper_type_index) =
                    load_option_values(&options, "media-type");
                (dialog.print_quality_values, dialog.print_quality_index) =
                    load_option_values(&options, "print-quality");

                // margins
                dialog.margin_options = MarginOptions::from_options(&options);
                dialog.custom_margins_vertical_index = if dialog.margin_options.top.is_empty() {
                    None
                } else {
                    Some(0)
                };
                dialog.custom_margins_horizontal_index = if dialog.margin_options.left.is_empty() {
                    None
                } else {
                    Some(0)
                };

                // paper size
                dialog.selected_paper_size_index = if let Some(opt) = options.get("media") {
                    media
                        .media
                        .iter()
                        .position(|m| m.name == opt.default_value)
                        .or({
                            if media.media.is_empty() {
                                None
                            } else {
                                Some(0)
                            }
                        })
                } else {
                    if media.media.is_empty() {
                        None
                    } else {
                        Some(0)
                    }
                };

                dialog.printer_options = Some(options);
                dialog.printer_media = Some(media);
                dialog.sanitize_printer_options();
            }
        }
        Msg::NavigateTo(view) => {
            commit_preset_edit(dialog);
            dialog.active_view = view;
        }
        Msg::CustomRangeInputChanged(val) => {
            dialog.custom_range_input = val.clone();
            dialog.custom_range_valid = validate_page_range(&val);
            if dialog.custom_range_valid {
                dialog.page_selection = PageSetSelection::Custom(val);
            }
        }
        Msg::IncrementCopies => {
            if dialog.copies < 9999 {
                dialog.copies = dialog.copies.saturating_add(1);
            }
        }
        Msg::DecrementCopies => {
            if dialog.copies > 1 {
                dialog.copies = dialog.copies.saturating_sub(1);
            }
        }
        Msg::ToggleCollate => {
            dialog.collate = !dialog.collate;
        }
        Msg::PaperSizeSelected(index) => {
            dialog.selected_paper_size_index = Some(index);
        }
        Msg::DuplexSelected(index) => {
            dialog.duplex_index = Some(index);
        }
        Msg::PagesPerSheetSelected(index) => {
            dialog.pages_per_sheet_index = Some(index);
        }
        Msg::MarginsSelected(margins) => {
            dialog.margins = margins;
        }
        Msg::CustomMarginVtSelected(index) => {
            dialog.custom_margins_vertical_index = Some(index);
        }
        Msg::CustomMarginHzSelected(index) => {
            dialog.custom_margins_horizontal_index = Some(index);
        }
        Msg::BorderSelected(border) => {
            dialog.border = border;
        }
        Msg::ScalingSelected(scaling) => {
            dialog.scaling = scaling;
        }
        Msg::IncrementScaling => {
            dialog.custom_scaling_input = dialog.custom_scaling_input.saturating_add(1);
        }
        Msg::DecrementScaling => {
            if dialog.custom_scaling_input > 1 {
                dialog.custom_scaling_input = dialog.custom_scaling_input.saturating_sub(1);
            }
        }
        Msg::TogglePrintHeaderFooter => {
            dialog.print_header_footer = !dialog.print_header_footer;
        }
        Msg::TogglePrintBackground => {
            dialog.print_background = !dialog.print_background;
        }
        Msg::ToggleReverseOrder => {
            dialog.reverse_order = !dialog.reverse_order;
        }
        Msg::PaperTraySelected(index) => {
            dialog.paper_tray_index = Some(index);
        }
        Msg::PaperTypeSelected(index) => {
            dialog.paper_type_index = Some(index);
        }
        Msg::PrintQualitySelected(index) => {
            dialog.print_quality_index = Some(index);
        }
        Msg::PresetSelected(index) => {
            dialog.apply_preset(index);
        }
        Msg::OpenSavePresetDialog => {
            commit_preset_edit(dialog);
            let now_str = jiff::Zoned::now().strftime("%Y-%m-%d %H:%M").to_string();
            dialog.save_preset_name_input = fl!("preset-default-name", timestamp = now_str);
            dialog.active_view = ActiveView::SavePreset;
        }
        Msg::SavePresetNameInputChanged(val) => {
            dialog.save_preset_name_input = val;
        }
        Msg::ConfirmSavePreset => {
            let name = dialog.save_preset_name_input.trim().to_string();
            if !name.is_empty() {
                let preset = dialog.build_current_preset(name);
                dialog.presets.push(preset);
                dialog.selected_preset_index = Some(dialog.presets.len() - 1);
                dialog.active_view = ActiveView::Main;
            }
        }
        Msg::OpenEditPresetsDialog => {
            commit_preset_edit(dialog);
            dialog.editing_preset_name_input = String::new();
            dialog.active_view = ActiveView::EditPresets;
        }
        Msg::ToggleEditPresetRow(idx) => {
            commit_preset_edit(dialog);
            if idx < dialog.presets.len() && !dialog.presets[idx].is_builtin {
                dialog.editing_preset_row = Some(idx);
                dialog.editing_preset_name_input = dialog.presets[idx].name.clone();
                let input_id = cosmic::iced::widget::Id::new(format!("preset_edit_input_{idx}"));
                return Task::batch(vec![
                    text_input::focus(input_id.clone()),
                    text_input::select_all(input_id),
                ]);
            }
        }
        Msg::PresetRowNameInputChanged(val) => {
            dialog.editing_preset_name_input = val;
        }
        Msg::SavePresetRowName(idx) => {
            if idx < dialog.presets.len() && !dialog.presets[idx].is_builtin {
                let name = dialog.editing_preset_name_input.trim().to_string();
                if !name.is_empty() {
                    dialog.presets[idx].name = name;
                }
                dialog.editing_preset_row = None;
            }
        }
        Msg::DeletePresetRow(idx) => {
            if idx < dialog.presets.len() && !dialog.presets[idx].is_builtin {
                dialog.presets.remove(idx);
                if dialog.selected_preset_index == Some(idx) {
                    dialog.selected_preset_index = Some(0);
                } else if dialog.selected_preset_index.is_some_and(|i| i > idx) {
                    dialog.selected_preset_index = dialog.selected_preset_index.map(|i| i - 1);
                }
                dialog.editing_preset_row = None;
            }
        }
        Msg::AddNewPresetInEditor => {
            commit_preset_edit(dialog);
            let now_str = jiff::Zoned::now().strftime("%Y-%m-%d %H:%M").to_string();
            dialog.save_preset_name_input = fl!("preset-default-name", timestamp = now_str);
            dialog.active_view = ActiveView::SavePreset;
        }
        Msg::CommitEdit => {
            commit_preset_edit(dialog);
        }
        Msg::EscapePressed => {
            if dialog.editing_preset_row.is_some() {
                commit_preset_edit(dialog);
            } else if dialog.active_view != ActiveView::Main {
                dialog.active_view = ActiveView::Main;
            }
        }
        Msg::Cancel
        | Msg::Confirm
        | Msg::EnterPressed
        | Msg::PageSelectionModelActivated(_)
        | Msg::ColorModelActivated(_)
        | Msg::OrientationModelActivated(_)
        | Msg::LayoutDirectionModelActivated(_)
        | Msg::SurfaceAction(_) => {}
    }
    cosmic::Task::none()
}

fn commit_preset_edit(dialog: &mut PrintDialog) {
    if let Some(idx) = dialog.editing_preset_row {
        if idx < dialog.presets.len() && !dialog.presets[idx].is_builtin {
            let name = dialog.editing_preset_name_input.trim().to_string();
            if !name.is_empty() {
                dialog.presets[idx].name = name;
            }
        }
        dialog.editing_preset_row = None;
    }
}

fn custom_dropdown<'a, S: AsRef<str> + Clone + Send + Sync + 'static>(
    window_id: cosmic::iced::window::Id,
    selections: impl Into<Cow<'a, [S]>>,
    selected: Option<usize>,
    on_selected: impl Fn(usize) -> Msg + Send + Sync + 'static,
) -> Element<'a, Msg> {
    popup_dropdown(
        selections,
        selected,
        on_selected,
        window_id,
        Msg::SurfaceAction,
        |msg| crate::app::Msg::Print(crate::print::Msg::Dialog(msg)),
    )
    .into()
}

fn option_row<'a>(
    label: impl Into<Cow<'a, str>> + 'a,
    control: impl Into<cosmic::Element<'a, Msg>>,
) -> Element<'a, Msg> {
    row![text(label), space::horizontal(), control.into()]
        .align_y(Alignment::Center)
        .into()
}

fn disabled_option_row<'a>(
    label: impl Into<Cow<'a, str>> + 'a,
    placeholder: impl Into<Cow<'a, str>> + 'a,
) -> Element<'a, Msg> {
    let label_text = text(label).class(Text::Custom(|theme| {
        let mut color = theme.current_container().component.on;
        color.alpha *= 0.38;
        cosmic::iced::core::widget::text::Style {
            color: Some(Color::from(color)),
            ..Default::default()
        }
    }));

    let control = disabled_placeholder(placeholder);

    row![label_text, space::horizontal(), control]
        .align_y(Alignment::Center)
        .into()
}

fn disabled_placeholder<'a, Msg: 'static + Clone>(
    label: impl Into<Cow<'a, str>> + 'a,
) -> Element<'a, Msg> {
    let theme_spacing = theme::spacing();
    container(text(label).size(14).class(Text::Custom(|theme| {
        let mut color = theme.current_container().component.on;
        color.alpha *= 0.38;
        cosmic::iced::core::widget::text::Style {
            color: Some(Color::from(color)),
            ..Default::default()
        }
    })))
    .height(Length::Fixed(f32::from(theme_spacing.space_l)))
    .padding(Padding::from([0.0, f32::from(theme_spacing.space_s)]))
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .into()
}

fn option_group<'a>(
    title: Option<Cow<'a, str>>,
    items: Vec<Vec<Element<'a, Msg>>>,
) -> Element<'a, Msg> {
    let mut col = column![].spacing(8);
    if let Some(t) = title {
        col = col.push(text(t).size(14).font(Font {
            weight: Weight::Bold,
            ..Default::default()
        }));
    }

    let mut list = column![].spacing(0);
    list = list.push(space::vertical().height(4.0));
    for (i, sub_items) in items.into_iter().enumerate() {
        if i > 0 {
            list = list.push(divider::horizontal::light());
        }
        for item in sub_items {
            let padded_item = container(item)
                .padding(Padding {
                    left: 24.0,
                    right: 24.0,
                    top: 10.0,
                    bottom: 10.0,
                })
                .width(Length::Fill);
            list = list.push(padded_item);
        }
    }
    list = list.push(space::vertical().height(4.0));

    col = col.push(
        widget::layer_container(list)
            .layer(cosmic::cosmic_theme::Layer::Primary)
            .padding(0)
            .width(Length::Fill),
    );
    col.into()
}

fn counter_button<'a>(label: &'a str, msg: Option<Msg>) -> Element<'a, Msg> {
    button::custom(
        container(text(label).size(16).font(Font {
            weight: Weight::Bold,
            ..Default::default()
        }))
        .width(32)
        .height(32)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center),
    )
    .padding(0)
    .on_press_maybe(msg)
    .class(Button::Custom {
        active: Box::new(|_focused, _theme| button::Style {
            background: None,
            border_radius: 16.0.into(),
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
            ..Default::default()
        }),
        disabled: Box::new(|theme| {
            let mut color = theme.current_container().component.on;
            color.alpha *= 0.5;
            button::Style {
                background: None,
                border_radius: 16.0.into(),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
                text_color: Some(Color::from(color)),
                ..Default::default()
            }
        }),
        hovered: Box::new(|_focused, theme| {
            let theme = theme.cosmic();
            button::Style {
                background: Some(Background::Color(theme.background(false).divider.into())),
                border_radius: 16.0.into(),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
                ..Default::default()
            }
        }),
        pressed: Box::new(|_focused, theme| {
            let theme = theme.cosmic();
            button::Style {
                background: Some(Background::Color(theme.background(false).divider.into())),
                border_radius: 16.0.into(),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
                ..Default::default()
            }
        }),
    })
    .into()
}

fn modal_overlay<'a>(
    base: Element<'a, Msg>,
    modal: Element<'a, Msg>,
    on_click_backdrop: Option<Msg>,
) -> Element<'a, Msg> {
    let overlay = container(modal)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(|_theme| container::Style {
            background: Some(cosmic::iced::Color::from_rgba(0.0, 0.0, 0.0, 0.5).into()),
            ..Default::default()
        });

    let overlay_element: Element<'a, Msg> = if let Some(msg) = on_click_backdrop {
        cosmic::iced::widget::mouse_area(overlay)
            .on_press(msg)
            .into()
    } else {
        overlay.into()
    };

    Stack::new().push(base).push(overlay_element).into()
}

pub fn view<'a>(
    dialog: &'a PrintDialog,
    color_model: &'a segmented_button::Model<SingleSelect>,
    orientation_model: &'a segmented_button::Model<SingleSelect>,
    layout_direction_model: &'a segmented_button::Model<SingleSelect>,
    page_selection_model: &'a segmented_button::Model<SingleSelect>,
) -> Element<'a, Msg> {
    let base = match dialog.active_view {
        ActiveView::Main | ActiveView::SavePreset | ActiveView::EditPresets => view_main(
            dialog,
            color_model,
            orientation_model,
            layout_direction_model,
        ),
        ActiveView::PageSelection => view_pages_selection(dialog, page_selection_model),
    };

    let content = match dialog.active_view {
        ActiveView::Main | ActiveView::PageSelection => base,
        ActiveView::SavePreset => modal_overlay(base, view_save_preset(dialog), None),
        ActiveView::EditPresets => {
            let on_click = if dialog.editing_preset_row.is_some() {
                Some(Msg::CommitEdit)
            } else {
                None
            };
            modal_overlay(base, view_edit_presets(dialog), on_click)
        }
    };

    widget::layer_container(content)
        .layer(cosmic::cosmic_theme::Layer::Background)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn view_pages_selection<'a>(
    dialog: &'a PrintDialog,
    page_selection_model: &'a segmented_button::Model<SingleSelect>,
) -> Element<'a, Msg> {
    let title_col = container(column![
        button::custom(
            row![icon::from_name("go-previous-symbolic"), text(fl!("print"))]
                .spacing(4)
                .align_y(Alignment::Center),
        )
        .class(Button::Text)
        .on_press(Msg::NavigateTo(ActiveView::Main)),
        space::vertical().height(8.0),
        text(fl!("pages")).size(20).font(Font {
            weight: Weight::Bold,
            ..Default::default()
        }),
    ])
    .padding(Padding {
        top: 16.0,
        bottom: 0.0,
        left: 24.0,
        right: 24.0,
    });

    let segmented = segmented_button::vertical(page_selection_model)
        .style(SegmentedButton::FileNav)
        .button_alignment(Alignment::Start)
        .button_height(50)
        .font_size(15.0)
        .on_activate(Msg::PageSelectionModelActivated)
        .width(Length::Fill);

    let is_custom = matches!(dialog.page_selection, PageSetSelection::Custom(_));

    let mut list_items = vec![];

    if is_custom {
        let overlay_row = row![
            text_input(fl!("page-range-placeholder"), &dialog.custom_range_input)
                .on_input(Msg::CustomRangeInputChanged)
                .width(Length::Fixed(200.0)),
            button::icon(icon::from_name("edit-clear-symbolic"))
                .on_press(Msg::CustomRangeInputChanged(String::new()))
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let overlay_col = column![
            widget::space::vertical().height(200.0),
            container(overlay_row)
                .width(Length::Fill)
                .height(Length::Fixed(50.0))
                .align_x(Alignment::End)
                .align_y(Alignment::Center)
                .padding(Padding {
                    top: 0.0,
                    bottom: 0.0,
                    left: 0.0,
                    right: 8.0,
                })
        ];

        let stack = Stack::new().push(segmented).push(overlay_col);

        list_items.push(stack.into());
    } else {
        list_items.push(segmented.into());
    }

    let show_error =
        is_custom && !dialog.custom_range_valid && !dialog.custom_range_input.trim().is_empty();
    if show_error {
        let error_text = text(fl!("page-range-invalid"))
            .size(11)
            .class(Text::Custom(|theme| {
                let theme = theme.cosmic();
                cosmic::iced::core::widget::text::Style {
                    color: Some(theme.destructive.base.into()),
                    ..Default::default()
                }
            }));
        list_items.push(error_text.into());
    }

    let list = option_group(None, vec![list_items]);

    column![
        title_col,
        container(list).padding(Padding {
            top: 16.0,
            bottom: 16.0,
            left: 24.0,
            right: 24.0,
        })
    ]
    .into()
}

fn view_save_preset<'a>(dialog: &'a PrintDialog) -> Element<'a, Msg> {
    let title_header = container(text(fl!("save-preset")).size(20).font(Font {
        weight: Weight::Bold,
        ..Default::default()
    }))
    .padding(Padding {
        top: 16.0,
        bottom: 8.0,
        left: 16.0,
        right: 16.0,
    });

    let name_input = text_input(fl!("preset-name"), &dialog.save_preset_name_input)
        .on_input(Msg::SavePresetNameInputChanged)
        .on_submit(|_| Msg::ConfirmSavePreset)
        .width(Length::Fill);

    let body = container(column![name_input].spacing(12)).padding(16);

    let can_save = !dialog.save_preset_name_input.trim().is_empty();

    let cancel_button = button::standard(fl!("cancel"))
        .class(Button::Standard)
        .on_press(Msg::NavigateTo(ActiveView::Main));

    let save_button = button::suggested(fl!("save")).on_press_maybe(if can_save {
        Some(Msg::ConfirmSavePreset)
    } else {
        None
    });

    let footer = row![widget::space::horizontal(), cancel_button, save_button]
        .align_y(Alignment::Center)
        .spacing(12)
        .padding(16);

    let card = column![
        title_header,
        widget::scrollable(body).height(Length::Fill),
        footer
    ];

    container(card)
        .width(Length::Fixed(400.0))
        .height(Length::Fixed(190.0))
        .style(|theme: &cosmic::Theme| {
            let cosmic = theme.cosmic();
            container::Style {
                background: Some(cosmic.bg_color().into()),
                border: cosmic::iced::Border {
                    radius: 16.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}

fn view_edit_presets<'a>(dialog: &'a PrintDialog) -> Element<'a, Msg> {
    let is_editing_preset = dialog.editing_preset_row.is_some();

    let title_header = container(text(fl!("presets")).size(20).font(Font {
        weight: Weight::Bold,
        ..Default::default()
    }))
    .padding(Padding {
        top: 16.0,
        bottom: 8.0,
        left: 16.0,
        right: 16.0,
    });

    let mut rows: Vec<Element<'_, Msg>> = Vec::new();

    for (idx, preset) in dialog.presets.iter().enumerate() {
        if preset.is_builtin {
            continue;
        }

        let is_editing = dialog.editing_preset_row == Some(idx);

        let name_widget: Element<'_, Msg> = if is_editing {
            let input_id = cosmic::iced::widget::Id::new(format!("preset_edit_input_{idx}"));
            text_input(fl!("preset-name"), &dialog.editing_preset_name_input)
                .id(input_id)
                .on_input(Msg::PresetRowNameInputChanged)
                .on_submit(move |_| Msg::SavePresetRowName(idx))
                .width(Length::Fill)
                .into()
        } else {
            text(&preset.name).size(14).into()
        };

        let clear_or_edit_button: Element<'_, Msg> = if is_editing {
            let clear_msg = if dialog.editing_preset_name_input.is_empty() {
                Msg::SavePresetRowName(idx)
            } else {
                Msg::PresetRowNameInputChanged(String::new())
            };
            button::icon(icon::from_name("edit-clear-symbolic"))
                .on_press(clear_msg)
                .into()
        } else {
            button::icon(icon::from_name("document-edit-symbolic"))
                .on_press(Msg::ToggleEditPresetRow(idx))
                .into()
        };

        let delete_button: Element<'_, Msg> = button::icon(icon::from_name("user-trash-symbolic"))
            .on_press(Msg::DeletePresetRow(idx))
            .into();

        let preset_row = row![
            container(name_widget)
                .width(Length::Fill)
                .height(36)
                .align_y(Alignment::Center),
            clear_or_edit_button,
            delete_button
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut row_mouse_area = cosmic::iced::widget::mouse_area(preset_row);
        if !is_editing && is_editing_preset {
            row_mouse_area = row_mouse_area.on_press(Msg::CommitEdit);
        }

        rows.push(row_mouse_area.into());
    }

    let add_preset_btn = button::standard(fl!("add-preset"))
        .class(Button::Standard)
        .on_press(Msg::AddNewPresetInEditor);

    let add_preset_row = row![space::horizontal(), add_preset_btn].align_y(Alignment::Center);

    let mut body_col = column![].spacing(16);

    if !rows.is_empty() {
        let preset_group = option_group(None, rows.into_iter().map(|r| vec![r]).collect());
        body_col = body_col.push(preset_group);
    }

    body_col = body_col.push(add_preset_row);

    let body = container(body_col).padding(16);

    let close_button = button::standard(fl!("close"))
        .class(Button::Standard)
        .on_press(Msg::NavigateTo(ActiveView::Main));

    let footer = container(
        row![widget::space::horizontal(), close_button]
            .align_y(Alignment::Center)
            .spacing(12)
            .padding(16),
    )
    .width(Length::Fill)
    .style(|theme: &cosmic::Theme| {
        let cosmic = theme.cosmic();
        container::Style {
            background: Some(cosmic.primary_container_color().into()),
            border: cosmic::iced::Border {
                radius: cosmic::iced::border::Radius {
                    top_left: 0.0,
                    top_right: 0.0,
                    bottom_right: 16.0,
                    bottom_left: 16.0,
                },
                ..Default::default()
            },
            ..Default::default()
        }
    });

    let card = column![
        title_header,
        widget::scrollable(body)
            .id(cosmic::iced::widget::Id::new("edit_presets_scroll"))
            .height(Length::Fill),
        footer
    ];

    container(card)
        .width(Length::Fixed(500.0))
        .height(Length::Fixed(460.0))
        .style(|theme: &cosmic::Theme| {
            let cosmic = theme.cosmic();
            container::Style {
                background: Some(cosmic.bg_color().into()),
                border: cosmic::iced::Border {
                    radius: 16.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}

fn view_main<'a>(
    dialog: &'a PrintDialog,
    color_model: &'a segmented_button::Model<SingleSelect>,
    orientation_model: &'a segmented_button::Model<SingleSelect>,
    layout_direction_model: &'a segmented_button::Model<SingleSelect>,
) -> Element<'a, Msg> {
    let title_header = container(text(fl!("print")).size(20).font(Font {
        weight: Weight::Bold,
        ..Default::default()
    }))
    .padding(Padding {
        top: 16.0,
        bottom: 0.0,
        left: 24.0,
        right: 24.0,
    });

    let options = view_options_panel(
        dialog,
        color_model,
        orientation_model,
        layout_direction_model,
    );
    let status_bar = view_status_row(dialog);

    column![
        title_header,
        widget::scrollable(options).height(Length::Fill),
        status_bar
    ]
    .into()
}

fn view_options_panel<'a>(
    dialog: &'a PrintDialog,
    color_model: &'a segmented_button::Model<SingleSelect>,
    orientation_model: &'a segmented_button::Model<SingleSelect>,
    layout_direction_model: &'a segmented_button::Model<SingleSelect>,
) -> Element<'a, Msg> {
    let spacing = 16;
    let mut groups = column![].spacing(spacing).padding(Padding {
        top: 16.0,
        bottom: 16.0,
        left: 24.0,
        right: 24.0,
    });

    // Group 1: Destination and Presets
    let printer_names: Vec<String> = dialog.printers.iter().map(|p| p.name.clone()).collect();
    let dest_dropdown: Element<'_, Msg> = if printer_names.is_empty() {
        custom_dropdown(
            dialog.window_id,
            vec![fl!("no-printers-found")],
            Some(0),
            |_| Msg::PrinterSelected(0),
        )
    } else {
        custom_dropdown(
            dialog.window_id,
            printer_names,
            dialog.selected_printer_index,
            Msg::PrinterSelected,
        )
    };

    let mut preset_names: Vec<String> = dialog.presets.iter().map(preset_display_name).collect();
    let save_action_index = preset_names.len();
    preset_names.push(fl!("save-preset-action"));
    let edit_action_index = preset_names.len();
    preset_names.push(fl!("edit-presets-action"));

    let preset_dropdown: Element<'_, Msg> = custom_dropdown(
        dialog.window_id,
        preset_names,
        dialog.selected_preset_index,
        move |idx| {
            if idx == save_action_index {
                Msg::OpenSavePresetDialog
            } else if idx == edit_action_index {
                Msg::OpenEditPresetsDialog
            } else {
                Msg::PresetSelected(idx)
            }
        },
    );

    let top_group = option_group(
        None,
        vec![
            vec![option_row(fl!("destination"), dest_dropdown)],
            vec![option_row(fl!("preset"), preset_dropdown)],
        ],
    );
    groups = groups.push(top_group);

    // Group 2: Primary settings
    let color_slider = segmented_button::horizontal(color_model)
        .style(SegmentedButton::Control)
        .button_alignment(Alignment::Center)
        .font_active(font::default())
        .on_activate(Msg::ColorModelActivated);

    let orientation_slider = segmented_button::horizontal(orientation_model)
        .style(SegmentedButton::Control)
        .button_alignment(Alignment::Center)
        .font_active(font::default())
        .on_activate(Msg::OrientationModelActivated);

    let pages_row = option_row(
        fl!("pages"),
        button::custom(
            row![
                text(dialog.page_selection.summary()),
                icon::from_name("go-next-symbolic")
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .class(Button::Text)
        .on_press(Msg::NavigateTo(ActiveView::PageSelection)),
    );

    let decrement_msg = if dialog.copies > 1 {
        Some(Msg::DecrementCopies)
    } else {
        None
    };

    let increment_msg = if dialog.copies < 9999 {
        Some(Msg::IncrementCopies)
    } else {
        None
    };

    let copies_control = row![
        counter_button("-", decrement_msg),
        text(format!("{}", dialog.copies)).size(16),
        counter_button("+", increment_msg),
    ]
    .align_y(Alignment::Center)
    .spacing(12);

    let collate_toggle = toggler(dialog.collate).on_toggle(|_| Msg::ToggleCollate);

    let paper_size_row = if let Some(media) = &dialog.printer_media {
        if media.is_empty() {
            disabled_option_row(fl!("paper-size"), fl!("not-supported"))
        } else if media.len() == 1 {
            disabled_option_row(fl!("paper-size"), media.media[0].name.clone())
        } else {
            let paper_sizes: Vec<String> = media.iter().map(|m| m.name.clone()).collect();
            let paper_size_dropdown = custom_dropdown(
                dialog.window_id,
                paper_sizes,
                dialog.selected_paper_size_index,
                Msg::PaperSizeSelected,
            );
            option_row(fl!("paper-size"), paper_size_dropdown)
        }
    } else {
        disabled_option_row(fl!("paper-size"), fl!("not-supported"))
    };

    let duplex_row: Option<Element<'_, Msg>> = if dialog.duplex_values.is_empty() {
        Some(disabled_option_row(
            fl!("print-on-sides"),
            fl!("not-supported"),
        ))
    } else if dialog.duplex_values.len() == 1 {
        Some(disabled_option_row(
            fl!("print-on-sides"),
            sides_label(dialog, &dialog.duplex_values[0]),
        ))
    } else {
        let labels: Vec<String> = dialog
            .duplex_values
            .iter()
            .map(|v| sides_label(dialog, v))
            .collect();
        Some(option_row(
            fl!("print-on-sides"),
            custom_dropdown(
                dialog.window_id,
                labels,
                dialog.duplex_index,
                Msg::DuplexSelected,
            ),
        ))
    };

    let is_pdf = dialog.is_pdf_selected();

    let mut primary_items = vec![
        vec![color_slider.into(), orientation_slider.into()],
        vec![pages_row],
    ];
    if !is_pdf {
        primary_items.push(vec![option_row(fl!("copies"), copies_control)]);
        primary_items.push(vec![option_row(fl!("collate"), collate_toggle)]);
    }
    primary_items.push(vec![paper_size_row]);
    if !is_pdf && let Some(row) = duplex_row {
        primary_items.push(vec![row]);
    }
    let primary_group = option_group(None, primary_items);
    groups = groups.push(primary_group);

    // Group 3: Layout
    let pps_dropdown = custom_dropdown(
        dialog.window_id,
        vec![
            "1".to_string(),
            "2".to_string(),
            "4".to_string(),
            "6".to_string(),
            "9".to_string(),
            "16".to_string(),
        ],
        dialog.pages_per_sheet_index,
        Msg::PagesPerSheetSelected,
    );

    let layout_dir_row = segmented_button::horizontal(layout_direction_model)
        .style(SegmentedButton::Control)
        .width(Length::Fill)
        .button_alignment(Alignment::Center)
        .font_active(font::default())
        .on_activate(Msg::LayoutDirectionModelActivated);

    let margins_dropdown = custom_dropdown(
        dialog.window_id,
        Margins::ALL.iter().map(Margins::label).collect::<Vec<_>>(),
        Some(
            Margins::ALL
                .iter()
                .position(|m| *m == dialog.margins)
                .unwrap_or(0),
        ),
        |i| Msg::MarginsSelected(Margins::ALL[i]),
    );

    let mut layout_items: Vec<Vec<Element<'a, Msg>>> =
        vec![vec![option_row(fl!("pages-per-sheet"), pps_dropdown)]];
    if !is_pdf {
        layout_items.push(vec![
            row![text(fl!("layout-direction")), layout_dir_row]
                .spacing(16)
                .align_y(Alignment::Center)
                .into(),
        ]);
    }

    if dialog.margins == Margins::Custom {
        let to_mm = |v: u32| fl!("margin-value", mm = format!("{:.1}", v as f32 / 100.0));

        let vt_labels: Vec<String> = dialog
            .margin_options
            .top
            .iter()
            .map(|&v| to_mm(v))
            .collect();
        let hz_labels: Vec<String> = dialog
            .margin_options
            .left
            .iter()
            .map(|&v| to_mm(v))
            .collect();

        let top_bottom_row = option_row(
            fl!("margin-vertical"),
            custom_dropdown(
                dialog.window_id,
                vt_labels,
                dialog.custom_margins_vertical_index,
                Msg::CustomMarginVtSelected,
            ),
        );
        let left_right_row = option_row(
            fl!("margin-horizontal"),
            custom_dropdown(
                dialog.window_id,
                hz_labels,
                dialog.custom_margins_horizontal_index,
                Msg::CustomMarginHzSelected,
            ),
        );

        layout_items.push(vec![
            option_row(fl!("margins"), margins_dropdown),
            top_bottom_row,
            left_right_row,
        ]);
    } else {
        layout_items.push(vec![option_row(fl!("margins"), margins_dropdown)]);
    }

    let border_dropdown = custom_dropdown(
        dialog.window_id,
        Border::ALL.iter().map(Border::label).collect::<Vec<_>>(),
        Some(
            Border::ALL
                .iter()
                .position(|b| *b == dialog.border)
                .unwrap_or(0),
        ),
        |i| Msg::BorderSelected(Border::ALL[i]),
    );
    layout_items.push(vec![option_row(fl!("border"), border_dropdown)]);

    let scaling_dropdown = custom_dropdown(
        dialog.window_id,
        ScalingMode::ALL
            .iter()
            .map(ScalingMode::label)
            .collect::<Vec<_>>(),
        Some(
            ScalingMode::ALL
                .iter()
                .position(|s| *s == dialog.scaling)
                .unwrap_or(0),
        ),
        |i| Msg::ScalingSelected(ScalingMode::ALL[i]),
    );

    if dialog.scaling == ScalingMode::Custom {
        let decrement_scaling_msg = if dialog.custom_scaling_input > 1 {
            Some(Msg::DecrementScaling)
        } else {
            None
        };
        let custom_scaling_control = row![
            counter_button("-", decrement_scaling_msg),
            text(format!("{}", dialog.custom_scaling_input)).size(16),
            counter_button("+", Some(Msg::IncrementScaling)),
        ]
        .align_y(Alignment::Center)
        .spacing(12);

        let custom_scaling_row: Element<'_, Msg> =
            row![space::horizontal(), custom_scaling_control]
                .align_y(Alignment::Center)
                .into();

        layout_items.push(vec![
            option_row(fl!("scaling"), scaling_dropdown),
            custom_scaling_row,
        ]);
    } else {
        layout_items.push(vec![option_row(fl!("scaling"), scaling_dropdown)]);
    }

    if dialog.show_print_header_footer_toggle {
        layout_items.push(vec![option_row(
            fl!("print-header-footer"),
            toggler(dialog.print_header_footer).on_toggle(|_| Msg::TogglePrintHeaderFooter),
        )]);
    }

    if dialog.show_print_background_toggle {
        layout_items.push(vec![option_row(
            fl!("print-background"),
            toggler(dialog.print_background).on_toggle(|_| Msg::TogglePrintBackground),
        )]);
    }

    let layout_group = option_group(Some(fl!("layout").into()), layout_items);
    groups = groups.push(layout_group);

    // Group 4: Paper handling
    let tray_row: Option<Element<'_, Msg>> = if dialog.media_source_values.is_empty() {
        Some(disabled_option_row(fl!("paper-tray"), fl!("not-supported")))
    } else if dialog.media_source_values.len() == 1 {
        Some(disabled_option_row(
            fl!("paper-tray"),
            tray_label(dialog, &dialog.media_source_values[0]),
        ))
    } else {
        let labels: Vec<String> = dialog
            .media_source_values
            .iter()
            .map(|v| tray_label(dialog, v))
            .collect();
        Some(option_row(
            fl!("paper-tray"),
            custom_dropdown(
                dialog.window_id,
                labels,
                dialog.paper_tray_index,
                Msg::PaperTraySelected,
            ),
        ))
    };

    let type_row: Option<Element<'_, Msg>> = if dialog.media_type_values.is_empty() {
        Some(disabled_option_row(fl!("paper-type"), fl!("not-supported")))
    } else if dialog.media_type_values.len() == 1 {
        Some(disabled_option_row(
            fl!("paper-type"),
            media_type_label(dialog, &dialog.media_type_values[0]),
        ))
    } else {
        let labels: Vec<String> = dialog
            .media_type_values
            .iter()
            .map(|v| media_type_label(dialog, v))
            .collect();
        Some(option_row(
            fl!("paper-type"),
            custom_dropdown(
                dialog.window_id,
                labels,
                dialog.paper_type_index,
                Msg::PaperTypeSelected,
            ),
        ))
    };

    let quality_row: Option<Element<'_, Msg>> = if dialog.print_quality_values.is_empty() {
        Some(disabled_option_row(
            fl!("print-quality"),
            fl!("not-supported"),
        ))
    } else if dialog.print_quality_values.len() == 1 {
        Some(disabled_option_row(
            fl!("print-quality"),
            quality_label(dialog, &dialog.print_quality_values[0]),
        ))
    } else {
        let labels: Vec<String> = dialog
            .print_quality_values
            .iter()
            .map(|v| quality_label(dialog, v))
            .collect();
        Some(option_row(
            fl!("print-quality"),
            custom_dropdown(
                dialog.window_id,
                labels,
                dialog.print_quality_index,
                Msg::PrintQualitySelected,
            ),
        ))
    };

    if !is_pdf {
        let mut paper_items = vec![vec![option_row(
            fl!("reverse-order"),
            toggler(dialog.reverse_order).on_toggle(|_| Msg::ToggleReverseOrder),
        )]];
        if let Some(row) = tray_row {
            paper_items.push(vec![row]);
        }
        if let Some(row) = type_row {
            paper_items.push(vec![row]);
        }
        if let Some(row) = quality_row {
            paper_items.push(vec![row]);
        }

        let paper_group = option_group(Some(fl!("paper-handling").into()), paper_items);
        groups = groups.push(paper_group);
    }

    groups.into()
}

fn printer_state_label(state: &PrinterState) -> String {
    match state {
        PrinterState::Idle => fl!("printer-state-idle"),
        PrinterState::Processing => fl!("printer-state-processing"),
        PrinterState::Stopped => fl!("printer-state-stopped"),
        PrinterState::Unknown(raw) => raw.clone(),
    }
}

fn printer_state_icon(state: &PrinterState) -> Option<&'static str> {
    match state {
        PrinterState::Idle => Some("process-completed-symbolic"),
        PrinterState::Stopped => Some("media-playback-pause-symbolic"),
        PrinterState::Unknown(_) => Some("dialog-error-symbolic"),
        PrinterState::Processing => None,
    }
}

fn view_status_row(dialog: &PrintDialog) -> Element<'_, Msg> {
    let selected_printer = dialog
        .selected_printer_index
        .and_then(|idx| dialog.printers.get(idx));

    let status_icon = selected_printer
        .filter(|printer| !is_pdf_printer(printer))
        .and_then(|printer| printer_state_icon(&printer.state));

    let status_text = if let Some(printer) = selected_printer {
        if is_pdf_printer(printer) {
            fl!("save-output-to-pdf")
        } else {
            fl!(
                "printer-status",
                name = printer.name.as_str(),
                state = printer_state_label(&printer.state)
            )
        }
    } else if dialog.selected_printer_index.is_some() {
        fl!("no-printer-selected")
    } else {
        fl!("no-printers-found")
    };

    let cancel_btn = button::standard(fl!("cancel")).on_press(Msg::Cancel);
    let confirm_label = dialog.accept_label.clone().unwrap_or_else(|| fl!("print"));
    let print_btn = button::suggested(confirm_label).on_press(Msg::Confirm);

    let status: Element<'_, Msg> = if let Some(icon_name) = status_icon {
        row![icon::from_name(icon_name), text(status_text).size(14)]
            .spacing(4)
            .align_y(Alignment::Center)
            .into()
    } else {
        text(status_text).size(14).into()
    };

    let content = row![status, widget::space::horizontal(), cancel_btn, print_btn]
        .align_y(Alignment::Center)
        .spacing(12)
        .padding(16);

    widget::layer_container(content)
        .layer(cosmic::cosmic_theme::Layer::Primary)
        .into()
}

fn validate_page_range(input: &str) -> bool {
    let input = input.trim();
    if input.is_empty() {
        return false;
    }
    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return false;
        }
        if part.contains('-') {
            let subparts: Vec<&str> = part.split('-').collect();
            if subparts.len() != 2 {
                return false;
            }
            let start = subparts[0].trim();
            let end = subparts[1].trim();
            if start.is_empty() || end.is_empty() {
                return false;
            }
            let start_val: u32 = match start.parse() {
                Ok(v) => v,
                Err(_) => return false,
            };
            let end_val: u32 = match end.parse() {
                Ok(v) => v,
                Err(_) => return false,
            };
            if start_val > end_val || start_val == 0 {
                return false;
            }
        } else {
            let val: u32 = match part.parse() {
                Ok(v) => v,
                Err(_) => return false,
            };
            if val == 0 {
                return false;
            }
        }
    }
    true
}

fn clean_supported_values(values: &[String]) -> Vec<String> {
    values
        .iter()
        .filter(|v| {
            let s = v.trim().to_uppercase();
            !s.is_empty() && s != "NA" && s != "N/A"
        })
        .cloned()
        .collect()
}

fn load_option_values(options: &OptionsCollection, key: &str) -> (Vec<String>, Option<usize>) {
    if let Some(opt) = options.get(key) {
        let values = clean_supported_values(&opt.supported_values);
        let index = values
            .iter()
            .position(|v| *v == opt.default_value)
            .or(if values.is_empty() { None } else { Some(0) });
        (values, index)
    } else {
        (Vec::new(), None)
    }
}

/// Backend supplied (from printer) label for a CPDB option choice, falling back to the raw keyword
fn backend_label(dialog: &PrintDialog, option: &str, value: &str) -> String {
    dialog
        .translations
        .get(&format!("OPT#{option}#{value}"))
        .cloned()
        .unwrap_or_else(|| value.to_owned())
}

/// Label for a `sides` (duplex) value
fn sides_label(dialog: &PrintDialog, raw: &str) -> String {
    match raw {
        "one-sided" => fl!("sides-one-sided"),
        "two-sided-long-edge" => fl!("sides-two-sided-long-edge"),
        "two-sided-short-edge" => fl!("sides-two-sided-short-edge"),
        other => backend_label(dialog, "sides", other),
    }
}

/// Label for a `media-source` (paper tray) value
fn tray_label(dialog: &PrintDialog, raw: &str) -> String {
    match raw {
        "auto" => fl!("media-source-auto"),
        "main" => fl!("media-source-main"),
        "manual" => fl!("media-source-manual"),
        "by-pass-tray" => fl!("media-source-by-pass-tray"),
        "top" => fl!("media-source-top"),
        "middle" => fl!("media-source-middle"),
        "bottom" => fl!("media-source-bottom"),
        "envelope" => fl!("media-source-envelope"),
        "large-capacity" => fl!("media-source-large-capacity"),
        other => backend_label(dialog, "media-source", other),
    }
}

/// Label for a `media-type` (paper type) value
fn media_type_label(dialog: &PrintDialog, raw: &str) -> String {
    match raw {
        "auto" => fl!("media-type-auto"),
        "stationery" => fl!("media-type-stationery"),
        "stationery-letterhead" => fl!("media-type-stationery-letterhead"),
        "stationery-lightweight" => fl!("media-type-stationery-lightweight"),
        "stationery-heavyweight" => fl!("media-type-stationery-heavyweight"),
        "cardstock" => fl!("media-type-cardstock"),
        "labels" => fl!("media-type-labels"),
        "envelope" => fl!("media-type-envelope"),
        "transparency" => fl!("media-type-transparency"),
        "recycled" => fl!("media-type-recycled"),
        "photographic" => fl!("media-type-photographic"),
        "photographic-glossy" => fl!("media-type-photographic-glossy"),
        "photographic-matte" => fl!("media-type-photographic-matte"),
        other => backend_label(dialog, "media-type", other),
    }
}

/// Label for a `print-quality` value
fn quality_label(dialog: &PrintDialog, raw: &str) -> String {
    match raw {
        "3" => fl!("print-quality-draft"),
        "4" => fl!("print-quality-normal"),
        "5" => fl!("print-quality-high"),
        other => backend_label(dialog, "print-quality", other),
    }
}

fn page_range_to_zero_based(input: &str) -> String {
    input
        .split(',')
        .map(|part| {
            let part = part.trim();
            if let Some((start, end)) = part.split_once('-') {
                let s: u32 = start.trim().parse().unwrap_or(1);
                let e: u32 = end.trim().parse().unwrap_or(1);
                format!("{}-{}", s.saturating_sub(1), e.saturating_sub(1))
            } else {
                let n: u32 = part.parse().unwrap_or(1);
                n.saturating_sub(1).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

// Convert 0-based XDG page ranges to 1-based for display in the dialog.
fn page_range_to_one_based(input: &str) -> String {
    input
        .split(',')
        .map(|part| {
            let part = part.trim();
            if let Some((start, end)) = part.split_once('-') {
                let s: u32 = start.trim().parse().unwrap_or(0);
                let e: u32 = end.trim().parse().unwrap_or(0);
                format!("{}-{}", s + 1, e + 1)
            } else {
                let n: u32 = part.parse().unwrap_or(0);
                (n + 1).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn to_owned_value<T: Into<zvariant::Value<'static>>>(v: T) -> zvariant::OwnedValue {
    zvariant::OwnedValue::try_from(v.into()).unwrap()
}

fn get_str(map: &HashMap<String, zvariant::OwnedValue>, key: &str) -> Option<String> {
    map.get(key).and_then(|v| String::try_from(v.clone()).ok())
}

fn get_f64(map: &HashMap<String, zvariant::OwnedValue>, key: &str) -> Option<f64> {
    map.get(key).and_then(|v| f64::try_from(v.clone()).ok())
}

fn sanitize_index(index: Option<usize>, len: usize) -> Option<usize> {
    match index {
        Some(i) if i < len => Some(i),
        _ if len > 0 => Some(0),
        _ => None,
    }
}

// The application calling `PreparePrint` passes hints for the initial state of the dialog.
// This function would change the initial print dialog state according to the settings and
// page_setup options passed by the application.
pub fn apply_xdg_hints(
    dialog: &mut PrintDialog,
    settings: &HashMap<String, zvariant::OwnedValue>,
    page_setup: &HashMap<String, zvariant::OwnedValue>,
    accept_label: Option<String>,
) {
    // accept label
    dialog.accept_label = accept_label;

    // n-copies
    if let Some(s) = get_str(settings, "n-copies")
        && let Ok(n) = s.parse::<u32>()
    {
        dialog.copies = n;
    }

    // use-color
    if let Some(s) = get_str(settings, "use-color") {
        dialog.color_mode = if s == "true" {
            ColorMode::Color
        } else {
            ColorMode::Monochrome
        };
    }

    // collate
    if let Some(s) = get_str(settings, "collate") {
        dialog.collate = s == "true";
    }

    // reverse
    if let Some(s) = get_str(settings, "reverse") {
        dialog.reverse_order = s == "true";
    }

    // orientation
    if let Some(s) = get_str(settings, "orientation") {
        dialog.orientation = match s.as_str() {
            "landscape" | "reverse_landscape" => Orientation::Landscape,
            _ => Orientation::Portrait,
        };
    }

    if let Some(s) = get_str(page_setup, "Orientation") {
        dialog.orientation = match s.as_str() {
            "landscape" | "reverse-landscape" => Orientation::Landscape,
            _ => Orientation::Portrait,
        };
    }

    // duplex
    if let Some(s) = get_str(settings, "duplex") {
        let cpdb = match s.as_str() {
            "horizontal" => "two-sided-long-edge",
            "vertical" => "two-sided-short-edge",
            _ => "one-sided",
        };
        dialog.duplex_index = dialog.duplex_values.iter().position(|v| v == cpdb);
    }

    // quality
    if let Some(s) = get_str(settings, "quality") {
        let cpdb = match s.as_str() {
            "high" => "5",
            "draft" | "low" => "3",
            _ => "4",
        };
        dialog.print_quality_index = dialog.print_quality_values.iter().position(|v| v == cpdb);
    }

    // scale
    if let Some(s) = get_str(settings, "scale")
        && let Ok(n) = s.parse::<u32>()
    {
        if n == 100 {
            dialog.scaling = ScalingMode::Auto;
        } else {
            dialog.scaling = ScalingMode::Custom;
            dialog.custom_scaling_input = n;
        }
    }

    // number-up
    if let Some(s) = get_str(settings, "number-up")
        && let Ok(n) = s.parse::<u32>()
    {
        let pps = [1u32, 2, 4, 6, 9, 16];
        dialog.pages_per_sheet_index = pps.iter().position(|&v| v == n);
    }

    // number-up-layout
    if let Some(s) = get_str(settings, "number-up-layout") {
        dialog.layout_direction = match s.as_str() {
            "rltb" | "rlbt" => LayoutDirection::RightToLeftTopToBottom,
            "tblr" | "btlr" => LayoutDirection::TopToBottomLeftToRight,
            "tbrl" | "btrl" => LayoutDirection::TopToBottomRightToLeft,
            _ => LayoutDirection::LeftToRightTopToBottom,
        };
    }

    if let Some(s) = get_str(settings, "page-set") {
        dialog.page_selection = match s.as_str() {
            "even" => PageSetSelection::Even,
            "odd" => PageSetSelection::Odd,
            _ => PageSetSelection::All,
        };
    }

    if let Some(s) = get_str(settings, "print-pages") {
        match s.as_str() {
            "current" => dialog.page_selection = PageSetSelection::Current,
            "ranges" => {
                if let Some(ranges) = get_str(settings, "page-ranges") {
                    // ranges in XDG portal are 0-based, convert to 1-based for dialog UI
                    let display = page_range_to_one_based(&ranges);
                    dialog.custom_range_input = display.clone();
                    dialog.custom_range_valid = true;
                    dialog.page_selection = PageSetSelection::Custom(display);
                }
            }
            _ => {}
        }
    }

    // default-source (paper tray)
    if let Some(s) = get_str(settings, "default-source") {
        dialog.paper_tray_index = dialog.media_source_values.iter().position(|v| v == &s);
    }

    // media-type
    if let Some(s) = get_str(settings, "media-type") {
        dialog.paper_type_index = dialog.media_type_values.iter().position(|v| v == &s);
    }

    // paper-format (paper size index)
    if let Some(name) = get_str(settings, "paper-format")
        && let Some(media) = &dialog.printer_media
    {
        dialog.selected_paper_size_index = media.media.iter().position(|m| m.name == name);
    }

    // margins
    if let (Some(t), Some(b), Some(l), Some(r)) = (
        get_f64(page_setup, "MarginTop"),
        get_f64(page_setup, "MarginBottom"),
        get_f64(page_setup, "MarginLeft"),
        get_f64(page_setup, "MarginRight"),
    ) {
        if t == 0.0 && b == 0.0 && l == 0.0 && r == 0.0 {
            dialog.margins = Margins::None;
        } else {
            let tc = (t * 100.0).round() as u32;
            let lc = (l * 100.0).round() as u32;
            let mo = &dialog.margin_options;
            if mo.top.first() == Some(&tc) && mo.left.first() == Some(&lc) {
                dialog.margins = Margins::Minimum;
            } else {
                dialog.margins = Margins::Custom;
                dialog.custom_margins_vertical_index = mo.top.iter().position(|&v| v == tc);
                dialog.custom_margins_horizontal_index = mo.left.iter().position(|&v| v == lc);
            }
        }
    }

    // App-specific keys, Firefox seems to be using `gtk-print-backgrounds`
    // and `gtk-print-header-footer`
    if let Some(val) = settings.get("gtk-print-backgrounds")
        && let Ok(b) = bool::try_from(val.clone())
    {
        dialog.show_print_background_toggle = true;
        dialog.print_background = b;
    }
    if let Some(val) = settings.get("gtk-print-header-footer")
        && let Ok(b) = bool::try_from(val.clone())
    {
        dialog.show_print_header_footer_toggle = true;
        dialog.print_header_footer = b;
    }
}

/// Maps dialog state to setting keys expected by the XDG portal
pub fn build_xdg_response(
    dialog: &PrintDialog,
) -> (
    HashMap<String, zvariant::OwnedValue>,
    HashMap<String, zvariant::OwnedValue>,
) {
    // NOTE: the expected key names for options in the XDG Print portal and CPDB print backends
    // are not consistent. Here the print dialog acts as the source of truth, and we translate the
    // state into option names that the xdg portal expects.
    let mut settings: HashMap<String, zvariant::OwnedValue> = HashMap::new();
    let mut page_setup: HashMap<String, zvariant::OwnedValue> = HashMap::new();

    // use-color
    let use_color = if dialog.color_mode == ColorMode::Color {
        "true"
    } else {
        "false"
    };
    settings.insert("use-color".into(), to_owned_value(use_color.to_string()));

    // duplex
    let duplex_xdg = dialog
        .duplex_index
        .and_then(|i| dialog.duplex_values.get(i))
        .map(|v| match v.as_str() {
            "two-sided-long-edge" => "horizontal",
            "two-sided-short-edge" => "vertical",
            _ => "simplex",
        })
        .unwrap_or("simplex");
    settings.insert("duplex".into(), to_owned_value(duplex_xdg.to_string()));

    // n-copies
    settings.insert("n-copies".into(), to_owned_value(dialog.copies.to_string()));

    // collate
    let collate = if dialog.collate { "true" } else { "false" };
    settings.insert("collate".into(), to_owned_value(collate.to_string()));

    // reverse
    let reverse = if dialog.reverse_order {
        "true"
    } else {
        "false"
    };
    settings.insert("reverse".into(), to_owned_value(reverse.to_string()));

    // orientation
    let orientation_s = match dialog.orientation {
        Orientation::Portrait => "portrait",
        Orientation::Landscape => "landscape",
    };
    settings.insert(
        "orientation".into(),
        to_owned_value(orientation_s.to_string()),
    );

    // quality
    let quality_xdg = dialog
        .print_quality_index
        .and_then(|i| dialog.print_quality_values.get(i))
        .map(|v| match v.as_str() {
            "5" => "high",
            "3" => "draft",
            _ => "normal",
        })
        .unwrap_or("normal");
    settings.insert("quality".into(), to_owned_value(quality_xdg.to_string()));

    // scale
    let scale_str = match dialog.scaling {
        ScalingMode::Custom => dialog.custom_scaling_input.to_string(),
        _ => "100".to_string(),
    };
    settings.insert("scale".into(), to_owned_value(scale_str));

    // number-up
    let pps = [1u32, 2, 4, 6, 9, 16];
    let pps_val = dialog
        .pages_per_sheet_index
        .and_then(|i| pps.get(i))
        .copied()
        .unwrap_or(1);
    settings.insert("number-up".into(), to_owned_value(pps_val.to_string()));

    // number-up-layout
    settings.insert(
        "number-up-layout".into(),
        to_owned_value(dialog.layout_direction.id().to_string()),
    );

    // print-pages / page-ranges / page-set
    match &dialog.page_selection {
        PageSetSelection::All => {
            settings.insert("print-pages".into(), to_owned_value("all".to_string()));
        }
        PageSetSelection::Current => {
            settings.insert("print-pages".into(), to_owned_value("current".to_string()));
        }
        PageSetSelection::Odd => {
            settings.insert("print-pages".into(), to_owned_value("ranges".to_string()));
            settings.insert("page-set".into(), to_owned_value("odd".to_string()));
        }
        PageSetSelection::Even => {
            settings.insert("print-pages".into(), to_owned_value("ranges".to_string()));
            settings.insert("page-set".into(), to_owned_value("even".to_string()));
        }
        PageSetSelection::Custom(val) => {
            settings.insert("print-pages".into(), to_owned_value("ranges".to_string()));
            settings.insert(
                "page-ranges".into(),
                to_owned_value(page_range_to_zero_based(val)),
            );
        }
    }

    // default-source
    if let Some(idx) = dialog.paper_tray_index
        && let Some(val) = dialog.media_source_values.get(idx)
    {
        settings.insert("default-source".into(), to_owned_value(val.clone()));
    }

    // media-type
    if let Some(idx) = dialog.paper_type_index
        && let Some(val) = dialog.media_type_values.get(idx)
    {
        settings.insert("media-type".into(), to_owned_value(val.clone()));
    }

    // paper-format / paper-width / paper-height
    if let Some(idx) = dialog.selected_paper_size_index
        && let Some(m) = dialog
            .printer_media
            .as_ref()
            .and_then(|mc| mc.media.get(idx))
    {
        settings.insert("paper-format".into(), to_owned_value(m.name.clone()));
        settings.insert(
            "paper-width".into(),
            to_owned_value((m.width as f64 / 100.0).to_string()),
        );
        settings.insert(
            "paper-height".into(),
            to_owned_value((m.length as f64 / 100.0).to_string()),
        );
        page_setup.insert("PPDName".into(), to_owned_value(m.name.clone()));
        page_setup.insert("Name".into(), to_owned_value(m.name.clone()));
        page_setup.insert("DisplayName".into(), to_owned_value(m.name.clone()));
        page_setup.insert("Width".into(), to_owned_value(m.width as f64 / 100.0));
        page_setup.insert("Height".into(), to_owned_value(m.length as f64 / 100.0));
    }

    // orientation for page_setup
    let orientation_ps = match dialog.orientation {
        Orientation::Portrait => "portrait",
        Orientation::Landscape => "landscape",
    };
    page_setup.insert(
        "Orientation".into(),
        to_owned_value(orientation_ps.to_string()),
    );

    // margins
    let (top_mm, bottom_mm, left_mm, right_mm) = {
        let mo = &dialog.margin_options;
        match dialog.margins {
            Margins::None => (0.0f64, 0.0, 0.0, 0.0),
            Margins::Minimum => (
                mo.top.first().copied().unwrap_or(0) as f64 / 100.0,
                mo.bottom.first().copied().unwrap_or(0) as f64 / 100.0,
                mo.left.first().copied().unwrap_or(0) as f64 / 100.0,
                mo.right.first().copied().unwrap_or(0) as f64 / 100.0,
            ),
            Margins::Custom => {
                let v = dialog
                    .custom_margins_vertical_index
                    .and_then(|i| mo.top.get(i))
                    .copied()
                    .unwrap_or(0) as f64
                    / 100.0;
                let h = dialog
                    .custom_margins_horizontal_index
                    .and_then(|i| mo.left.get(i))
                    .copied()
                    .unwrap_or(0) as f64
                    / 100.0;
                (v, v, h, h)
            }
            Margins::Default => (0.0, 0.0, 0.0, 0.0),
        }
    };
    page_setup.insert("MarginTop".into(), to_owned_value(top_mm));
    page_setup.insert("MarginBottom".into(), to_owned_value(bottom_mm));
    page_setup.insert("MarginLeft".into(), to_owned_value(left_mm));
    page_setup.insert("MarginRight".into(), to_owned_value(right_mm));

    // App-specific keys, Firefox seems to be using `gtk-print-backgrounds`
    // and `gtk-print-header-footer`
    if dialog.show_print_background_toggle {
        settings.insert(
            "gtk-print-backgrounds".into(),
            to_owned_value(dialog.print_background),
        );
    }
    if dialog.show_print_header_footer_toggle {
        settings.insert(
            "gtk-print-header-footer".into(),
            to_owned_value(dialog.print_header_footer),
        );
    }

    (settings, page_setup)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavePdfResult {
    Saved,
    Cancelled,
    Failed,
}

pub fn sanitize_pdf_filename(title: &str) -> String {
    let sanitized = title.replace('/', "_");
    if sanitized.trim().is_empty() {
        "document.pdf".to_string()
    } else if sanitized.to_lowercase().ends_with(".pdf") {
        sanitized
    } else {
        format!("{sanitized}.pdf")
    }
}

pub(crate) async fn save_pdf_to_file(title: &str, fd: &OwnedFd) -> SavePdfResult {
    let default_filename = sanitize_pdf_filename(title);
    let pdf_document_label = fl!("pdf-document");
    let save_title = fl!("save-pdf-document");
    let save_label = fl!("save");
    let pdf_filter = file_chooser::FileFilter::new(pdf_document_label.as_str())
        .mimetype("application/pdf")
        .glob("*.pdf");

    let save_request = file_chooser::SaveFileRequest::default()
        .title(save_title.as_str())
        .accept_label(Some(save_label.as_str()))
        .modal(true)
        .current_name(Some(default_filename.as_str()))
        .filter(pdf_filter);

    let selected_files = match save_request.send().await {
        Ok(request) => match request.response() {
            Ok(files) => files,
            Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => {
                return SavePdfResult::Cancelled;
            }
            Err(e) => {
                tracing::error!("File chooser response error: {e:?}");
                return SavePdfResult::Failed;
            }
        },
        Err(e) => {
            tracing::error!("Failed to send SaveFileRequest: {e:?}");
            return SavePdfResult::Failed;
        }
    };

    let target_path = if let Some(url) = selected_files.uris().first() {
        if let Ok(path) = url.to_file_path() {
            path
        } else {
            tracing::error!("Failed to convert URL to file path: {url}");
            return SavePdfResult::Failed;
        }
    } else {
        tracing::error!("No URI returned from file chooser");
        return SavePdfResult::Cancelled;
    };

    let readable_fd = match fd.as_fd().try_clone_to_owned() {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Failed to clone document FD: {e:?}");
            return SavePdfResult::Failed;
        }
    };

    let path_for_closure = target_path.clone();
    let copy_result = tokio::task::spawn_blocking(move || {
        let mut reader = File::from(readable_fd);
        let mut writer = File::create(&path_for_closure)?;
        copy(&mut reader, &mut writer)
    })
    .await;

    match copy_result {
        Ok(Ok(bytes)) => {
            tracing::debug!(
                "Successfully saved {bytes} bytes to PDF file: {:?}",
                target_path
            );
            SavePdfResult::Saved
        }
        Ok(Err(e)) => {
            tracing::error!("Failed saving document to file: {e:?}");
            SavePdfResult::Failed
        }
        Err(e) => {
            tracing::error!("Task join error during PDF save: {e:?}");
            SavePdfResult::Failed
        }
    }
}

pub(crate) async fn do_print_execution(
    printer_id: String,
    backend: String,
    settings: Vec<(String, String)>,
    title: String,
    fd: Arc<OwnedFd>,
) -> PortalResponse<PrintResult> {
    if is_pdf_printer_by_id(&printer_id, &backend) {
        return match save_pdf_to_file(&title, &fd).await {
            SavePdfResult::Saved => PortalResponse::Success(PrintResult {
                settings: HashMap::new(),
            }),
            SavePdfResult::Cancelled => PortalResponse::Cancelled,
            SavePdfResult::Failed => PortalResponse::Other,
        };
    }

    let client = match CpdbClient::new().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create CPDB client for printing: {e:?}");
            return PortalResponse::Other;
        }
    };

    let settings_ref: Vec<(&str, &str)> = settings
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // CPDB backends (e.g. CUPS) require a printer query to populate internal tables before printing
    let _ = client.get_all_printers().await;

    tracing::debug!("Submitting print job to printer '{printer_id}' via backend '{backend}'");
    let (job_id, cpdb_writable_fd) = match client
        .print_fd(&printer_id, &backend, &settings_ref, &title)
        .await
    {
        Ok(res) => res,
        Err(e) => {
            tracing::error!("Failed to submit print job via CPDB client: {e:?}");
            return PortalResponse::Other;
        }
    };
    tracing::debug!("Print job submitted successfully. Job Id: {job_id}");

    let readable_fd = match fd.as_fd().try_clone_to_owned() {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Failed to clone document FD: {e:?}");
            return PortalResponse::Other;
        }
    };

    let copy_result = tokio::task::spawn_blocking(move || {
        let mut reader = File::from(readable_fd);
        let mut writer = File::from(OwnedFd::from(cpdb_writable_fd));
        copy(&mut reader, &mut writer)
    })
    .await;

    match copy_result {
        Ok(Ok(bytes)) => {
            tracing::debug!("Copied {bytes} bytes of document data to CPDB");
            PortalResponse::Success(PrintResult {
                settings: HashMap::new(),
            })
        }
        Ok(Err(e)) => {
            tracing::error!("Failed copying document data to CPDB: {e:?}");
            PortalResponse::Other
        }
        Err(e) => {
            tracing::error!("{e:?}");
            PortalResponse::Other
        }
    }
}

/// Maps dialog state to setting keys expected by the CPDB backends
pub fn build_cpdb_settings(dialog: &PrintDialog) -> Vec<(String, String)> {
    // NOTE: the expected key names for options in the XDG Print portal and CPDB print backends
    // are not consistent. Here the print dialog's state acts as the source of truth, and we
    // translate the state into option names that the CPDB backends understand.

    let mut out: Vec<(String, String)> = Vec::new();

    // print-color-mode
    out.push(("print-color-mode".into(), dialog.color_mode.id().into()));

    // sides
    if let Some(idx) = dialog.duplex_index
        && let Some(val) = dialog.duplex_values.get(idx)
    {
        out.push(("sides".into(), val.clone()));
    }

    // copies
    out.push(("copies".into(), dialog.copies.to_string()));

    // collate
    out.push((
        "multiple-document-handling".into(),
        if dialog.collate {
            "separate-documents-collated-copies".into()
        } else {
            "separate-documents-uncollated-copies".into()
        },
    ));

    // page-delivery
    out.push((
        "page-delivery".into(),
        if dialog.reverse_order {
            "reverse-order"
        } else {
            "same-order"
        }
        .into(),
    ));

    // print-quality
    if let Some(idx) = dialog.print_quality_index
        && let Some(val) = dialog.print_quality_values.get(idx)
    {
        out.push(("print-quality".into(), val.clone()));
    }

    // print-scaling
    out.push(("print-scaling".into(), dialog.scaling.as_cpdb_str().into()));

    // number-up
    let pps = [1u32, 2, 4, 6, 9, 16];
    if let Some(&v) = dialog.pages_per_sheet_index.and_then(|i| pps.get(i)) {
        out.push(("number-up".into(), v.to_string()));
    }

    // number-up-layout
    out.push((
        "number-up-layout".into(),
        dialog.layout_direction.id().into(),
    ));

    // page-border
    out.push(("page-border".into(), dialog.border.id().into()));

    // media
    if let Some(idx) = dialog.selected_paper_size_index
        && let Some(m) = dialog
            .printer_media
            .as_ref()
            .and_then(|mc| mc.media.get(idx))
    {
        out.push(("media".into(), m.name.clone()));
    }

    // media-source / media-type
    if let Some(idx) = dialog.paper_tray_index
        && let Some(val) = dialog.media_source_values.get(idx)
    {
        out.push(("media-source".into(), val.clone()));
    }
    if let Some(idx) = dialog.paper_type_index
        && let Some(val) = dialog.media_type_values.get(idx)
    {
        out.push(("media-type".into(), val.clone()));
    }

    // margins
    let mo = &dialog.margin_options;
    match dialog.margins {
        Margins::None if mo.supports_borderless => {
            for key in [
                "media-top-margin",
                "media-bottom-margin",
                "media-left-margin",
                "media-right-margin",
            ] {
                out.push((key.into(), "0".into()));
            }
        }
        Margins::Minimum => {
            if let Some(&v) = mo.top.first() {
                out.push(("media-top-margin".into(), v.to_string()));
            }
            if let Some(&v) = mo.bottom.first() {
                out.push(("media-bottom-margin".into(), v.to_string()));
            }
            if let Some(&v) = mo.left.first() {
                out.push(("media-left-margin".into(), v.to_string()));
            }
            if let Some(&v) = mo.right.first() {
                out.push(("media-right-margin".into(), v.to_string()));
            }
        }
        Margins::Custom => {
            if let Some(v) = dialog
                .custom_margins_vertical_index
                .and_then(|i| mo.top.get(i))
                .copied()
            {
                out.push(("media-top-margin".into(), v.to_string()));
                out.push(("media-bottom-margin".into(), v.to_string()));
            }
            if let Some(v) = dialog
                .custom_margins_horizontal_index
                .and_then(|i| mo.left.get(i))
                .copied()
            {
                out.push(("media-left-margin".into(), v.to_string()));
                out.push(("media-right-margin".into(), v.to_string()));
            }
        }
        _ => {}
    }

    out
}

pub fn sync_print_models(portal: &mut CosmicPortal) {
    if let Some(args) = &portal.print_args {
        let dialog = &args.dialog;

        let mut color_model = segmented_button::Model::builder()
            .insert(|b| b.text(ColorMode::Color.label()).data(ColorMode::Color))
            .insert(|b| {
                b.text(ColorMode::Monochrome.label())
                    .data(ColorMode::Monochrome)
            })
            .build();
        let color_active = color_model
            .iter()
            .find(|&id| color_model.data::<ColorMode>(id) == Some(&dialog.color_mode));
        if let Some(entity) = color_active {
            color_model.activate(entity);
        }
        let color_entity_opt = color_model
            .iter()
            .find(|&id| color_model.data::<ColorMode>(id) == Some(&ColorMode::Color));
        if let Some(color_entity) = color_entity_opt {
            color_model.enable(color_entity, dialog.color_supported);
        }
        portal.print_color_model = color_model;

        let mut orientation_model = segmented_button::Model::builder()
            .insert(|b| {
                b.text(Orientation::Portrait.label())
                    .data(Orientation::Portrait)
            })
            .insert(|b| {
                b.text(Orientation::Landscape.label())
                    .data(Orientation::Landscape)
            })
            .build();
        let orientation_active = orientation_model
            .iter()
            .find(|&id| orientation_model.data::<Orientation>(id) == Some(&dialog.orientation));
        if let Some(entity) = orientation_active {
            orientation_model.activate(entity);
        }
        portal.print_orientation_model = orientation_model;

        let mut layout_direction_model = segmented_button::Model::builder()
            .insert(|b| {
                b.icon(icon::from_name(
                    LayoutDirection::LeftToRightTopToBottom.icon_name(),
                ))
                .data(LayoutDirection::LeftToRightTopToBottom)
            })
            .insert(|b| {
                b.icon(icon::from_name(
                    LayoutDirection::RightToLeftTopToBottom.icon_name(),
                ))
                .data(LayoutDirection::RightToLeftTopToBottom)
            })
            .insert(|b| {
                b.icon(icon::from_name(
                    LayoutDirection::TopToBottomLeftToRight.icon_name(),
                ))
                .data(LayoutDirection::TopToBottomLeftToRight)
            })
            .insert(|b| {
                b.icon(icon::from_name(
                    LayoutDirection::TopToBottomRightToLeft.icon_name(),
                ))
                .data(LayoutDirection::TopToBottomRightToLeft)
            })
            .build();
        let layout_active = layout_direction_model.iter().find(|&id| {
            layout_direction_model.data::<LayoutDirection>(id) == Some(&dialog.layout_direction)
        });
        if let Some(entity) = layout_active {
            layout_direction_model.activate(entity);
        }
        portal.print_layout_direction_model = layout_direction_model;

        let mut page_selection_model = segmented_button::Model::builder()
            .insert(|b| {
                b.text(PageSetSelection::All.label())
                    .data(PageSetSelection::All)
            })
            .insert(|b| {
                b.text(PageSetSelection::Current.label())
                    .data(PageSetSelection::Current)
            })
            .insert(|b| {
                b.text(PageSetSelection::Odd.label())
                    .data(PageSetSelection::Odd)
            })
            .insert(|b| {
                b.text(PageSetSelection::Even.label())
                    .data(PageSetSelection::Even)
            })
            .insert(|b| {
                b.text(PageSetSelection::Custom(String::new()).label())
                    .data(PageSetSelection::Custom(dialog.custom_range_input.clone()))
            })
            .build();
        let page_active = page_selection_model.iter().find(|&id| {
            if let Some(data) = page_selection_model.data::<PageSetSelection>(id) {
                matches!(
                    (data, &dialog.page_selection),
                    (PageSetSelection::All, PageSetSelection::All)
                        | (PageSetSelection::Current, PageSetSelection::Current)
                        | (PageSetSelection::Odd, PageSetSelection::Odd)
                        | (PageSetSelection::Even, PageSetSelection::Even)
                        | (PageSetSelection::Custom(_), PageSetSelection::Custom(_))
                )
            } else {
                false
            }
        });
        if let Some(entity) = page_active {
            page_selection_model.activate(entity);
        }
        portal.print_page_selection_model = page_selection_model;
    }
}
