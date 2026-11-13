use std::cmp::Ordering;
use std::sync::Arc;

use super::super::components::button::Button;
use super::super::components::data_table;
use gpui_kit::TestSupportExt as _;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Context, Div, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::super::format::{format_bytes, format_duration, format_read_value};
use crate::shared::workspace::{DetailState, PrinterFile, ReadStatus, ResourceId};
use crate::state::{LanConnection, LocalPrinterId};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum FileOrder {
    Name,
    NameDesc,
    DurationAsc,
    DurationDesc,
    SizeAsc,
    SizeDesc,
    #[default]
    Newest,
    Oldest,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum FileRange {
    #[default]
    All,
    Under,
    Middle,
    Over,
    Unknown,
}

impl FileRange {
    const OPTIONS: [Self; 5] = [
        Self::All,
        Self::Under,
        Self::Middle,
        Self::Over,
        Self::Unknown,
    ];
    fn matches(self, value: Option<f64>, low: f64, high: f64) -> bool {
        match (self, value) {
            (Self::All, _) | (Self::Unknown, None) => true,
            (Self::Under, Some(value)) => value < low,
            (Self::Middle, Some(value)) => (low..high).contains(&value),
            (Self::Over, Some(value)) => value >= high,
            _ => false,
        }
    }
    pub(in crate::gpui_app) fn label_with_locale(
        self,
        duration: bool,
        locale: &str,
    ) -> std::borrow::Cow<'static, str> {
        match (self, duration) {
            (Self::All, _) => rust_i18n::t!("files.filters.options.all", locale = locale),
            (Self::Unknown, _) => rust_i18n::t!("files.filters.options.unknown", locale = locale),
            (Self::Under, true) => rust_i18n::t!("files.filters.duration.under", locale = locale),
            (Self::Middle, true) => rust_i18n::t!("files.filters.duration.middle", locale = locale),
            (Self::Over, true) => rust_i18n::t!("files.filters.duration.over", locale = locale),
            (Self::Under, false) => rust_i18n::t!("files.filters.size.under", locale = locale),
            (Self::Middle, false) => rust_i18n::t!("files.filters.size.middle", locale = locale),
            (Self::Over, false) => rust_i18n::t!("files.filters.size.over", locale = locale),
        }
    }

    pub(in crate::gpui_app) fn label(self, duration: bool) -> std::borrow::Cow<'static, str> {
        self.label_with_locale(duration, rust_i18n::locale().as_ref())
    }
}

/// One native virtual table for the open, normalized detail.
/// Rebuild indices only when catalogue, query, order or ranges change.
pub(crate) struct FileList {
    owner: Option<(LocalPrinterId, u64)>,
    detail: Arc<DetailState>,
    pub(in crate::gpui_app) visible: Vec<usize>,
    selection: Option<(ResourceId, String)>,
    last_requested_selection: Option<ResourceId>,
    // TableState emits SelectRow even when restoring selection after a sort.
    // Ignore that event; allow an explicit click to retry a failed read.
    selection_sync: Option<usize>,
    viewport: std::ops::Range<usize>,
    read_view_revision: u64,
    interest_sender: Option<tokio::sync::watch::Sender<crate::shared::workspace::FileReadInterest>>,
    reads_active: bool,
    connection: LanConnection,
    query: String,
    pub(in crate::gpui_app) order: FileOrder,
    pub(in crate::gpui_app) duration: FileRange,
    pub(in crate::gpui_app) size: FileRange,
}

impl Default for FileList {
    fn default() -> Self {
        Self {
            owner: None,
            detail: Arc::new(DetailState::empty(0)),
            visible: Vec::new(),
            selection: None,
            last_requested_selection: None,
            selection_sync: None,
            viewport: 0..0,
            read_view_revision: 0,
            interest_sender: None,
            reads_active: false,
            connection: LanConnection::Connecting,
            query: String::new(),
            order: FileOrder::default(),
            duration: FileRange::default(),
            size: FileRange::default(),
        }
    }
}

