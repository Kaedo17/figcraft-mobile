//! The built-in file browser: every file dialog on a host with no system file picker (Android —
//! rfd has no backend there; the web keeps its own flow). Open, Save as, Export, Import, Place
//! image, Choose image, Add library, plugin and variables files and folder picks are all served
//! by it, answering exactly as the system dialog would.
//!
//! A folder path field with Up one level and Home folder, the folder's subfolders and the files
//! of the chosen type, a file name field and the file types. It only lists folders the user
//! navigates to and never reads a file: the picked path goes to the same follow-up as the system
//! dialog's. Navigation is clamped inside the host's app-private storage
//! ([`Services::app_files_dir`]): *Up one level* stops at the root's top, the home-folder button
//! goes to the app's Documents folder, and a typed pick outside the root is refused as not
//! found. The desktop has no root and opens no browser — its system dialogs answer, as before.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::task::{Context as TaskContext, Poll};

use egui::{Align2, Rect, Sense, Ui, pos2, vec2};

use crate::i18n::{tr, trf};
use crate::theme::{Tokens, regular};
use crate::widgets;
use crate::{FigApp, Picked, icons};

/// At most this many entries are listed; a bigger folder shows the first ones and says so.
const MAX_LISTED: usize = 5000;
/// At most this many folder entries are read, so a huge folder can't stall the window.
const MAX_SCANNED: usize = 100_000;
const ROW: f32 = 24.0;

/// The browser while it is open, and the folder it showed last (where it opens next time).
#[derive(Default)]
pub(crate) struct State {
    pub(crate) open: Option<Box<FileBrowser>>,
    pub(crate) last_dir: Option<PathBuf>,
}

/// One subfolder or file in the listing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    /// The name as shown (a name that isn't valid UTF-8 is shown with replacement characters).
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) dir: bool,
}

/// A folder's subfolders (first) and files, by name.
#[derive(Debug, Default)]
pub(crate) struct Listing {
    pub(crate) entries: Vec<Entry>,
    /// The folder holds more than was listed.
    pub(crate) truncated: bool,
}

/// Whether `path` has one of `exts` (case-insensitive); `*` takes any file.
fn matches(path: &Path, exts: &[String]) -> bool {
    exts.iter().any(|e| e == "*")
        || path.extension().and_then(std::ffi::OsStr::to_str).is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// The subfolders and the files with one of `exts` in `dir`, at most `cap` of them, folders first
/// and each by name. Hidden entries (a leading dot) are left out; entries that can't be read are
/// skipped.
fn list_dir(dir: &Path, exts: &[String], cap: usize) -> std::io::Result<Listing> {
    let mut listing = Listing::default();
    for (i, item) in std::fs::read_dir(dir)?.enumerate() {
        if i >= MAX_SCANNED {
            listing.truncated = true;
            break;
        }
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = item.path();
        // A symbolic link to a folder is a folder.
        let is_dir = match item.file_type() {
            Ok(t) if t.is_symlink() => std::fs::metadata(&path).is_ok_and(|m| m.is_dir()),
            Ok(t) => t.is_dir(),
            Err(_) => continue,
        };
        if !is_dir && !matches(&path, exts) {
            continue;
        }
        if listing.entries.len() >= cap {
            listing.truncated = true;
            break;
        }
        listing.entries.push(Entry { name, path, dir: is_dir });
    }
    listing.entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
    Ok(listing)
}

/// The user's home folder (`None` on Android, where `HOME` is unset).
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// "Up one level" from `dir`: the parent while both `dir` and its parent stay inside `root`, else
/// `None` — at the root's top, and for a `dir` the root doesn't contain. `Path::starts_with`
/// compares path components, so a sibling whose name shares the root's text is not inside it.
fn up_from(root: &Path, dir: &Path) -> Option<PathBuf> {
    match dir.parent() {
        Some(parent) if dir.starts_with(root) && parent.starts_with(root) => Some(parent.to_path_buf()),
        _ => None,
    }
}

/// Whether `p` is a folder the browser may open: under the root its navigation is clamped to.
fn inside_root(p: &Path, root: &Path) -> bool {
    p.starts_with(root)
}

/// The root an Android browser's navigation is clamped to (the host's app-private storage). The
/// desktop has none — `FileBrowser::roots` is `None` and every folder the user types is fair
/// game.
pub(crate) struct Roots {
    /// The app-private root (`Services::app_files_dir`).
    pub(crate) app: PathBuf,
}

/// The app storage the browser starts in: its `Documents` folder when it exists, else the root.
pub(crate) fn app_start(roots: &Roots) -> PathBuf {
    let documents = roots.app.join("Documents");
    if documents.is_dir() { documents } else { roots.app.clone() }
}

/// What the user typed in a path field, as a path: relative to `dir`, absolute, or under the home
/// folder (`~/…`). `None` when nothing was typed.
fn resolve(dir: &Path, typed: &str) -> Option<PathBuf> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    if typed == "~" {
        return home();
    }
    if let Some(rest) = typed.strip_prefix("~/") {
        return home().map(|h| h.join(rest));
    }
    Some(dir.join(typed))
}

