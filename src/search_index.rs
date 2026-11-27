//! Índice local de nombres. La instantánea se consulta desde disco para
//! evitar mantener todos los nombres de los volúmenes en la memoria.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}};
use std::time::{Duration, Instant, SystemTime};
use windows::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE};

use notify::{Event, EventKind, RecursiveMode, Watcher, event::ModifyKind};

const MAX_RESULTS: usize = 9;
const MAX_CHANGES: usize = 16_384;
const NORMAL_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const LIVE_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
struct State {
    building: bool,
    live: bool,
    watched_drives: usize,
    error: Option<String>,
    failed_at: Option<Instant>,
    overflowed: bool,
    revision: u64,
    changes: HashMap<PathBuf, u64>,
}

pub(crate) struct SearchIndex {
    state: Arc<Mutex<State>>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    path: Option<PathBuf>,
}

#[derive(PartialEq, Eq)]
pub(crate) struct IndexStatus {
    pub ready: bool,
    pub building: bool,
    pub watched_drives: usize,
    pub error: Option<String>,
}

impl SearchIndex {
    pub fn new(live: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(State { live, ..State::default() })),
            watcher: Mutex::new(None),
            path: crate::settings::cache_dir().and_then(|dir| dir.parent().map(|parent| parent.join("search-index-v1.txt"))),
        }
    }

    pub fn status(&self) -> IndexStatus {
        let state = self.state.lock().unwrap();
        IndexStatus {
            ready: self.path.as_ref().is_some_and(|path| path.is_file()),
            building: state.building,
            watched_drives: state.watched_drives,
            error: state.error.clone(),
        }
    }

    pub fn set_live(self: &Arc<Self>, enabled: bool) {
        let mut state = self.state.lock().unwrap();
        state.live = enabled;
        if !enabled {
            state.changes.clear();
            state.watched_drives = 0;
        }
        drop(state);
        let index = self.clone();
        std::thread::spawn(move || index.configure_watcher());
    }

    fn configure_watcher(self: &Arc<Self>) {
        let mut watcher_slot = self.watcher.lock().unwrap();
        let enabled = self.state.lock().unwrap().live;
        if !enabled {
            *watcher_slot = None;
            return;
        }
        if watcher_slot.is_some() { return; }
        let state = self.state.clone();
        let index_dir = self.path.as_ref().and_then(|path| path.parent()).map(Path::to_path_buf);
        let Ok(mut watcher) = notify::recommended_watcher(move |event: notify::Result<Event>| {
            Self::record_changes(&state, index_dir.as_deref(), event);
        }) else { return; };
        let count = crate::platform::fixed_drive_roots().into_iter().filter(|path| {
            watcher.watch(path, RecursiveMode::Recursive).is_ok()
        }).count();
        self.state.lock().unwrap().watched_drives = count;
        if count != 0 {
            *watcher_slot = Some(watcher);
        }
    }

    fn record_changes(state: &Mutex<State>, index_dir: Option<&Path>, event: notify::Result<Event>) {
        let mut state = state.lock().unwrap();
        if !state.live { return; }
        match event {
            Ok(event) if matches!(event.kind, EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))) => {
                if event.need_rescan() { state.overflowed = true; }
                for path in event.paths {
                    if index_dir.is_some_and(|dir| path.starts_with(dir)) {
                        continue;
                    }
                    if state.changes.len() >= MAX_CHANGES {
                        state.overflowed = true;
                        break;
                    }
                    state.revision = state.revision.wrapping_add(1);
                    let revision = state.revision;
                    state.changes.insert(path, revision);
                }
            }
            Ok(event) if event.need_rescan() => state.overflowed = true,
            Err(_) => state.overflowed = true,
            _ => {}
        }
    }

    pub fn schedule(self: &Arc<Self>, force: bool) -> bool {
        let Some(path) = self.path.clone() else { return false; };
        let mut state = self.state.lock().unwrap();
        if state.building { return false; }
        if !force && state.failed_at.is_some_and(|time| time.elapsed() < Duration::from_secs(60)) { return false; }
        let ttl = if state.live { LIVE_TTL } else { NORMAL_TTL };
        let is_fresh = fs::metadata(&path).ok().and_then(|meta| meta.modified().ok())
            .and_then(|stamp| stamp.elapsed().ok()).is_some_and(|age| age < ttl);
        if !force && is_fresh && !state.overflowed { return false; }
        state.building = true;
        state.error = None;
        state.failed_at = None;
        state.overflowed = false;
        let revision = state.revision;
        drop(state);

        let index = self.clone();
        let spawn = std::thread::Builder::new().name("xool-search-index".into()).spawn(move || {
            let result = rebuild(&path, &crate::platform::fixed_drive_roots());
            let mut state = index.state.lock().unwrap();
            state.building = false;
            match result {
                Ok(()) => state.changes.retain(|_, modified| *modified > revision),
                Err(error) => {
                    state.failed_at = Some(Instant::now());
                    state.error = Some(crate::localized!(format, "No se pudo actualizar el índice: {error}", "Could not update the index: {error}"));
                }
            }
        });
        if let Err(error) = spawn {
            let mut state = self.state.lock().unwrap();
            state.building = false;
            state.failed_at = Some(Instant::now());
            state.error = Some(crate::localized!(format, "No se pudo iniciar la indexación: {error}", "Could not start indexing: {error}"));
            return false;
        }
        true
    }

    pub fn find(&self, text: &str, ignore_accents: bool, generation: &AtomicU64, token: u64) -> Vec<PathBuf> {
        self.find_matching(text, ignore_accents, generation, token, Path::exists)
    }

    pub fn find_folders(&self, text: &str, ignore_accents: bool, generation: &AtomicU64, token: u64) -> Vec<PathBuf> {
        self.find_matching(text, ignore_accents, generation, token, Path::is_dir)
    }

    fn find_matching(
        &self, text: &str, ignore_accents: bool, generation: &AtomicU64, token: u64,
        accepts: impl Fn(&Path) -> bool,
    ) -> Vec<PathBuf> {
        let mut query = SearchTerms::new(text, ignore_accents);
        if query.terms.is_empty() { return Vec::new(); }
        let mut matches = Vec::new();
        let mut seen = HashSet::new();
        let cancelled = || generation.load(Ordering::Relaxed) != token;
        let changes: Vec<_> = self.state.lock().unwrap().changes.keys().cloned().collect();

        for path in changes {
            if cancelled() || matches.len() >= MAX_RESULTS { return matches; }
            if query.matches(&path) && accepts(&path) && seen.insert(path.clone()) { matches.push(path); }
        }

        let Some(file) = self.path.as_ref().and_then(|path| {
            // Allow the background scan to replace a snapshot while a search still reads it.
            OpenOptions::new().read(true).share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0).open(path).ok()
        }) else { return matches; };
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let mut line = String::new();
        while !cancelled() && matches.len() < MAX_RESULTS {
            line.clear();
            if !matches!(reader.read_line(&mut line), Ok(count) if count != 0) { break; }
            let candidate = line.trim_end_matches(['\r', '\n']);
            if query.matches_index_line(candidate) {
                let path = Path::new(candidate);
                if accepts(path) && seen.insert(path.to_path_buf()) {
                    matches.push(path.to_path_buf());
                }
            }
        }
        matches
    }
}