impl FileList {
    pub(in crate::gpui_app) fn bind_interest(
        &mut self,
        sender: &tokio::sync::watch::Sender<crate::shared::workspace::FileReadInterest>,
        active: bool,
    ) {
        if self
            .interest_sender
            .as_ref()
            .is_some_and(|old| old.same_channel(sender))
        {
            self.reads_active = active;
            if !active {
                self.suspend_reads();
            }
            return;
        }
        self.suspend_reads();
        self.interest_sender = Some(sender.clone());
        self.reads_active = active;
    }

    pub(in crate::gpui_app) fn suspend_reads(&mut self) {
        self.reads_active = false;
        if let Some(sender) = &self.interest_sender {
            sender.send_if_modified(|current| {
                if *current == Default::default() {
                    return false;
                }
                *current = Default::default();
                true
            });
        }
    }

    pub(in crate::gpui_app) fn resume_reads(&mut self) {
        self.reads_active = true;
        self.update_read_interest();
    }

    pub(in crate::gpui_app) fn update_read_interest(&self) {
        if !self.reads_active {
            return;
        }
        let Some(sender) = &self.interest_sender else {
            return;
        };
        let mut ids = Vec::with_capacity(crate::shared::workspace::FILE_READ_WINDOW);
        let mut add = |file: &PrinterFile| {
            if file.estimated_duration_s.is_none()
                && !matches!(
                    file.duration_read_status,
                    ReadStatus::Ready | ReadStatus::Unsupported | ReadStatus::Denied
                )
                && ids.len() < crate::shared::workspace::FILE_READ_WINDOW
                && !ids.contains(&file.id)
            {
                ids.push(file.id);
            }
        };
        if let Some(file) = self.selected_file() {
            add(file);
        }
        let start = self.viewport.start.min(self.visible.len());
        let end = self.viewport.end.min(self.visible.len());
        for ix in start..end {
            if let Some(file) = self.file_at(ix) {
                add(file);
            }
        }
        // Limit look-ahead; do not scan the entire filtered catalogue.
        for ix in end..end
            .saturating_add(crate::shared::workspace::FILE_READ_WINDOW)
            .min(self.visible.len())
        {
            if let Some(file) = self.file_at(ix) {
                add(file);
            }
        }
        let interest = crate::shared::workspace::FileReadInterest {
            generation: self.detail.generation,
            view_revision: self.read_view_revision,
            ids,
        };
        sender.send_if_modified(|current| {
            if *current == interest {
                false
            } else {
                *current = interest;
                true
            }
        });
    }
    pub(in crate::gpui_app) fn files(&self) -> &[PrinterFile] {
        self.detail.files.value.as_deref().unwrap_or(&[])
    }

    pub(in crate::gpui_app) fn sync_connection(&mut self, connection: LanConnection) -> bool {
        std::mem::replace(&mut self.connection, connection) != connection
    }

    pub(in crate::gpui_app) fn sync(
        &mut self,
        owner: LocalPrinterId,
        detail: Arc<DetailState>,
        selected: Option<ResourceId>,
        query: &str,
    ) -> bool {
        let identity = (owner, detail.generation);
        let owner_changed = self.owner != Some(identity);
        let catalog_changed = owner_changed
            || (!Arc::ptr_eq(&self.detail, &detail)
                && self.detail.files.value != detail.files.value);
        let query = query.trim().to_lowercase();
        let query_changed = self.query != query;
        let read_state_changed = self.detail.files.status != detail.files.status;
        self.detail = detail;
        if owner_changed {
            self.selection = None;
            self.last_requested_selection = self.owner.and(selected);
        }
        self.owner = Some(identity);
        let selection_changed = selected != self.last_requested_selection;
        if selection_changed {
            self.selection = selected.and_then(|id| {
                self.files()
                    .iter()
                    .find(|f| f.id == id)
                    .map(|f| (id, f.display_name.clone()))
            });
            self.last_requested_selection = selected;
        }
        // FlashForge's temporary ordinal handles can change after upload.
        // Never reuse a preview or action for another filename under the same handle.
        if catalog_changed
            && self.selection.as_ref().is_some_and(|(id, name)| {
                !self
                    .files()
                    .iter()
                    .any(|f| f.id == *id && f.display_name == *name)
            })
        {
            self.selection = None;
        }
        if catalog_changed || query_changed {
            if owner_changed || query_changed {
                self.read_view_revision = self.read_view_revision.saturating_add(1);
            }
            self.query = query;
            self.rebuild_rows();
        } else {
            self.update_read_interest();
        }
        owner_changed || catalog_changed || query_changed || selection_changed || read_state_changed
    }

