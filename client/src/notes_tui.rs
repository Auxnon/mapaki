//! `mapaki notes`: browse, search and edit the `.note` files in the synced
//! folder. Editing hands the note to $EDITOR as plain text (title on the
//! first line, then the body) and writes it back in the canonical format.
//!
//! Notes are read from the local folder. If `mapaki run` is active it syncs
//! edits on its own; otherwise this syncs on open and after every save.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::config::{self, Config, State};
use crate::note::{self, Note};
use crate::sync::{self, BoxError, Syncer};

/// List rows show this much of a note under its title; the terminal width clips the rest.
const SNIPPET_CHARS: usize = 120;

struct Item {
    file: String,
    note: Note,
    mtime: i64,
}

#[derive(PartialEq)]
enum Mode {
    List,
    Search,
    View,
}

struct App {
    cfg: Config,
    background: bool,
    items: Vec<Item>,
    /// Indexes into `items` matching the search.
    shown: Vec<usize>,
    list: ListState,
    query: String,
    mode: Mode,
    scroll: u16,
    status: String,
}

pub fn run(cfg: Config) -> Result<(), BoxError> {
    let background = config::service_running();
    let mut app = App {
        cfg,
        background,
        items: vec![],
        shown: vec![],
        list: ListState::default(),
        query: String::new(),
        mode: Mode::List,
        scroll: 0,
        status: String::new(),
    };
    if !app.background {
        eprintln!("mapaki: syncing…");
        app.sync();
    }
    app.reload(None);

    let mut terminal = ratatui::init();
    let result = app.event_loop(&mut terminal);
    ratatui::restore();
    result
}

