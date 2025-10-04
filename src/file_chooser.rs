use cosmic::{action, iced::window, widget};
use cosmic_files::dialog::{
    DialogChoice, DialogChoiceOption, DialogFilter, DialogFilterPattern, DialogKind, DialogMessage,
    DialogResult, DialogSettings,
};
use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
use tokio::sync::mpsc::Sender;
use zbus::zvariant;

use crate::{
    PortalResponse,
    app::{CosmicPortal, Msg as AppMsg},
    subscription,
};

pub(crate) type Dialog = cosmic_files::dialog::Dialog<Msg>;

type Choices = Vec<(String, String, Vec<(String, String)>, String)>;
type Filter = (String, Vec<(u32, String)>);
type Filters = Vec<Filter>;

#[derive(zvariant::DeserializeDict, zvariant::Type, Clone, Debug)]
#[zvariant(signature = "a{sv}")]
pub struct OpenFileOptions {
    accept_label: Option<String>,
    #[allow(dead_code)]
    modal: Option<bool>,
    multiple: Option<bool>,
    directory: Option<bool>,
    filters: Option<Filters>,
    current_filter: Option<Filter>,
    choices: Option<Choices>,
    current_folder: Option<Vec<u8>>,
}

#[derive(zvariant::DeserializeDict, zvariant::Type, Clone, Debug)]
#[zvariant(signature = "a{sv}")]
pub struct SaveFileOptions {
    accept_label: Option<String>,
    #[allow(dead_code)]
    modal: Option<bool>,
    filters: Option<Filters>,
    current_filter: Option<Filter>,
    choices: Option<Choices>,
    current_name: Option<String>,
    current_folder: Option<Vec<u8>>,
    #[allow(dead_code)]
    current_file: Option<Vec<u8>>,
}

#[derive(zvariant::DeserializeDict, zvariant::Type, Clone, Debug)]
#[zvariant(signature = "a{sv}")]
pub struct SaveFilesOptions {
    accept_label: Option<String>,
    #[allow(dead_code)]
    modal: Option<bool>,
    choices: Option<Choices>,
    current_folder: Option<Vec<u8>>,
    #[allow(dead_code)]
    files: Option<Vec<Vec<u8>>>,
}

#[derive(Clone, Debug)]
pub enum FileChooserOptions {
    OpenFile(OpenFileOptions),
    SaveFile(SaveFileOptions),
    SaveFiles(SaveFilesOptions),
}

impl FileChooserOptions {
    fn accept_label(&self) -> Option<String> {
        match self {
            Self::OpenFile(x) => x.accept_label.clone(),
            Self::SaveFile(x) => x.accept_label.clone(),
            Self::SaveFiles(x) => x.accept_label.clone(),
        }
    }

    fn choices(&self) -> Option<Choices> {
        match self {
            Self::OpenFile(x) => x.choices.clone(),
            Self::SaveFile(x) => x.choices.clone(),
            Self::SaveFiles(x) => x.choices.clone(),
        }
    }

    fn filters(&self) -> Option<Filters> {
        match self {
            Self::OpenFile(x) => x.filters.clone(),
            Self::SaveFile(x) => x.filters.clone(),
            Self::SaveFiles(_) => None,
        }
    }

    fn current_filter(&self) -> Option<Filter> {
        match self {
            Self::OpenFile(x) => x.current_filter.clone(),
            Self::SaveFile(x) => x.current_filter.clone(),
            Self::SaveFiles(_) => None,
        }
    }

    #[allow(dead_code)]
    fn modal(&self) -> bool {
        // Defaults to true
        match self {
            Self::OpenFile(x) => x.modal,
            Self::SaveFile(x) => x.modal,
            Self::SaveFiles(x) => x.modal,
        }
        .unwrap_or(true)
    }

    fn current_folder(&self) -> Option<PathBuf> {
        match self {
            Self::OpenFile(x) => x.current_folder.clone(),
            Self::SaveFile(x) => x.current_folder.clone(),
            Self::SaveFiles(x) => x.current_folder.clone(),
        }
        .map(|mut x| {
            // Trim leading NULs
            while x.last() == Some(&0) {
                x.pop();
            }
            PathBuf::from(OsString::from_vec(x))
        })
    }
}

#[derive(zvariant::SerializeDict, zvariant::Type)]
#[zvariant(signature = "a{sv}")]
pub struct FileChooserResult {
    uris: Vec<String>,
    choices: Vec<(String, String)>,
    current_filter: Option<Filter>,
}

pub struct FileChooser {
    tx: Sender<subscription::Event>,
}

impl FileChooser {
    pub fn new(tx: Sender<subscription::Event>) -> Self {
        Self { tx }
    }