    pub(in crate::gpui_app) fn rebuild(&mut self) {
        self.read_view_revision = self.read_view_revision.saturating_add(1);
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        let files = self.detail.files.value.as_deref().unwrap_or(&[]);
        self.visible = files
            .iter()
            .enumerate()
            .filter(|(_, file)| {
                (self.query.is_empty() || file.display_name.to_lowercase().contains(&self.query))
                    && self
                        .duration
                        .matches(file.estimated_duration_s, 3600., 14_400.)
                    && self.size.matches(
                        file.size_bytes.map(|v| v as f64),
                        10. * 1_048_576.,
                        100. * 1_048_576.,
                    )
            })
            .map(|(index, _)| index)
            .collect();
        if matches!(self.order, FileOrder::Name | FileOrder::NameDesc) {
            self.visible
                .sort_by_cached_key(|&ix| files[ix].display_name.to_lowercase());
            if self.order == FileOrder::NameDesc {
                self.visible.reverse();
            }
        } else {
            self.visible.sort_by(|&a, &b| {
                let (a, b) = (&files[a], &files[b]);
                let numeric = match self.order {
                    FileOrder::DurationAsc => {
                        compare_known(a.estimated_duration_s, b.estimated_duration_s, false)
                    }
                    FileOrder::DurationDesc => {
                        compare_known(a.estimated_duration_s, b.estimated_duration_s, true)
                    }
                    FileOrder::SizeAsc => compare_known(a.size_bytes, b.size_bytes, false),
                    FileOrder::SizeDesc => compare_known(a.size_bytes, b.size_bytes, true),
                    FileOrder::Newest => {
                        compare_known(a.modified_at_unix_s, b.modified_at_unix_s, true)
                    }
                    FileOrder::Oldest => {
                        compare_known(a.modified_at_unix_s, b.modified_at_unix_s, false)
                    }
                    FileOrder::Name | FileOrder::NameDesc => Ordering::Equal,
                };
                numeric
                    .then_with(|| a.display_name.cmp(&b.display_name))
                    .then_with(|| a.id.0.cmp(&b.id.0))
            });
        }
        self.update_read_interest();
    }

    pub(in crate::gpui_app) fn file_at(&self, ix: usize) -> Option<&PrinterFile> {
        self.visible.get(ix).and_then(|&row| self.files().get(row))
    }

    pub(in crate::gpui_app) fn selected_index(&self) -> Option<usize> {
        let (id, name) = self.selection.as_ref()?;
        self.visible
            .iter()
            .position(|&ix| self.files()[ix].id == *id && self.files()[ix].display_name == *name)
    }

    pub(in crate::gpui_app) fn selected_file(&self) -> Option<&PrinterFile> {
        self.file_at(self.selected_index()?)
    }

    pub(in crate::gpui_app) fn select(&mut self, ix: Option<usize>) {
        let selection = ix.and_then(|ix| {
            self.file_at(ix)
                .map(|file| (file.id, file.display_name.clone()))
        });
        if self.selection != selection {
            self.selection = selection;
            self.read_view_revision = self.read_view_revision.saturating_add(1);
        }
        self.update_read_interest();
    }

    pub(in crate::gpui_app) fn consume_selection_sync(&mut self, ix: usize) -> bool {
        self.selection_sync.take() == Some(ix)
    }
}