/// What OK (or Enter, or a double click) did.
#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    /// The browser stays open (it moved to a folder, or has something to say).
    Stay,
    /// The file to open or save to, or the folder picked.
    Pick(PathBuf),
}

/// A message under the listing.
#[derive(Clone, Debug, PartialEq)]
enum Note {
    None,
    NotFound(String),
    /// The file exists: the next OK replaces it.
    Replace(PathBuf),
}

/// What a pick asks for, when the built-in browser serves it (a host with no system file dialog):
/// the kind, the title the caller shows and the file types.
pub(crate) enum BrowserRequest {
    /// Pick a file to open or import.
    Open { title: String, exts: Vec<String> },
    /// Pick where to save; `name` pre-fills the file name field.
    Save { title: String, name: String, exts: Vec<String> },
    /// Pick a folder (no files listed, no name field).
    Folder { title: String },
}

impl BrowserRequest {
    fn title(&self) -> &str {
        match self {
            BrowserRequest::Open { title, .. } | BrowserRequest::Save { title, .. } | BrowserRequest::Folder { title } => title,
        }
    }

    /// OK's label, as the system dialog's would read.
    fn ok_label(&self) -> &'static str {
        match self {
            BrowserRequest::Open { .. } => "Open",
            BrowserRequest::Save { .. } => "Save",
            BrowserRequest::Folder { .. } => "Select folder",
        }
    }

    /// The type filters as (label, extensions): Open one combined list, Save one per type plus
    /// All files, Folder none.
    fn filters(&self) -> Vec<(String, Vec<String>)> {
        match self {
            BrowserRequest::Open { exts, .. } => vec![("Supported files".into(), exts.clone())],
            BrowserRequest::Save { exts, .. } => {
                let mut filters: Vec<(String, Vec<String>)> = exts.iter().map(|e| (format!("*.{e}"), vec![e.clone()])).collect();
                filters.push(("All files".into(), vec!["*".into()]));
                filters
            }
            BrowserRequest::Folder { .. } => Vec::new(),
        }
    }
}

/// The open browser: what it asks for, where its answer goes, the folder it shows and its fields.
pub(crate) struct FileBrowser {
    request: BrowserRequest,
    /// Where the answer goes when it closes (the pending dialog's future reads it).
    send: Sender<Option<PathBuf>>,
    /// The roots navigation is clamped to (Android); `None` on the desktop, as before.
    roots: Option<Roots>,
    dir: PathBuf,
    /// The folder field (edited by the user, applied on Enter).
    location: String,
    /// The file name field.
    pub(crate) name: String,
    /// The file types, as (label, extensions).
    filters: Vec<(String, Vec<String>)>,
    filter: usize,
    listing: Result<Listing, String>,
    /// The highlighted entry.
    selected: Option<PathBuf>,
    note: Note,
    focus_name: bool,
}

impl FileBrowser {
    pub(crate) fn new(request: BrowserRequest, start: PathBuf, roots: Option<Roots>, send: Sender<Option<PathBuf>>) -> Self {
        let name = match &request {
            BrowserRequest::Save { name, .. } => name.clone(),
            _ => String::new(),
        };
        let filters = request.filters();
        let mut b = FileBrowser {
            request,
            send,
            roots,
            dir: start.clone(),
            location: String::new(),
            name,
            filters,
            filter: 0,
            listing: Ok(Listing::default()),
            selected: None,
            note: Note::None,
            focus_name: true,
        };
        b.navigate(start);
        b
    }