impl App {
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<(), BoxError> {
        loop {
            terminal.draw(|f| self.draw(f))?;
            let Event::Key(key) = event::read()? else { continue };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }
            match self.mode {
                Mode::Search => match key.code {
                    KeyCode::Enter => self.mode = Mode::List,
                    KeyCode::Esc => {
                        self.query.clear();
                        self.filter();
                        self.mode = Mode::List;
                    }
                    KeyCode::Backspace => {
                        self.query.pop();
                        self.filter();
                    }
                    KeyCode::Char(c) => {
                        self.query.push(c);
                        self.filter();
                    }
                    KeyCode::Down => self.step(1),
                    KeyCode::Up => self.step(-1),
                    _ => {}
                },
                Mode::List => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('j') | KeyCode::Down => self.step(1),
                    KeyCode::Char('k') | KeyCode::Up => self.step(-1),
                    KeyCode::Char('g') | KeyCode::Home => self.select(0),
                    KeyCode::Char('G') | KeyCode::End => self.select(self.shown.len().saturating_sub(1)),
                    KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right if self.current().is_some() => {
                        self.mode = Mode::View;
                        self.scroll = 0;
                    }
                    KeyCode::Char('/') => self.mode = Mode::Search,
                    KeyCode::Char('e') => self.edit(terminal, false)?,
                    KeyCode::Char('n') => self.edit(terminal, true)?,
                    KeyCode::Char('o') => self.open_images(),
                    KeyCode::Char('r') => {
                        if !self.background {
                            self.sync();
                        }
                        let keep = self.current().map(|i| i.file.clone());
                        self.reload(keep);
                    }
                    _ => {}
                },
                Mode::View => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('h') | KeyCode::Left => self.mode = Mode::List,
                    KeyCode::Char('j') | KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                    KeyCode::Char('k') | KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                    KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = self.scroll.saturating_add(20),
                    KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(20),
                    KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
                    KeyCode::Char('e') => self.edit(terminal, false)?,
                    KeyCode::Char('o') => self.open_images(),
                    _ => {}
                },
            }
        }
    }

    fn sync(&mut self) {
        let result = crate::connect(&self.cfg).and_then(|api| {
            let mut state = State::load();
            let mut never = |_: &str, _: &crate::api::Entry| None;
            let report = Syncer::new(&api, &self.cfg.folder, &mut state, &mut never).run()?;
            Ok(report)
        });
        self.status = match result {
            Ok(r) if r.conflicts.is_empty() => format!("synced: {}", r.summary()),
            Ok(r) => format!("{} — settle with `mapaki conflicts` / `mapaki resolve`", r.summary()),
            Err(e) => format!("not synced: {e}"),
        };
    }

    fn reload(&mut self, keep: Option<String>) {
        self.items = load_notes(&self.cfg.folder);
        self.filter();
        if let Some(file) = keep {
            if let Some(pos) = self.shown.iter().position(|&i| self.items[i].file == file) {
                self.select(pos);
            }
        }
    }

    fn filter(&mut self) {
        let q = self.query.to_lowercase();
        self.shown = (0..self.items.len())
            .filter(|&i| {
                let n = &self.items[i].note;
                q.is_empty()
                    || n.display_title().to_lowercase().contains(&q)
                    || n.body.to_lowercase().contains(&q)
                    || self.items[i].file.to_lowercase().contains(&q)
            })
            .collect();
        self.select(0);
    }

    fn select(&mut self, pos: usize) {
        self.list.select(if self.shown.is_empty() { None } else { Some(pos.min(self.shown.len() - 1)) });
    }

    fn step(&mut self, by: i32) {
        let cur = self.list.selected().unwrap_or(0) as i32;
        self.select((cur + by).max(0) as usize);
    }

    fn current(&self) -> Option<&Item> {
        self.list.selected().and_then(|p| self.shown.get(p)).map(|&i| &self.items[i])
    }

    /// Edit the selected note (or a new one) in $EDITOR, then save and sync.
    fn edit(&mut self, terminal: &mut DefaultTerminal, new: bool) -> Result<(), BoxError> {
        let (file, mut note) = match (new, self.current()) {
            (true, _) => (None, Note { created: note::now_rfc3339(), ..Note::default() }),
            (false, Some(item)) => (Some(item.file.clone()), item.note.clone()),
            (false, None) => return Ok(()),
        };
        let before = to_editable(&note);
        ratatui::restore();
        let edited = run_editor(&before);
        *terminal = ratatui::init();
        let edited = match edited {
            Ok(text) => text,
            Err(e) => {
                self.status = format!("editor failed: {e}");
                return Ok(());
            }
        };
        if edited == before || (new && edited.trim().is_empty()) {
            self.status = "no changes".into();
            return Ok(());
        }
        (note.title, note.body) = from_editable(&edited);
        let file = match file {
            Some(f) => f,
            None => free_name(&self.cfg.folder, &file_stem_for(&note)),
        };
        if let Err(e) = write_note(&self.cfg.folder, &file, &note) {
            self.status = format!("couldn't save {file}: {e}");
            return Ok(());
        }
        if self.background {
            self.status = format!("saved {file}; the background service will sync it");
        } else {
            self.sync();
            self.status = format!("saved {file}, {}", self.status);
        }
        self.query.clear();
        self.reload(Some(file));
        Ok(())
    }

    fn open_images(&mut self) {
        let Some(item) = self.current() else { return };
        if item.note.images.is_empty() {
            self.status = "this note has no images".into();
            return;
        }
        let dir = std::env::temp_dir().join("mapaki-images");
        let mut opened = 0;
        for (i, img) in item.note.images.iter().enumerate() {
            let name = if img.name.is_empty() || img.name.contains(['/', '\\']) {
                format!("image-{i}.{}", img.mime.rsplit('/').next().unwrap_or("jpg"))
            } else {
                img.name.clone()
            };
            let path = dir.join(name);
            if fs::create_dir_all(&dir).and_then(|_| fs::write(&path, &img.data)).is_ok() && open_file(&path) {
                opened += 1;
            }
        }
        self.status = format!("opened {opened} of {} image(s)", item.note.images.len());
    }

    fn draw(&mut self, f: &mut Frame) {
        let [main, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(f.area());
        let wide = main.width >= 80;
        match self.mode {
            Mode::View => self.draw_note(f, main),
            _ if wide => {
                let [left, right] =
                    Layout::horizontal([Constraint::Percentage(35), Constraint::Min(20)]).areas(main);
                self.draw_list(f, left);
                self.draw_note(f, right);
            }
            _ => self.draw_list(f, main),
        }
        self.draw_footer(f, footer);
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .shown
            .iter()
            .map(|&i| {
                let it = &self.items[i];
                let date = if it.note.created.len() >= 10 {
                    it.note.created[..10].to_string()
                } else {
                    crate::civil(it.mtime).0
                };
                let mut spans = vec![Span::raw(it.note.display_title()), Span::raw("  "), Span::raw(date).dim()];
                if !it.note.images.is_empty() {
                    spans.push(Span::raw(format!("  [{}]", it.note.images.len())).dim());
                }
                let snippet = it.note.snippet(SNIPPET_CHARS);
                if snippet.is_empty() {
                    ListItem::new(Line::from(spans))
                } else {
                    ListItem::new(vec![Line::from(spans), Line::from(format!("  {snippet}")).dim()])
                }
            })
            .collect();
        let title = if self.query.is_empty() {
            format!(" notes ({}) ", self.items.len())
        } else {
            format!(" notes ({}/{}) /{} ", self.shown.len(), self.items.len(), self.query)
        };
        let list = List::new(items)
            .block(Block::bordered().title(title))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("› ");
        f.render_stateful_widget(list, area, &mut self.list);
    }

    fn draw_note(&self, f: &mut Frame, area: Rect) {
        let Some(item) = self.current() else {
            let hint = if self.items.is_empty() { "no notes yet — press n to write one" } else { "no match" };
            f.render_widget(Paragraph::new(hint).dim().block(Block::bordered()), area);
            return;
        };
        let note = &item.note;
        let mut lines: Vec<Line> = vec![];
        let mut shown_images = vec![];
        for l in note.body.lines() {
            match note::image_ref(l) {
                Some(name) => {
                    shown_images.push(name);
                    lines.push(image_line(note, name));
                }
                None => lines.push(Line::raw(l.to_string())),
            }
        }
        // Images the body doesn't place go at the end.
        let rest: Vec<_> = note.images.iter().filter(|i| !shown_images.contains(&i.name.as_str())).collect();
        if !rest.is_empty() {
            lines.push(Line::raw(""));
            for img in rest {
                lines.push(image_line(note, &img.name));
            }
        }
        let block = Block::bordered()
            .title(Line::from(format!(" {} ", note.display_title())).bold())
            .title_bottom(Line::from(format!(" {} ", item.file)).dim());
        let scroll = if self.mode == Mode::View { self.scroll } else { 0 };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(block), area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let keys = match self.mode {
            Mode::List => "↑↓ move  enter read  / search  e edit  n new  o open images  r refresh  q quit",
            Mode::Search => "type to search  enter keep  esc clear",
            Mode::View => "↑↓ scroll  e edit  o open images  esc back",
        };
        let sync = if self.background { "background sync on" } else { "sync on open/save" };
        let text = vec![
            Line::from(vec![Span::raw(keys).dim(), Span::raw(format!("  · {sync}")).dim()]),
            Line::raw(self.status.clone()),
        ];
        f.render_widget(Paragraph::new(text), area);
    }
}