    async fn run(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: FileChooserOptions,
    ) -> PortalResponse<FileChooserResult> {
        log::debug!("file chooser {handle}, {app_id}, {parent_window}, {title}, {options:?}");

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        if let Err(err) = self
            .tx
            .send(subscription::Event::FileChooser(Args {
                handle: handle.to_owned(),
                app_id: app_id.to_string(),
                parent_window: parent_window.to_string(),
                title: title.to_string(),
                options,
                tx,
            }))
            .await
        {
            log::error!("failed to send file chooser event: {}", err);
            return PortalResponse::Other;
        }
        if let Some(res) = rx.recv().await {
            res
        } else {
            PortalResponse::Cancelled
        }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    async fn open_file(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: OpenFileOptions,
    ) -> PortalResponse<FileChooserResult> {
        self.run(
            handle,
            app_id,
            parent_window,
            title,
            FileChooserOptions::OpenFile(options),
        )
        .await
    }

    async fn save_file(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: SaveFileOptions,
    ) -> PortalResponse<FileChooserResult> {
        self.run(
            handle,
            app_id,
            parent_window,
            title,
            FileChooserOptions::SaveFile(options),
        )
        .await
    }

    async fn save_files(
        &self,
        handle: zvariant::ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: SaveFilesOptions,
    ) -> PortalResponse<FileChooserResult> {
        self.run(
            handle,
            app_id,
            parent_window,
            title,
            FileChooserOptions::SaveFiles(options),
        )
        .await
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    DialogMessage(DialogMessage),
    DialogResult(DialogResult),
}

#[derive(Clone, Debug)]
pub(crate) struct Args {
    pub handle: zvariant::ObjectPath<'static>,
    pub app_id: String,
    pub parent_window: String,
    pub title: String,
    pub options: FileChooserOptions,
    pub tx: Sender<PortalResponse<FileChooserResult>>,
}

fn map_msg(id: window::Id, message: cosmic::Action<Msg>) -> cosmic::Action<AppMsg> {
    match message {
        cosmic::Action::App(msg) => cosmic::Action::App(AppMsg::FileChooser(id, msg)),
        cosmic::Action::Cosmic(cosmic_message) => cosmic::Action::Cosmic(cosmic_message),
        cosmic::Action::None => cosmic::Action::None,
    }
}

pub(crate) fn view(portal: &CosmicPortal, id: window::Id) -> cosmic::Element<'_, AppMsg> {
    match portal.file_choosers.get(&id) {
        Some((_args, dialog)) => dialog.view(id).map(move |msg| AppMsg::FileChooser(id, msg)),
        None => widget::text(format!("no file chooser dialog with ID {id:?}")).into(),
    }
}

pub fn update_msg(
    portal: &mut CosmicPortal,
    id: window::Id,
    msg: Msg,
) -> cosmic::Task<cosmic::Action<AppMsg>> {
    match msg {
        Msg::DialogMessage(dialog_msg) => match portal.file_choosers.get_mut(&id) {
            Some((_args, dialog)) => dialog.update(dialog_msg).map(move |msg| map_msg(id, msg)),
            None => {
                log::warn!("no file chooser dialog with ID {id:?}");
                cosmic::Task::none()
            }
        },
        Msg::DialogResult(dialog_res) => match portal.file_choosers.remove(&id) {
            Some((args, dialog)) => {
                log::debug!("file chooser result {:?}", dialog_res);
                let response = match dialog_res {
                    DialogResult::Cancel => PortalResponse::Cancelled,
                    DialogResult::Open(paths) => {
                        let mut uris = Vec::with_capacity(paths.len());
                        for path in paths {
                            match url::Url::from_file_path(&path) {
                                Ok(url) => uris.push(url.to_string()),
                                Err(()) => {
                                    log::error!("failed to convert to URL: {:?}", path);
                                }
                            }
                        }

                        if uris.is_empty() {
                            // Return error if URIs is empty, likely as a result of failing to convert paths
                            PortalResponse::Other
                        } else {
                            let dialog_choices = dialog.choices();
                            let mut choices = Vec::with_capacity(dialog_choices.len());
                            for choice in dialog_choices.iter() {
                                match choice {
                                    DialogChoice::CheckBox { id, value, .. } => {
                                        choices.push((
                                            id.clone(),
                                            if *value { "true" } else { "false" }.to_string(),
                                        ));
                                    }
                                    DialogChoice::ComboBox {
                                        id,
                                        options,
                                        selected,
                                        ..
                                    } => {
                                        // If nothing is selected, fall back to the first selection
                                        let option_i = selected.unwrap_or(0);
                                        if let Some(option) = options.get(option_i) {
                                            choices.push((id.clone(), option.id.clone()));
                                        }
                                    }
                                }
                            }

                            let (filters, filter_selected) = dialog.filters();
                            let mut current_filter = None;
                            if let Some(filter_i) = filter_selected
                                && let Some(filter) = filters.get(filter_i) {
                                    let mut patterns = Vec::with_capacity(filter.patterns.len());
                                    for pattern in filter.patterns.iter() {
                                        patterns.push(match pattern {
                                            DialogFilterPattern::Glob(glob) => (0u32, glob.clone()),
                                            DialogFilterPattern::Mime(mime) => (1u32, mime.clone()),
                                        });
                                    }
                                    current_filter = Some((filter.label.clone(), patterns));
                                }

                            PortalResponse::Success(FileChooserResult {
                                uris,
                                choices,
                                current_filter,
                            })
                        }
                    }
                };
                cosmic::Task::perform(
                    async move {
                        let _ = args.tx.send(response).await;
                        action::none()
                    },
                    |x| x,
                )
            }
            None => {
                log::warn!("no file chooser dialog with ID {id:?}");
                cosmic::Task::none()
            }
        },
    }
}

pub fn update_args(portal: &mut CosmicPortal, args: Args) -> cosmic::Task<cosmic::Action<AppMsg>> {
    let mut cmds = Vec::with_capacity(2);

    let kind = match &args.options {
        FileChooserOptions::OpenFile(options) => {
            if options.directory.unwrap_or(false) {
                if options.multiple.unwrap_or(false) {
                    DialogKind::OpenMultipleFolders
                } else {
                    DialogKind::OpenFolder
                }
            } else if options.multiple.unwrap_or(false) {
                DialogKind::OpenMultipleFiles
            } else {
                DialogKind::OpenFile
            }
        }
        FileChooserOptions::SaveFile(options) => DialogKind::SaveFile {
            filename: options.current_name.clone().unwrap_or_default(),
        },
        FileChooserOptions::SaveFiles(options) => {
            log::error!("{options:?} not supported");
            DialogKind::OpenFolder
        }
    };
    let mut settings = DialogSettings::new().kind(kind);
    //TODO: setting app_id breaks dialog floating: .app_id(args.app_id.clone());
    if let Some(path) = args.options.current_folder() {
        settings = settings.path(path);
    }

    let (mut dialog, command) = Dialog::new(
        settings,
        Msg::DialogMessage,
        Msg::DialogResult,
    );
    cmds.push(command);
    cmds.push(dialog.set_title(args.title.clone()));
    if let Some(accept_label) = args.options.accept_label() {
        dialog.set_accept_label(accept_label);
    }
    if let Some(xdg_choices) = args.options.choices() {
        let mut choices = Vec::with_capacity(xdg_choices.len());
        for (id, label, xdg_options, selected_id) in xdg_choices {
            if xdg_options.is_empty() {
                choices.push(DialogChoice::CheckBox {
                    id,
                    label,
                    value: selected_id == "true",
                });
            } else {
                let mut options = Vec::with_capacity(xdg_options.len());
                for (id, label) in xdg_options {
                    options.push(DialogChoiceOption { id, label });
                }
                let selected = options.iter().position(|x| x.id == selected_id);
                choices.push(DialogChoice::ComboBox {
                    id,
                    label,
                    options,
                    selected,
                });
            }
        }
        dialog.set_choices(choices);
    }
    {
        let mut xdg_filters = args.options.filters().unwrap_or_default();
        let filter_selected = match args.options.current_filter() {
            Some(current_filter) => match xdg_filters.iter().position(|x| *x == current_filter) {
                Some(filter_i) => Some(filter_i),
                None => {
                    let filter_i = 0;
                    xdg_filters.insert(filter_i, current_filter);
                    Some(filter_i)
                }
            },
            None => {
                if !xdg_filters.is_empty() {
                    Some(0)
                } else {
                    None
                }
            }
        };
        let mut filters = Vec::with_capacity(xdg_filters.len());
        for (label, xdg_patterns) in xdg_filters {
            let mut patterns = Vec::with_capacity(xdg_patterns.len());
            for (kind, value) in xdg_patterns {
                patterns.push(match kind {
                    0 => DialogFilterPattern::Glob(value),
                    1 => DialogFilterPattern::Mime(value),
                    _ => {
                        log::warn!("unsupported filter pattern {:?}", (kind, value));
                        continue;
                    }
                });
            }
            filters.push(DialogFilter { label, patterns });
        }
        cmds.push(dialog.set_filters(filters, filter_selected));
    }
    let id = dialog.window_id();
    portal.file_choosers.insert(id, (args, dialog));
    cosmic::iced::Task::batch(cmds).map(move |msg| map_msg(id, msg))
}