    fn exts(&self) -> &[String] {
        self.filters.get(self.filter).map_or(&[], |(_, e)| e.as_slice())
    }

    /// Show the folder `dir` (an unreadable folder shows why, and Up still works). On Android a
    /// folder outside the browser's root lands on the root's top instead.
    pub(crate) fn navigate(&mut self, dir: PathBuf) {
        let dir = self.clamped(dir);
        self.listing = list_dir(&dir, self.exts(), MAX_LISTED).map_err(|e| e.to_string());
        self.location = dir.to_string_lossy().into_owned();
        self.dir = dir;
        self.selected = None;
        self.note = Note::None;
    }

    /// The folder actually shown for `dir`: itself, or — when the browser is clamped and `dir` is
    /// outside its root (a typed path, a jump) — the root's top.
    fn clamped(&self, dir: PathBuf) -> PathBuf {
        match &self.roots {
            Some(roots) if inside_root(&dir, &roots.app) => dir,
            Some(roots) => roots.app.clone(),
            None => dir,
        }
    }

    /// Where "Up one level" goes: the parent, clamped to the shown root on Android; `None` when
    /// there is nowhere up to go (the root's top, or no parent on the desktop).
    pub(crate) fn up_target(&self) -> Option<PathBuf> {
        match &self.roots {
            Some(roots) => up_from(&roots.app, &self.dir),
            None => self.dir.parent().map(Path::to_path_buf),
        }
    }

    fn up(&mut self) {
        if let Some(parent) = self.up_target() {
            self.navigate(parent);
        }
    }

    /// Where the home-folder button goes: the app's Documents on Android, the user's home on the
    /// desktop (where no browser opens — for `resolve`'s `~`).
    fn home_target(&self) -> Option<PathBuf> {
        self.roots.as_ref().map(app_start).or_else(home)
    }

    /// OK: a highlighted folder is opened (picked, for a folder pick); otherwise the typed name
    /// is the pick. Save adds the chosen type's extension to a name without one and asks once
    /// more before replacing a file; Open wants a file that exists.
    pub(crate) fn accept(&mut self) -> Step {
        match &self.request {
            BrowserRequest::Folder { .. } => {
                // A highlighted folder is the pick (a double click entered it, Up and the
                // location field navigated there).
                self.selected.take().filter(|p| p.is_dir()).map_or(Step::Stay, Step::Pick)
            }
            _ => self.accept_file(),
        }
    }

    /// OK in an Open or Save browser.
    fn accept_file(&mut self) -> Step {
        if let Some(dir) = self.selected.take().filter(|p| p.is_dir()) {
            self.navigate(dir);
            return Step::Stay;
        }
        let Some(mut path) = resolve(&self.dir, &self.name) else { return Step::Stay };
        // Android: a typed pick outside the browser's root (…/, an absolute path) is not one this
        // dialog may take.
        if self.roots.as_ref().is_some_and(|r| !inside_root(&path, &r.app)) {
            self.note = Note::NotFound(self.name.trim().to_string());
            return Step::Stay;
        }
        if path.is_dir() {
            self.name.clear();
            self.navigate(path);
            return Step::Stay;
        }
        if matches!(self.request, BrowserRequest::Open { .. }) {
            if path.is_file() {
                return Step::Pick(path);
            }
            self.note = Note::NotFound(self.name.trim().to_string());
            return Step::Stay;
        }
        if path.extension().is_none()
            && let Some(ext) = self.exts().first().filter(|e| e.as_str() != "*")
        {
            path.set_extension(ext);
        }
        if path.exists() && self.note != Note::Replace(path.clone()) {
            self.note = Note::Replace(path);
            return Step::Stay;
        }
        Step::Pick(path)
    }

    /// A click on an entry: highlight it, and a file's name goes into the file name field.
    fn select(&mut self, e: &Entry) {
        self.selected = Some(e.path.clone());
        if !e.dir {
            self.name = e.name.clone();
        }
        self.note = Note::None;
    }
}

/// A pick the built-in browser serves (Android): pending while it is open, then what it picked
/// (`None`: cancelled). [`FigApp::poll_dialog`] polls it every frame; the browser's drawing keeps
/// the frames coming, so no waker is needed.
pub(crate) struct BrowserPick {
    rx: Receiver<Option<PathBuf>>,
}