fn compare_known<T: PartialOrd>(a: Option<T>, b: Option<T>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) if descending => b.partial_cmp(&a).unwrap_or(Ordering::Equal),
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub(in crate::gpui_app) fn sync_selection(
    list: &mut TableState<FileList>,
    cx: &mut Context<TableState<FileList>>,
) {
    let ix = list.delegate().selected_index();
    if ix != list.selected_row() {
        if let Some(ix) = ix {
            list.delegate_mut().selection_sync = Some(ix);
            list.set_selected_row(ix, cx);
        } else {
            list.clear_selection(cx);
        }
    }
    list.refresh(cx);
    cx.notify();
}

pub(in crate::gpui_app) fn new_table(
    window: &mut Window,
    cx: &mut Context<TableState<FileList>>,
) -> TableState<FileList> {
    TableState::new(FileList::default(), window, cx)
        .col_selectable(false)
        .cell_selectable(false)
        .col_movable(false)
        .col_resizable(false)
        .loop_selection(false)
}

pub(in crate::gpui_app) fn list_controls(
    list: &Entity<TableState<FileList>>,
    cx: &App,
) -> impl IntoElement {
    let delegate = list.read(cx).delegate();
    h_flex()
        .flex_wrap()
        .gap_2()
        .children([true, false].map(|duration| {
            let chosen = if duration {
                delegate.duration
            } else {
                delegate.size
            };
            let list = list.clone();
            Button::new(if duration {
                "files-duration-filter"
            } else {
                "files-size-filter"
            })
            .outline()
            .popup_trigger()
            .small()
            .label(if duration {
                rust_i18n::t!(
                    "files.filters.duration.summary",
                    value = chosen.label(duration)
                )
                .into_owned()
            } else {
                rust_i18n::t!("files.filters.size.summary", value = chosen.label(duration))
                    .into_owned()
            })
            .select_menu(
                FileRange::OPTIONS
                    .iter()
                    .position(|option| *option == chosen)
                    .expect("FileRange has a menu option"),
                move |mut menu, _, _| {
                    for option in FileRange::OPTIONS {
                        let list = list.clone();
                        menu = menu.item(
                            PopupMenuItem::new(option.label(duration).to_string())
                                .checked(option == chosen)
                                .on_click(move |_, _, cx| {
                                    list.update(cx, |state, cx| {
                                        let delegate = state.delegate_mut();
                                        if duration {
                                            delegate.duration = option;
                                        } else {
                                            delegate.size = option;
                                        }
                                        delegate.rebuild();
                                        sync_selection(state, cx);
                                    })
                                }),
                        );
                    }
                    menu
                },
            )
        }))
}

impl TableDelegate for FileList {
    fn visible_rows_changed(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        // A shrinking result changes the end, not the scroll position.
        if self.viewport.start != range.start {
            self.read_view_revision = self.read_view_revision.saturating_add(1);
        }
        self.viewport = range;
        // The callback runs during layout; publish interest after the frame without I/O.
        let table = cx.entity();
        cx.defer(move |cx| table.update(cx, |table, _| table.delegate().update_read_interest()));
    }
    fn columns_count(&self, _: &App) -> usize {
        4
    }