fn image_line<'a>(note: &Note, name: &str) -> Line<'a> {
    let label = match note.images.iter().find(|i| i.name == name) {
        Some(img) => format!("[image: {name} · {}]", human_size(img.data.len())),
        None => format!("[missing image: {name}]"),
    };
    Line::from(Span::raw(label).italic().cyan())
}

fn human_size(n: usize) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

fn load_notes(folder: &Path) -> Vec<Item> {
    let mut out: Vec<Item> = fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().into_string().ok()?;
            let is_note = Path::new(&file).extension().is_some_and(|x| x.eq_ignore_ascii_case("note"));
            if !is_note || !sync::is_synced_name(&file) {
                return None;
            }
            let mtime = e
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Some(Item { note: Note::parse(&fs::read(e.path()).ok()?), file, mtime })
        })
        .collect();
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.file.cmp(&b.file)));
    out
}

/// What $EDITOR sees: the title, a blank line, then the body.
fn to_editable(note: &Note) -> String {
    format!("{}\n\n{}", note.title, note.body)
}

fn from_editable(text: &str) -> (String, String) {
    let (title, body) = text.split_once('\n').unwrap_or((text, ""));
    let body = body.strip_prefix('\n').unwrap_or(body);
    (title.trim().to_string(), body.to_string())
}

fn run_editor(initial: &str) -> std::io::Result<String> {
    let path = std::env::temp_dir().join(format!("mapaki-note-{}.md", std::process::id()));
    fs::write(&path, initial)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| if cfg!(windows) { "notepad".into() } else { "vi".into() });
    // $EDITOR may carry flags, like `code --wait`.
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = Command::new(program).args(parts).arg(&path).status();
    let text = fs::read_to_string(&path);
    let _ = fs::remove_file(&path);
    match status {
        Ok(s) if s.success() => text,
        Ok(s) => Err(std::io::Error::other(format!("{editor} exited with {s}"))),
        Err(e) => Err(std::io::Error::other(format!("couldn't start {editor}: {e}"))),
    }
}

/// File name for a new note from its title: readable, safe on every OS.
pub fn file_stem_for(note: &Note) -> String {
    let title = note.display_title();
    let stem: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_()".contains(c) { c } else { '-' })
        .collect::<String>()
        .trim_matches([' ', '-', '.'])
        .chars()
        .take(60)
        .collect();
    if stem.is_empty() || stem == "(untitled)" { format!("note-{}", crate::civil(now()).0) } else { stem }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn free_name(folder: &Path, stem: &str) -> String {
    let mut name = format!("{stem}.note");
    let mut n = 2;
    while folder.join(&name).exists() {
        name = format!("{stem}-{n}.note");
        n += 1;
    }
    name
}

fn write_note(folder: &Path, file: &str, note: &Note) -> std::io::Result<()> {
    fs::create_dir_all(folder)?;
    let tmp: PathBuf = folder.join(format!(".{file}.mapaki-tmp"));
    fs::write(&tmp, note.to_toml())?;
    fs::rename(&tmp, folder.join(file))
}

fn open_file(path: &Path) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        Command::new("xdg-open")
    };
    cmd.arg(path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editable_round_trip() {
        let n = Note { title: "Hi".into(), body: "line\n\nmore\n".into(), ..Note::default() };
        assert_eq!(from_editable(&to_editable(&n)), ("Hi".into(), "line\n\nmore\n".into()));
        assert_eq!(from_editable("only a title"), ("only a title".into(), "".into()));
    }

    #[test]
    fn new_note_names() {
        let n = Note { title: "Trip: Oslo / Bergen?".into(), ..Note::default() };
        assert_eq!(file_stem_for(&n), "Trip- Oslo - Bergen");
        assert!(file_stem_for(&Note::default()).starts_with("note-"));
    }
}