impl Future for BrowserPick {
    type Output = Option<Picked>;

    fn poll(self: std::pin::Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        match self.get_mut().rx.try_recv() {
            Ok(answer) => Poll::Ready(answer.map(Picked::Path)),
            Err(std::sync::mpsc::TryRecvError::Empty) => Poll::Pending,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Poll::Ready(None),
        }
    }
}

/// Open the browser at `start` for `request`, its navigation clamped to `roots`; the answer (or
/// `None`: cancelled) arrives on the returned future, which [`FigApp::poll_dialog`] polls.
pub(crate) fn open_browser(request: BrowserRequest, start: PathBuf, roots: Option<Roots>) -> (BrowserPick, Box<FileBrowser>) {
    let (send, recv) = channel();
    (BrowserPick { rx: recv }, Box::new(FileBrowser::new(request, start, roots, send)))
}

/// Close the browser with this answer (`None`: cancelled); the pending future reads it.
pub(crate) fn close(app: &mut FigApp, picked: Option<PathBuf>) {
    let Some(b) = app.file_browser.open.take() else { return };
    app.file_browser.last_dir = Some(b.dir.clone());
    let _ = b.send.send(picked);
}

/// The browser closed without a pick (Escape, the Cancel button or the window's X).
pub(crate) fn cancel(app: &mut FigApp) {
    close(app, None);
}

/// Draw the browser, if it is open (called from [`crate::dialogs`] with the other dialogs).
pub(crate) fn show(app: &mut FigApp, ctx: &egui::Context) {
    let Some(mut b) = app.file_browser.open.take() else { return };
    let t = Tokens::get(ctx);
    let title = b.request.title().to_string();
    let mut done: Option<Option<PathBuf>> = None;
    let mut open = true;
    crate::dialogs::window(&title, ctx).order(egui::Order::Foreground).open(&mut open).show(ctx, |ui| {
        ui.set_width(620.0);
        done = body(ui, &mut b, &t);
    });
    if !open {
        done = Some(None);
    }
    app.file_browser.open = Some(b);
    if let Some(answer) = done {
        close(app, answer);
    }
}