    fn rows_count(&self, _: &App) -> usize {
        self.visible.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (key, label, width, sort) = match col_ix {
            0 => (
                "name",
                rust_i18n::t!("files.table.columns.name"),
                280.,
                match self.order {
                    FileOrder::Name => ColumnSort::Ascending,
                    FileOrder::NameDesc => ColumnSort::Descending,
                    _ => ColumnSort::Default,
                },
            ),
            1 => (
                "duration",
                rust_i18n::t!("files.table.columns.duration"),
                150.,
                match self.order {
                    FileOrder::DurationAsc => ColumnSort::Ascending,
                    FileOrder::DurationDesc => ColumnSort::Descending,
                    _ => ColumnSort::Default,
                },
            ),
            2 => (
                "size",
                rust_i18n::t!("files.table.columns.size"),
                100.,
                match self.order {
                    FileOrder::SizeAsc => ColumnSort::Ascending,
                    FileOrder::SizeDesc => ColumnSort::Descending,
                    _ => ColumnSort::Default,
                },
            ),
            _ => (
                "modified",
                rust_i18n::t!("files.table.columns.modified"),
                160.,
                match self.order {
                    FileOrder::Oldest => ColumnSort::Ascending,
                    FileOrder::Newest => ColumnSort::Descending,
                    _ => ColumnSort::Default,
                },
            ),
        };
        Column::new(key, label)
            .p_0()
            .width(px(width))
            .min_width(px(width))
            .sort(sort)
            .resizable(false)
            .movable(false)
            .when(col_ix == 0, |column| column.fixed_left())
            .when(col_ix == 1 || col_ix == 2, |column| column.text_right())
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.order = match (col_ix, sort) {
            (0, ColumnSort::Descending | ColumnSort::Default) => FileOrder::NameDesc,
            (1, ColumnSort::Ascending) => FileOrder::DurationAsc,
            (1, ColumnSort::Descending | ColumnSort::Default) => FileOrder::DurationDesc,
            (2, ColumnSort::Ascending) => FileOrder::SizeAsc,
            (2, ColumnSort::Descending | ColumnSort::Default) => FileOrder::SizeDesc,
            (3, ColumnSort::Ascending) => FileOrder::Oldest,
            (3, ColumnSort::Descending | ColumnSort::Default) => FileOrder::Newest,
            _ => FileOrder::Name,
        };
        self.rebuild();
        let table = cx.entity();
        cx.defer(move |cx| table.update(cx, sync_selection));
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        let button = Button::new(("file-column-title", col_ix))
            .accessibility_label(column.name.clone())
            .tooltip(column.name.clone())
            .on_click(cx.listener(move |state, _, window, cx| {
                let sort = match state.delegate().column(col_ix, cx).sort {
                    Some(ColumnSort::Default) => ColumnSort::Descending,
                    Some(ColumnSort::Descending) => ColumnSort::Ascending,
                    _ => ColumnSort::Default,
                };
                state.delegate_mut().perform_sort(col_ix, sort, window, cx);
            }));
        data_table::header(
            button,
            column.name,
            column.sort,
            col_ix == 1 || col_ix == 2,
            cx,
        )
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        match self.file_at(row_ix) {
            Some(file) => div().id(("select-file", file.id.0 as usize)),
            None => div().id(("file-empty-row", row_ix)),
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let file = self.file_at(row_ix).expect("Visible file row");
        // An unsupported catalogue operation does not make every field unsupported;
        // metadata or G-code parsing may provide another source.
        let catalog_status = match self.detail.files.status {
            ReadStatus::Unsupported => ReadStatus::NotLoaded,
            status => status,
        };
        let duration_status = match catalog_status {
            ReadStatus::Error | ReadStatus::Denied | ReadStatus::Limited => catalog_status,
            _ => file.duration_read_status,
        };
        let value = match col_ix {
            0 => file.display_name.clone(),
            1 => format_read_value(
                file.estimated_duration_s,
                duration_status,
                self.connection,
                format_duration,
            ),
            2 => format_read_value(file.size_bytes, catalog_status, self.connection, |bytes| {
                format_bytes(Some(bytes))
            }),
            _ => format_read_value(
                file.modified_at_unix_s
                    .and_then(|s| chrono::DateTime::from_timestamp(s as i64, 0)),
                catalog_status,
                self.connection,
                |date| {
                    date.with_timezone(&chrono::Local)
                        .format("%d/%m/%Y %H:%M")
                        .to_string()
                },
            ),
        };
        data_table::cell()
            .id((
                ["file-name", "file-duration", "file-size", "file-modified"][col_ix],
                file.id.0 as usize,
            ))
            .aria_label(value.clone())
            .test_support()
            .truncate()
            .when(col_ix == 0, |cell| cell.font_medium())
            .when(col_ix != 0, |cell| {
                cell.text_color(cx.theme().muted_foreground)
                    .font_features(gpui_kit::FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
            })
            .when(col_ix == 1 || col_ix == 2, |cell| cell.text_right())
            .child(value)
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        super::super::feedback::empty_state(
            "files-empty-state",
            IconName::File,
            &rust_i18n::t!("files.table.empty"),
            cx,
        )
    }
}