fn rebuild(destination: &Path, roots: &[PathBuf]) -> io::Result<()> {
    let Some(parent) = destination.parent() else {
        return Err(io::Error::other(crate::i18n::tr("no hay una carpeta para el índice", "no folder is available for the index")));
    };
    fs::create_dir_all(parent)?;
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let temp = destination.with_extension(format!("{}.{}.tmp", std::process::id(), stamp.as_nanos()));
    let result = (|| {
        let mut writer = BufWriter::with_capacity(64 * 1024, File::create(&temp)?);
        let mut stack = roots.to_vec();
        while let Some(folder) = stack.pop() {
            let Ok(entries) = fs::read_dir(&folder) else { continue; };
            for entry in entries.flatten() {
                let path = entry.path();
                if path == temp || path == destination { continue; }
                writeln!(writer, "{}", path.display())?;
                if entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && entry.metadata().is_ok_and(|meta| meta.file_attributes() & 0x400 == 0)
                {
                    stack.push(path);
                }
            }
        }
        writer.flush()?;
        drop(writer);
        fs::rename(&temp, destination)
    })();
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result
}

struct SearchTerm {
    text: String,
    is_path: bool,
    has_wildcard: bool,
}

pub(crate) struct SearchTerms {
    terms: Vec<SearchTerm>,
    ignore_accents: bool,
    normalized_name: String,
    normalized_path: String,
}

impl SearchTerms {
    pub fn new(input: &str, ignore_accents: bool) -> Self {
        let terms = crate::fs_model::search_text(input, ignore_accents).split_whitespace()
            .map(|term| SearchTerm {
                text: term.to_owned(),
                is_path: term.contains('\\'),
                has_wildcard: term.contains(['*', '?']),
            }).collect();
        Self {
            terms, ignore_accents,
            normalized_name: String::new(),
            normalized_path: String::new(),
        }
    }