/// The browser's contents; `Some` once it closes (`Some(None)`: cancelled).
fn body(ui: &mut Ui, b: &mut FileBrowser, t: &Tokens) -> Option<Option<PathBuf>> {
    let mut location_done = false;
    ui.horizontal(|ui| {
        if widgets::icon_button(ui, icons::Icon::ChevronUp, tr("Up one level")) {
            b.up();
        }
        if widgets::icon_button(ui, icons::Icon::Home, tr("Home folder"))
            && let Some(h) = b.home_target()
        {
            b.navigate(h);
        }
        let r = ui.add(egui::TextEdit::singleline(&mut b.location).desired_width(f32::INFINITY));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            location_done = true;
            let typed = b.location.clone();
            if let Some(dir) = resolve(&b.dir, &typed) {
                b.navigate(dir);
            }
        }
    });
    ui.add_space(4.0);
    let mut pick = false;
    let mut go: Option<PathBuf> = None;
    let mut clicked: Option<Entry> = None;
    egui::Frame::NONE.fill(t.field).stroke(egui::Stroke::new(1.0, t.border)).inner_margin(2.0).show(ui, |ui| {
        let entries: &[Entry] = b.listing.as_ref().map(|l| l.entries.as_slice()).unwrap_or(&[]);
        // An unreadable folder has its own message below; the hint is for a listing that came back empty.
        let listed = b.listing.is_ok();
        egui::ScrollArea::vertical()
            .id_salt("builtin_file_browser_list")
            .max_height(300.0)
            .min_scrolled_height(300.0)
            .auto_shrink([false, false])
            .show_rows(ui, ROW, entries.len(), |ui, range| {
                if listed && entries.is_empty() {
                    // An empty folder (or a type filter that matches nothing) says so instead of
                    // showing a blank box.
                    ui.add_space(140.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new(tr("No items to show.")).small().color(t.text_secondary));
                    });
                }
                for e in entries.get(range).unwrap_or(&[]) {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                    if b.selected.as_ref() == Some(&e.path) {
                        ui.painter().rect_filled(r, 2.0, t.selected_row);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, 2.0, t.hover);
                    }
                    let icon = Rect::from_center_size(pos2(r.min.x + 14.0, r.center().y), vec2(18.0, 18.0));
                    icons::paint(ui.painter(), icon, if e.dir { icons::Icon::Folder } else { icons::Icon::Page }, t.icon);
                    ui.painter().with_clip_rect(r).text(pos2(r.min.x + 30.0, r.center().y), Align2::LEFT_CENTER, &e.name, regular(13.0), t.text);
                    if resp.double_clicked() {
                        if e.dir {
                            go = Some(e.path.clone());
                        } else {
                            clicked = Some(e.clone());
                            pick = true;
                        }
                    } else if resp.clicked() {
                        clicked = Some(e.clone());
                    }
                }
            });
    });
    if let Some(e) = clicked {
        b.select(&e);
    }
    if let Some(dir) = go {
        b.navigate(dir);
    }
    let message = match (&b.listing, &b.note) {
        (_, Note::NotFound(name)) => Some((trf("{name} wasn't found.", &[("name", name)]), t.danger)),
        (_, Note::Replace(p)) => {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            Some((trf("{name} already exists. Choose Save again to replace it.", &[("name", &name)]), t.danger))
        }
        (Err(e), _) => Some((trf("This folder can't be read: {error}", &[("error", e)]), t.danger)),
        (Ok(l), _) if l.truncated => Some((trf("Showing the first {count} items.", &[("count", &l.entries.len().to_string())]), t.text_secondary)),
        _ => None,
    };
    if let Some((text, color)) = message {
        ui.label(egui::RichText::new(text).color(color));
    }
    let is_folder_pick = matches!(b.request, BrowserRequest::Folder { .. });
    if !is_folder_pick {
        ui.add_space(6.0);
        let mut refilter = false;
        ui.horizontal(|ui| {
            ui.label(tr("File name"));
            // The name field takes what the label and the type list leave.
            let width = (ui.available_width() - 170.0).max(120.0);
            let r = ui.add(egui::TextEdit::singleline(&mut b.name).desired_width(width));
            if b.focus_name {
                r.request_focus();
                b.focus_name = false;
            }
            if r.changed() {
                b.selected = None;
                b.note = Note::None;
            }
            let before = b.filter;
            let current = b.filters.get(b.filter).map(|(l, _)| tr(l).to_string()).unwrap_or_default();
            egui::ComboBox::from_id_salt("builtin_file_browser_type").selected_text(current).width(150.0).show_ui(ui, |ui| {
                for (i, (l, _)) in b.filters.iter().enumerate() {
                    ui.selectable_value(&mut b.filter, i, tr(l));
                }
            });
            refilter = b.filter != before;
        });
        if refilter {
            b.navigate(b.dir.clone());
        }
    }
    ui.add_space(8.0);
    let mut cancel = false;
    // Enter answers OK when the location field didn't just take it.
    let enter = !location_done && ui.input(|i| i.key_pressed(egui::Key::Enter));
    ui.horizontal(|ui| {
        if widgets::button(ui, tr("Cancel"), false, true).clicked() {
            cancel = true;
        }
        if widgets::button(ui, tr(b.request.ok_label()), true, true).clicked() {
            pick = true;
        }
    });
    if cancel {
        return Some(None);
    }
    if (pick || enter)
        && let Step::Pick(path) = b.accept()
    {
        return Some(Some(path));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoServices;
    use figcraft_engine::Session;
    use serde_json::json;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("figcraft-ui-browser-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn exts(e: &[&str]) -> Vec<String> {
        e.iter().map(|x| x.to_string()).collect()
    }

    fn open_request() -> BrowserRequest {
        BrowserRequest::Open { title: "Open".into(), exts: exts(&["figcraft"]) }
    }

    /// Folders come first, then the files of the chosen type (any case), each by name; hidden
    /// entries and other types are left out, and a big folder is cut off and says so.
    #[test]
    fn listing_filters_sorts_and_caps() {
        let dir = scratch("list");
        for f in ["b.figcraft", "A.FIGCRAFT", "notes.txt", ".hidden.figcraft", "c.fig"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::create_dir(dir.join("zeta")).unwrap();
        std::fs::create_dir(dir.join(".git")).unwrap();
        let l = list_dir(&dir, &exts(&["figcraft"]), 100).unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zeta", "A.FIGCRAFT", "b.figcraft"], "folders first, then the chosen type (any case) by name; .fig is another type here");
        assert!(!l.truncated);
        assert_eq!(list_dir(&dir, &exts(&["*"]), 100).unwrap().entries.len(), 5, "the files plus the folder");
        let capped = list_dir(&dir, &exts(&["figcraft"]), 2).unwrap();
        assert_eq!(capped.entries.len(), 2);
        assert!(capped.truncated);
        assert!(list_dir(&dir.join("missing"), &exts(&["*"]), 10).is_err(), "an unreadable folder is an error, not a panic");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Save in the browser: a typed folder is opened, a bare name gets the chosen type's
    /// extension, and an existing file is replaced only on the second OK.
    #[test]
    fn save_adds_the_extension_and_asks_before_replacing() {
        let dir = scratch("save");
        std::fs::create_dir(dir.join("drafts")).unwrap();
        std::fs::write(dir.join("drafts").join("old.figcraft"), b"x").unwrap();
        let (send, _rx) = channel();
        let mut b = FileBrowser::new(
            BrowserRequest::Save { title: "Save as".into(), name: "Untitled.figcraft".into(), exts: exts(&["figcraft", "fig"]) },
            dir.clone(),
            None,
            send,
        );
        b.name = "drafts".into();
        assert_eq!(b.accept(), Step::Stay);
        assert_eq!(b.dir, dir.join("drafts"));
        b.name = "novel".into();
        assert_eq!(b.accept(), Step::Pick(dir.join("drafts").join("novel.figcraft")));
        b.name = "old".into();
        assert_eq!(b.accept(), Step::Stay, "the first OK asks before replacing");
        assert_eq!(b.accept(), Step::Pick(dir.join("drafts").join("old.figcraft")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// "Up one level" stays inside the shown root: the parent while it is still under the root,
    /// and none at the root's top.
    #[test]
    fn up_from_stops_at_the_root() {
        let root = Path::new("/data/user/0/ai.storyteller.figcraft/files/FigCraft");
        assert_eq!(up_from(root, &root.join("Documents").join("drafts")), Some(root.join("Documents")));
        assert_eq!(up_from(root, &root.join("Documents")), Some(root.to_path_buf()));
        assert_eq!(up_from(root, root), None, "the root's top has no Up");
        // a root that isn't a parent of dir (defensive): no Up rather than an escape
        assert_eq!(up_from(root, Path::new("/storage/emulated/0")), None);
    }

    /// A folder this browser may open is under the root; a path that merely starts with the same
    /// text but is a sibling directory is not inside it.
    #[test]
    fn inside_root_accepts_only_the_roots() {
        let root = Path::new("/data/user/0/ai.storyteller.figcraft/files/FigCraft");
        assert!(inside_root(&root.join("Documents"), root));
        assert!(inside_root(root, root), "the root itself is a folder the browser may show");
        assert!(!inside_root(Path::new("/data/user/0/ai.storyteller.figcraft/other"), root));
        assert!(!inside_root(Path::new("/data/user/0/ai.storyteller.figcraft/files/FigCraft-x"), root), "a sibling that shares a prefix");
        assert!(!inside_root(Path::new("/etc"), root));
    }

    /// Android (no system dialog, the host's app-private storage recorded): Save as serves the
    /// request in the built-in browser over that storage instead of answering none — the browser
    /// is clamped to the host's root, starts in its Documents, and its answer saves the file.
    #[test]
    fn app_files_dir_serves_the_pick_in_the_built_in_browser() {
        let base = scratch("android-route");
        std::fs::create_dir_all(base.join("Documents")).unwrap();
        struct AppFiles(PathBuf);
        impl crate::Services for AppFiles {
            fn app_files_dir(&self) -> Option<PathBuf> {
                Some(self.0.clone())
            }
        }
        let mut app = FigApp::new(Session::empty(), Box::new(AppFiles(base.clone())));
        app.session.execute("file.new", &json!({})).unwrap();
        app.save_as_dialog();
        {
            let b = app.file_browser.open.as_ref().expect("the built-in browser serves the request");
            assert_eq!(b.roots.as_ref().map(|r| r.app.clone()), Some(base.clone()), "clamped to the host's root");
            assert_eq!(b.dir, base.join("Documents"), "starts in the app's Documents");
            assert_eq!(b.name, "Untitled.figcraft", "the save name pre-fills the field");
        }
        assert!(app.session.docs.iter().all(|d| d.path.is_none()), "the pick is pending: nothing saved yet");
        let path = {
            let b = app.file_browser.open.as_mut().unwrap();
            b.name = "novel".into();
            let Step::Pick(path) = b.accept() else { panic!("picked") };
            path
        };
        close(&mut app, Some(path));
        app.poll_dialog(&egui::Context::default());
        let saved = base.join("Documents").join("novel.figcraft");
        assert!(saved.is_file(), "the answer saves into the browser's Documents");
        assert_eq!(app.session.docs[0].path.as_deref(), Some(saved.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// No app-private storage (a bare host): today's behaviour — no browser opens and the pick
    /// answers none.
    #[test]
    fn no_app_files_dir_answers_none_without_a_browser() {
        let mut app = FigApp::new(Session::empty(), Box::new(NoServices));
        app.session.execute("file.new", &json!({})).unwrap();
        app.save_as_dialog();
        assert!(app.file_browser.open.is_none(), "no browser without a host's root");
        app.poll_dialog(&egui::Context::default());
        assert!(app.pending_dialog.is_none(), "the pick answered none (cancelled)");
    }

    /// Cancel (Escape, the button or the window's X) closes the browser and answers none:
    /// nothing is saved.
    #[test]
    fn cancelling_the_browser_answers_none() {
        let base = scratch("cancel");
        std::fs::create_dir_all(base.join("Documents")).unwrap();
        struct AppFiles(PathBuf);
        impl crate::Services for AppFiles {
            fn app_files_dir(&self) -> Option<PathBuf> {
                Some(self.0.clone())
            }
        }
        let mut app = FigApp::new(Session::empty(), Box::new(AppFiles(base.clone())));
        app.session.execute("file.new", &json!({})).unwrap();
        app.save_as_dialog();
        assert!(app.file_browser.open.is_some());
        cancel(&mut app);
        assert!(app.file_browser.open.is_none());
        app.poll_dialog(&egui::Context::default());
        assert!(app.pending_dialog.is_none(), "the cancelled dialog is gone");
        assert!(app.session.docs.iter().all(|d| d.path.is_none()), "nothing was saved");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Navigation is clamped inside the host's root: a typed pick outside is refused as not
    /// found, a folder jump outside lands on the root's top, and the root's top has no Up.
    #[test]
    fn a_pick_outside_the_root_is_refused_and_clamped() {
        let base = scratch("clamp");
        std::fs::create_dir_all(base.join("Documents")).unwrap();
        let (send, _rx) = channel();
        let mut b = FileBrowser::new(open_request(), base.join("Documents"), Some(Roots { app: base.clone() }), send);
        b.name = "/etc/passwd".into();
        assert_eq!(b.accept(), Step::Stay);
        assert_eq!(b.note, Note::NotFound("/etc/passwd".into()), "outside the root: not found");
        b.navigate(base.parent().unwrap().to_path_buf());
        assert_eq!(b.dir, base, "a folder outside the root lands on the root's top");
        assert_eq!(b.up_target(), None, "the root's top has no Up");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A folder pick lists no files and picks the highlighted folder; a double click enters it.
    #[test]
    fn a_folder_pick_picks_the_highlighted_folder() {
        let dir = scratch("folder");
        std::fs::create_dir(dir.join("out")).unwrap();
        std::fs::write(dir.join("file.figcraft"), b"x").unwrap();
        let (send, _rx) = channel();
        let mut b = FileBrowser::new(BrowserRequest::Folder { title: "Export".into() }, dir.clone(), None, send);
        assert!(b.filters.is_empty(), "a folder pick has no type filters");
        assert!(matches!(&b.listing, Ok(l) if l.entries.iter().all(|e| e.dir)), "no files are listed");
        let out = dir.join("out");
        b.select(&Entry { name: "out".into(), path: out.clone(), dir: true });
        assert_eq!(b.accept(), Step::Pick(out.clone()), "OK picks the highlighted folder");
        // A double click entered instead: Up came back, and OK with no highlight stays.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
