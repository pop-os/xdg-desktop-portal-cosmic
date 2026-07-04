use cosmic::iced::font::Weight;
use cosmic::iced::widget::Stack;
use cosmic::iced::{Alignment, Background, Color, Font, Length, Padding, Subscription};
use cosmic::theme::{Button, SegmentedButton, Text};
use cosmic::widget::segmented_button::SingleSelect;
use cosmic::widget::{
    self, button, column, container, divider, dropdown, icon, row, segmented_button, space, text,
    text_input, toggler,
};
use cosmic::{Element, Task, font, theme};
use cpdb_rs::client::CpdbClient;
use cpdb_rs::media::MediaCollection;
use cpdb_rs::options::{OptionInfo, OptionsCollection};
use cpdb_rs::{DiscoveryEvent, PrinterSnapshot};
use futures::SinkExt;
use futures::StreamExt;
use futures::channel::mpsc;
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use zbus::zvariant;

use crate::app::CosmicPortal;

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
    pub fn to_string_label(&self) -> String {
        match self {
            PageSetSelection::All => "All pages".to_string(),
            PageSetSelection::Current => "Current page".to_string(),
            PageSetSelection::Odd => "Odd pages only".to_string(),
            PageSetSelection::Even => "Even pages only".to_string(),
            PageSetSelection::Custom(val) => {
                if val.trim().is_empty() {
                    "Custom range".to_string()
                } else {
                    format!("Custom, {}", val)
                }
            }
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
    pub is_discovering: bool,
    pub printers: Vec<PrinterSnapshot>,
    pub selected_printer_index: Option<usize>,
    pub printer_options: Option<OptionsCollection>,
    pub printer_media: Option<MediaCollection>,

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Color,
    Monochrome,
}

impl ColorMode {
    pub fn as_cpdb_str(&self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Monochrome => "monochrome",
        }
    }

    pub fn is_color(&self) -> bool {
        *self == Self::Color
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Portrait,
    Landscape,
}

impl Orientation {
    pub fn is_portrait(&self) -> bool {
        *self == Self::Portrait
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
    pub fn as_cpdb_str(&self) -> &'static str {
        match self {
            Self::LeftToRightTopToBottom => "lrtb",
            Self::RightToLeftTopToBottom => "rltb",
            Self::TopToBottomLeftToRight => "tblr",
            Self::TopToBottomRightToLeft => "tbrl",
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
    pub fn label(&self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::None => "None",
            Self::Minimum => "Minimum",
            Self::Custom => "Custom",
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
    pub fn label(&self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Single => "Single",
            Self::Double => "Double",
        }
    }

    pub fn as_cpdb_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Single => "single",
            Self::Double => "double",
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
    pub fn label(&self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::AutoFit => "Auto fit",
            Self::Fit => "Fit to page",
            Self::Fill => "Fill page",
            Self::Custom => "Custom",
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

impl Default for PrintDialog {
    fn default() -> Self {
        Self {
            is_discovering: true,
            printers: Vec::new(),
            selected_printer_index: None,
            printer_options: None,
            printer_media: None,
            active_view: ActiveView::Main,
            page_selection: PageSetSelection::All,
            custom_range_input: String::new(),
            custom_range_valid: true,
            copies: 1,
            collate: false,
            selected_paper_size_index: None,
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
            margin_options: MarginOptions::default(),
            custom_margins_vertical_index: None,
            custom_margins_horizontal_index: None,
            border: Border::None,
            scaling: ScalingMode::Auto,
            custom_scaling_input: 100,
            show_print_header_footer_toggle: false,
            print_header_footer: false,
            show_print_background_toggle: false,
            print_background: false,
            reverse_order: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    PrintersLoaded(Vec<PrinterSnapshot>),
    DiscoveryEvent(DiscoveryEvent),
    PrinterSelected(usize),
    PrinterDetailsLoaded(OptionsCollection, MediaCollection),

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
}

impl PrintDialog {
    pub fn subscription(&self) -> Subscription<Msg> {
        if self.is_discovering {
            Subscription::run_with(PrinterDiscovery, |_| {
                cosmic::iced::stream::channel(100, |mut output: mpsc::Sender<Msg>| async move {
                    log::debug!("Starting CPDB printer discovery subscription stream");
                    let client = match CpdbClient::new().await {
                        Ok(c) => c,
                        Err(e) => {
                            log::error!("Failed to create CPDB client: {:?}", e);
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
}

fn fetch_printer_details(printer: &PrinterSnapshot) -> Task<Msg> {
    let printer_id = printer.id.clone();
    let backend = printer.backend.clone();
    Task::perform(
        async move {
            let client = match CpdbClient::new().await {
                Ok(c) => c,
                Err(e) => {
                    log::error!("Failed to create client for details fetch: {:?}", e);
                    return Msg::PrinterDetailsLoaded(
                        OptionsCollection::default(),
                        MediaCollection::default(),
                    );
                }
            };

            // Call get_all_printers first so the CPDB backend populates the printer list
            let _ = client.get_all_printers().await;
            match client.get_printer_details(&printer_id, &backend).await {
                Ok((opts, media)) => Msg::PrinterDetailsLoaded(opts, media),
                Err(e) => {
                    log::error!("Failed to fetch printer details: {:?}", e);
                    Msg::PrinterDetailsLoaded(
                        OptionsCollection::default(),
                        MediaCollection::default(),
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
            dialog.printers = printers;
            if dialog.selected_printer_index.is_none() && !dialog.printers.is_empty() {
                dialog.selected_printer_index = Some(0);
                return fetch_printer_details(&dialog.printers[0]);
            }
        }
        Msg::DiscoveryEvent(event) => match event {
            DiscoveryEvent::PrinterAdded(snap) => {
                if let Some(pos) = dialog
                    .printers
                    .iter()
                    .position(|p| p.id == snap.id && p.backend == snap.backend)
                {
                    dialog.printers[pos] = snap;
                } else {
                    dialog.printers.push(snap);
                }
                if dialog.selected_printer_index.is_none() && !dialog.printers.is_empty() {
                    dialog.selected_printer_index = Some(0);
                    return fetch_printer_details(&dialog.printers[0]);
                }
            }
            DiscoveryEvent::PrinterRemoved { id, backend } => {
                dialog
                    .printers
                    .retain(|p| !(p.id == id && p.backend == backend));
                if let Some(sel) = dialog.selected_printer_index
                    && sel >= dialog.printers.len()
                {
                    if dialog.printers.is_empty() {
                        dialog.selected_printer_index = None;
                        dialog.printer_options = None;
                        dialog.printer_media = None;
                    } else {
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
        },
        Msg::PrinterSelected(index) => {
            if index < dialog.printers.len() {
                dialog.selected_printer_index = Some(index);
                return fetch_printer_details(&dialog.printers[index]);
            }
        }
        Msg::PrinterDetailsLoaded(options, media) => {
            dialog.color_supported = options
                .get("print-color-mode")
                .map(|o| o.supported_values.iter().any(|v| v == "color"))
                .unwrap_or(false);
            if !dialog.color_supported {
                dialog.color_mode = ColorMode::Monochrome;
            }

            // sides (duplex)
            if let Some(opt) = options.get("sides") {
                dialog.duplex_values = clean_supported_values(&opt.supported_values);
                dialog.duplex_index = dialog
                    .duplex_values
                    .iter()
                    .position(|v| *v == opt.default_value)
                    .or({
                        if dialog.duplex_values.is_empty() {
                            None
                        } else {
                            Some(0)
                        }
                    });
            } else {
                dialog.duplex_values = Vec::new();
                dialog.duplex_index = None;
            }

            // media-source (paper tray)
            if let Some(opt) = options.get("media-source") {
                dialog.media_source_values = clean_supported_values(&opt.supported_values);
                dialog.paper_tray_index = dialog
                    .media_source_values
                    .iter()
                    .position(|v| *v == opt.default_value)
                    .or({
                        if dialog.media_source_values.is_empty() {
                            None
                        } else {
                            Some(0)
                        }
                    });
            } else {
                dialog.media_source_values = Vec::new();
                dialog.paper_tray_index = None;
            }

            // media-type (paper type)
            if let Some(opt) = options.get("media-type") {
                dialog.media_type_values = clean_supported_values(&opt.supported_values);
                dialog.paper_type_index = dialog
                    .media_type_values
                    .iter()
                    .position(|v| *v == opt.default_value)
                    .or({
                        if dialog.media_type_values.is_empty() {
                            None
                        } else {
                            Some(0)
                        }
                    });
            } else {
                dialog.media_type_values = Vec::new();
                dialog.paper_type_index = None;
            }

            // print-quality
            if let Some(opt) = options.get("print-quality") {
                dialog.print_quality_values = clean_supported_values(&opt.supported_values);
                dialog.print_quality_index = dialog
                    .print_quality_values
                    .iter()
                    .position(|v| *v == opt.default_value)
                    .or({
                        if dialog.print_quality_values.is_empty() {
                            None
                        } else {
                            Some(0)
                        }
                    });
            } else {
                dialog.print_quality_values = Vec::new();
                dialog.print_quality_index = None;
            }

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
        }
        Msg::NavigateTo(view) => {
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
        Msg::Cancel
        | Msg::Confirm
        | Msg::EnterPressed
        | Msg::EscapePressed
        | Msg::PageSelectionModelActivated(_)
        | Msg::ColorModelActivated(_)
        | Msg::OrientationModelActivated(_)
        | Msg::LayoutDirectionModelActivated(_) => {}
    }
    cosmic::Task::none()
}

fn option_row<'a>(
    label: &'a str,
    control: impl Into<cosmic::Element<'a, Msg>>,
) -> Element<'a, Msg> {
    row![text(label), space::horizontal(), control.into()]
        .align_y(Alignment::Center)
        .into()
}

fn disabled_placeholder<'a, Msg: 'static + Clone>(
    label: impl Into<Cow<'a, str>> + 'a,
) -> Element<'a, Msg> {
    let theme_spacing = theme::spacing();
    container(text(label).size(14).class(Text::Custom(|theme| {
        let mut color = theme.current_container().component.on;
        color.alpha *= 0.75;
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

fn option_group<'a>(title: Option<&'a str>, items: Vec<Element<'a, Msg>>) -> Element<'a, Msg> {
    let mut col = column![].spacing(8);
    if let Some(t) = title {
        col = col.push(text(t).size(14).font(Font {
            weight: Weight::Bold,
            ..Default::default()
        }));
    }

    let mut list = column![].spacing(12);
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            list = list.push(divider::horizontal::light());
        }
        list = list.push(item);
    }

    col = col.push(container(list).padding(16).width(Length::Fill));
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
                background: Some(Background::Color(theme.background.divider.into())),
                border_radius: 16.0.into(),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
                ..Default::default()
            }
        }),
        pressed: Box::new(|_focused, theme| {
            let theme = theme.cosmic();
            button::Style {
                background: Some(Background::Color(theme.background.divider.into())),
                border_radius: 16.0.into(),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
                ..Default::default()
            }
        }),
    })
    .into()
}

pub fn view<'a>(
    dialog: &'a PrintDialog,
    color_model: &'a segmented_button::Model<SingleSelect>,
    orientation_model: &'a segmented_button::Model<SingleSelect>,
    layout_direction_model: &'a segmented_button::Model<SingleSelect>,
    page_selection_model: &'a segmented_button::Model<SingleSelect>,
) -> Element<'a, Msg> {
    let content = match dialog.active_view {
        ActiveView::Main => view_main(
            dialog,
            color_model,
            orientation_model,
            layout_direction_model,
        ),
        ActiveView::PageSelection => view_pages_selection(dialog, page_selection_model),
    };

    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn view_pages_selection<'a>(
    dialog: &'a PrintDialog,
    page_selection_model: &'a segmented_button::Model<SingleSelect>,
) -> Element<'a, Msg> {
    let title_col = column![
        button::standard("< Print")
            .class(Button::Text)
            .on_press(Msg::NavigateTo(ActiveView::Main)),
        space::horizontal(),
        text("Pages").size(20),
        space::horizontal(),
    ]
    .align_x(Alignment::Start)
    .spacing(5)
    .padding(8);

    let segmented = segmented_button::vertical(page_selection_model)
        .style(SegmentedButton::Control)
        .button_alignment(Alignment::Start)
        .button_height(50)
        .font_size(15.0)
        .on_activate(Msg::PageSelectionModelActivated)
        .width(Length::Fill);

    let is_custom = matches!(dialog.page_selection, PageSetSelection::Custom(_));

    let mut list_items = vec![];

    if is_custom {
        let overlay_row = row![
            text_input("e.g. 1-5, 8, 11-13", &dialog.custom_range_input)
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
        let error_text = text("Invalid format: use numbers and ranges (e.g. 1-5, 8)")
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

    let list = option_group(None, list_items);

    column![title_col, container(list).padding(16)].into()
}

fn view_main<'a>(
    dialog: &'a PrintDialog,
    color_model: &'a segmented_button::Model<SingleSelect>,
    orientation_model: &'a segmented_button::Model<SingleSelect>,
    layout_direction_model: &'a segmented_button::Model<SingleSelect>,
) -> Element<'a, Msg> {
    let options = view_options_panel(
        dialog,
        color_model,
        orientation_model,
        layout_direction_model,
    );
    let status_bar = view_status_row(dialog);

    column![widget::scrollable(options).height(Length::Fill), status_bar].into()
}

fn view_options_panel<'a>(
    dialog: &'a PrintDialog,
    color_model: &'a segmented_button::Model<SingleSelect>,
    orientation_model: &'a segmented_button::Model<SingleSelect>,
    layout_direction_model: &'a segmented_button::Model<SingleSelect>,
) -> Element<'a, Msg> {
    let spacing = 16;
    let mut groups = column![].spacing(spacing);

    // Group 1: Destination and Presets
    let printer_names: Vec<String> = dialog.printers.iter().map(|p| p.name.clone()).collect();
    let dest_dropdown: Element<'_, Msg> = if printer_names.is_empty() {
        dropdown(vec!["No printers found".to_string()], Some(0), |_| {
            Msg::PrinterSelected(0)
        })
        .into()
    } else {
        dropdown(
            printer_names,
            dialog.selected_printer_index,
            Msg::PrinterSelected,
        )
        .into()
    };

    let preset_dropdown: Element<'_, Msg> =
        dropdown(vec!["Default preset".to_string()], Some(0), |_| {
            Msg::PrinterSelected(0)
        })
        .into();

    let top_group = option_group(
        None,
        vec![
            option_row("Destination", dest_dropdown),
            option_row("Preset", preset_dropdown),
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
        "Pages",
        button::standard(format!("{} >", dialog.page_selection.to_string_label()))
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
            option_row("Paper size", disabled_placeholder("Not supported"))
        } else if media.len() == 1 {
            option_row("Paper size", disabled_placeholder(&media.media[0].name))
        } else {
            let paper_sizes: Vec<String> = media.iter().map(|m| m.name.clone()).collect();
            let paper_size_dropdown = dropdown(
                paper_sizes,
                dialog.selected_paper_size_index,
                Msg::PaperSizeSelected,
            );
            option_row("Paper size", paper_size_dropdown)
        }
    } else {
        option_row("Paper size", disabled_placeholder("Not supported"))
    };

    let duplex_row: Option<Element<'_, Msg>> = if dialog.duplex_values.is_empty() {
        Some(option_row(
            "Print on sides",
            disabled_placeholder("Not supported"),
        ))
    } else if dialog.duplex_values.len() == 1 {
        fn duplex_label(raw: &str) -> &str {
            match raw {
                "one-sided" => "One side",
                "two-sided-long-edge" => "Both sides (book - bind on long edges)",
                "two-sided-short-edge" => "Both sides (notepad - bind on short edges)",
                other => other,
            }
        }
        Some(option_row(
            "Print on sides",
            disabled_placeholder(duplex_label(&dialog.duplex_values[0])),
        ))
    } else {
        fn sides_label(raw: &str) -> &str {
            match raw {
                "one-sided" => "One-sided",
                "two-sided-long-edge" => "Two-sided (long edge)",
                "two-sided-short-edge" => "Two-sided (short edge)",
                other => other,
            }
        }
        let labels: Vec<String> = dialog
            .duplex_values
            .iter()
            .map(|v| sides_label(v).to_string())
            .collect();
        Some(option_row(
            "Print on sides",
            dropdown(labels, dialog.duplex_index, Msg::DuplexSelected),
        ))
    };

    let mut primary_items = vec![
        color_slider.into(),
        orientation_slider.into(),
        pages_row,
        option_row("Copies", copies_control),
        option_row("Collate", collate_toggle),
        paper_size_row,
    ];
    if let Some(row) = duplex_row {
        primary_items.push(row);
    }
    let primary_group = option_group(None, primary_items);
    groups = groups.push(primary_group);

    // Group 3: Layout
    let pps_dropdown = dropdown(
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

    let margins_dropdown = dropdown(
        Margins::ALL
            .iter()
            .map(|m| m.label().to_string())
            .collect::<Vec<_>>(),
        Some(
            Margins::ALL
                .iter()
                .position(|m| *m == dialog.margins)
                .unwrap_or(0),
        ),
        |i| Msg::MarginsSelected(Margins::ALL[i]),
    );

    let mut layout_items = vec![
        option_row("Pages per sheet", pps_dropdown),
        row![text("Layout direction"), layout_dir_row]
            .spacing(16)
            .align_y(Alignment::Center)
            .into(),
        option_row("Margins", margins_dropdown),
    ];

    if dialog.margins == Margins::Custom {
        let to_mm = |v: u32| format!("{:.1} mm", v as f32 / 100.0);

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

        layout_items.push(option_row(
            "Top & bottom margin",
            dropdown(
                vt_labels,
                dialog.custom_margins_vertical_index,
                Msg::CustomMarginVtSelected,
            ),
        ));
        layout_items.push(option_row(
            "Left & right margin",
            dropdown(
                hz_labels,
                dialog.custom_margins_horizontal_index,
                Msg::CustomMarginHzSelected,
            ),
        ));
    }

    let border_dropdown = dropdown(
        Border::ALL
            .iter()
            .map(|b| b.label().to_string())
            .collect::<Vec<_>>(),
        Some(
            Border::ALL
                .iter()
                .position(|b| *b == dialog.border)
                .unwrap_or(0),
        ),
        |i| Msg::BorderSelected(Border::ALL[i]),
    );
    layout_items.push(option_row("Border", border_dropdown));

    let scaling_dropdown = dropdown(
        ScalingMode::ALL
            .iter()
            .map(|s| s.label().to_string())
            .collect::<Vec<_>>(),
        Some(
            ScalingMode::ALL
                .iter()
                .position(|s| *s == dialog.scaling)
                .unwrap_or(0),
        ),
        |i| Msg::ScalingSelected(ScalingMode::ALL[i]),
    );
    layout_items.push(option_row("Scaling", scaling_dropdown));

    if dialog.scaling == ScalingMode::Custom {
        let decrement_scaling_msg = if dialog.custom_scaling_input > 1 {
            Some(Msg::DecrementScaling)
        } else {
            None
        };
        let custom_scaling_control = row![
            counter_button("-", decrement_scaling_msg),
            text(format!("{}%", dialog.custom_scaling_input)).size(16),
            counter_button("+", Some(Msg::IncrementScaling)),
        ]
        .align_y(Alignment::Center)
        .spacing(12);
        layout_items.push(option_row("Scaling percentage", custom_scaling_control));
    }

    if dialog.show_print_header_footer_toggle {
        layout_items.push(option_row(
            "Print header and footer",
            toggler(dialog.print_header_footer).on_toggle(|_| Msg::TogglePrintHeaderFooter),
        ));
    }

    if dialog.show_print_background_toggle {
        layout_items.push(option_row(
            "Print background",
            toggler(dialog.print_background).on_toggle(|_| Msg::TogglePrintBackground),
        ));
    }

    let layout_group = option_group(Some("Layout"), layout_items);
    groups = groups.push(layout_group);

    // Group 4: Paper handling
    let tray_row: Option<Element<'_, Msg>> = if dialog.media_source_values.is_empty() {
        Some(option_row(
            "Paper tray",
            disabled_placeholder("Not supported"),
        ))
    } else if dialog.media_source_values.len() == 1 {
        fn tray_label(raw: &str) -> &str {
            match raw {
                "auto" => "Auto Select",
                "main" => "Main Tray",
                "manual" => "Manual Feed",
                "by-pass-tray" => "Bypass Tray",
                other => other,
            }
        }
        Some(option_row(
            "Paper tray",
            disabled_placeholder(tray_label(&dialog.media_source_values[0])),
        ))
    } else {
        fn tray_label(raw: &str) -> &str {
            match raw {
                "auto" => "Auto Select",
                "main" => "Main Tray",
                "manual" => "Manual Feed",
                "by-pass-tray" => "Bypass Tray",
                other => other,
            }
        }
        let labels: Vec<String> = dialog
            .media_source_values
            .iter()
            .map(|v| tray_label(v).to_string())
            .collect();
        Some(option_row(
            "Paper tray",
            dropdown(labels, dialog.paper_tray_index, Msg::PaperTraySelected),
        ))
    };

    let type_row: Option<Element<'_, Msg>> = if dialog.media_type_values.is_empty() {
        Some(option_row(
            "Paper type",
            disabled_placeholder("Not supported"),
        ))
    } else if dialog.media_type_values.len() == 1 {
        Some(option_row(
            "Paper type",
            disabled_placeholder(&dialog.media_type_values[0]),
        ))
    } else {
        let labels: Vec<String> = dialog.media_type_values.clone();
        Some(option_row(
            "Paper type",
            dropdown(labels, dialog.paper_type_index, Msg::PaperTypeSelected),
        ))
    };

    let quality_row: Option<Element<'_, Msg>> = if dialog.print_quality_values.is_empty() {
        Some(option_row(
            "Print quality",
            disabled_placeholder("Not supported"),
        ))
    } else if dialog.print_quality_values.len() == 1 {
        fn quality_label(raw: &str) -> &str {
            match raw {
                "3" => "Draft",
                "4" => "Normal",
                "5" => "High",
                other => other,
            }
        }
        Some(option_row(
            "Print quality",
            disabled_placeholder(quality_label(&dialog.print_quality_values[0])),
        ))
    } else {
        fn quality_label(raw: &str) -> &str {
            match raw {
                "3" => "Draft",
                "4" => "Normal",
                "5" => "High",
                other => other,
            }
        }
        let labels: Vec<String> = dialog
            .print_quality_values
            .iter()
            .map(|v| quality_label(v).to_string())
            .collect();
        Some(option_row(
            "Print quality",
            dropdown(
                labels,
                dialog.print_quality_index,
                Msg::PrintQualitySelected,
            ),
        ))
    };

    let mut paper_items = vec![option_row(
        "Print pages in reverse order",
        toggler(dialog.reverse_order).on_toggle(|_| Msg::ToggleReverseOrder),
    )];
    if let Some(row) = tray_row {
        paper_items.push(row);
    }
    if let Some(row) = type_row {
        paper_items.push(row);
    }
    if let Some(row) = quality_row {
        paper_items.push(row);
    }

    let paper_group = option_group(Some("Paper handling & quality"), paper_items);
    groups = groups.push(paper_group);

    groups.into()
}

fn view_status_row(dialog: &PrintDialog) -> Element<'_, Msg> {
    let status_text = if let Some(idx) = dialog.selected_printer_index {
        if let Some(printer) = dialog.printers.get(idx) {
            let state_str = format!("{}", printer.state);
            format!("{} - {}", printer.name, state_str)
        } else {
            "No printer selected".to_string()
        }
    } else {
        "No printers found".to_string()
    };

    let cancel_btn = button::standard("Cancel").on_press(Msg::Cancel);
    let print_btn = button::suggested("Print").on_press(Msg::Confirm);

    row![
        text(status_text).size(14),
        widget::space::horizontal(),
        cancel_btn,
        print_btn
    ]
    .align_y(Alignment::Center)
    .spacing(12)
    .padding(16)
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

// The application calling `PreparePrint` passes hints for the initial state of the dialog.
// This function would change the initial print dialog state according to the settings and
// page_setup options passed by the application.
pub fn apply_xdg_hints(
    dialog: &mut PrintDialog,
    settings: &HashMap<String, zvariant::OwnedValue>,
    page_setup: &HashMap<String, zvariant::OwnedValue>,
) {
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
        to_owned_value(dialog.layout_direction.as_cpdb_str().to_string()),
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

/// Maps dialog state to setting keys expected by the CPDB backends
pub fn build_cpdb_settings(dialog: &PrintDialog) -> Vec<(String, String)> {
    // NOTE: the expected key names for options in the XDG Print portal and CPDB print backends
    // are not consistent. Here the print dialog's state acts as the source of truth, and we
    // translate the state into option names that the CPDB backends understand.

    let mut out: Vec<(String, String)> = Vec::new();

    // print-color-mode
    out.push((
        "print-color-mode".into(),
        dialog.color_mode.as_cpdb_str().into(),
    ));

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
        dialog.layout_direction.as_cpdb_str().into(),
    ));

    // page-border
    out.push(("page-border".into(), dialog.border.as_cpdb_str().into()));

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
            .insert(|b| b.text("Color").data(ColorMode::Color))
            .insert(|b| b.text("Greyscale").data(ColorMode::Monochrome))
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
            .insert(|b| b.text("Portrait").data(Orientation::Portrait))
            .insert(|b| b.text("Landscape").data(Orientation::Landscape))
            .build();
        let orientation_active = orientation_model
            .iter()
            .find(|&id| orientation_model.data::<Orientation>(id) == Some(&dialog.orientation));
        if let Some(entity) = orientation_active {
            orientation_model.activate(entity);
        }
        portal.print_orientation_model = orientation_model;

        let mut layout_direction_model = segmented_button::Model::builder()
            .insert(|b| b.text("LRTB").data(LayoutDirection::LeftToRightTopToBottom))
            .insert(|b| b.text("RLTB").data(LayoutDirection::RightToLeftTopToBottom))
            .insert(|b| b.text("TBLR").data(LayoutDirection::TopToBottomLeftToRight))
            .insert(|b| b.text("TBRL").data(LayoutDirection::TopToBottomRightToLeft))
            .build();
        let layout_active = layout_direction_model.iter().find(|&id| {
            layout_direction_model.data::<LayoutDirection>(id) == Some(&dialog.layout_direction)
        });
        if let Some(entity) = layout_active {
            layout_direction_model.activate(entity);
        }
        portal.print_layout_direction_model = layout_direction_model;

        let mut page_selection_model = segmented_button::Model::builder()
            .insert(|b| b.text("All Pages").data(PageSetSelection::All))
            .insert(|b| b.text("Current Page").data(PageSetSelection::Current))
            .insert(|b| b.text("Odd Pages Only").data(PageSetSelection::Odd))
            .insert(|b| b.text("Even Pages Only").data(PageSetSelection::Even))
            .insert(|b| {
                b.text("Custom Range")
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