    pub fn matches(&mut self, path: &Path) -> bool {
        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
        self.matches_text(&path.to_string_lossy(), &name)
    }

    fn matches_index_line(&mut self, path: &str) -> bool {
        let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
        self.matches_text(path, name)
    }

    fn matches_text(&mut self, path: &str, name: &str) -> bool {
        let mut normalize_name = true;
        let mut normalize_path = true;
        for term in &self.terms {
            let (target, source, pending) = if term.is_path {
                (&mut self.normalized_path, path, &mut normalize_path)
            } else {
                (&mut self.normalized_name, name, &mut normalize_name)
            };
            if *pending {
                normalize_search_text(target, source, self.ignore_accents);
                *pending = false;
            }
            let matched = if term.has_wildcard {
                wildcard_match(target, &term.text)
            } else {
                target.contains(&term.text)
            };
            if !matched { return false; }
        }
        true
    }
}

fn normalize_search_text(buffer: &mut String, text: &str, ignore_accents: bool) {
    buffer.clear();
    if text.is_ascii() {
        buffer.push_str(text);
        buffer.make_ascii_lowercase();
    } else {
        buffer.push_str(&crate::fs_model::search_text(text, ignore_accents));
    }
}

// Match a glob against one file name or path. A '?' matches one Unicode
// character and '*' may match any number of characters, including zero.
fn wildcard_match(text: &str, pattern: &str) -> bool {
    let mut text = text.chars();
    let mut pattern = pattern.chars().peekable();
    let mut restart = None;
    loop {
        match (text.clone().next(), pattern.peek().copied()) {
            (_, Some('*')) => {
                pattern.next();
                restart = Some((text.clone(), pattern.clone()));
            }
            (Some(ch), Some(expected)) if expected == '?' || expected == ch => {
                text.next();
                pattern.next();
            }
            (None, None) => return true,
            _ => {
                let Some((resume_text, resume_pattern)) = restart.as_mut() else { return false; };
                if resume_text.next().is_none() { return false; }
                text = resume_text.clone();
                pattern = resume_pattern.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_replaces_snapshot_and_live_changes_are_searchable() {
        let root = std::env::current_dir().unwrap().join("target")
            .join(format!("search-index-test-{}", std::process::id()));
        let files = root.join("files");
        fs::create_dir_all(&files).unwrap();
        let index_path = root.join("index.txt");
        let original = files.join("Música inicial.txt");
        fs::write(&original, "").unwrap();
        rebuild(&index_path, std::slice::from_ref(&files)).unwrap();
        let index = SearchIndex {
            state: Arc::new(Mutex::new(State { live: true, ..State::default() })),
            watcher: Mutex::new(None),
            path: Some(index_path.clone()),
        };
        let generation = AtomicU64::new(1);
        assert_eq!(index.find("musica", true, &generation, 1), vec![original.clone()]);
        let renamed = files.join("Música nueva.txt");
        fs::rename(&original, &renamed).unwrap();
        SearchIndex::record_changes(&index.state, None,
            Ok(Event::new(EventKind::Modify(ModifyKind::Name(notify::event::RenameMode::Both)))
                .add_path(original.clone()).add_path(renamed.clone())));
        assert_eq!(index.find("musica", true, &generation, 1), vec![renamed.clone()]);
        let matching_folder = files.join("Música");
        fs::create_dir_all(&matching_folder).unwrap();
        rebuild(&index_path, std::slice::from_ref(&files)).unwrap();
        assert_eq!(index.find_folders("musica", true, &generation, 1), vec![matching_folder.clone()]);
        let contents = fs::read_to_string(&index_path).unwrap();
        assert!(!contents.contains("inicial"));
        assert!(contents.contains("nueva"));

        fs::remove_file(renamed).unwrap();
        fs::remove_file(index_path).unwrap();
        fs::remove_dir(matching_folder).unwrap();
        fs::remove_dir(files).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn indexed_query_supports_multiple_terms_paths_wildcards_and_accents() {
        let path = Path::new(r"D:\Trabajo\Álbum\Música 2026.mp3");
        assert!(!SearchTerms::new(r"musica 20?6.* trabajo\album", true).matches(path));
        assert!(SearchTerms::new(r"musica *20?6.* trabajo\album", true).matches(path));
        assert!(!SearchTerms::new("musica *.mp3", false).matches(path));
        assert!(!SearchTerms::new("música *.png", false).matches(path));
        assert!(SearchTerms::new("música *.mp3", false).matches(path));
    }
}
