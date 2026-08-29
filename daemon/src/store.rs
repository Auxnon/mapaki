use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Link,
    File,
}

pub fn kind_for_name(name: &str) -> Kind {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "link" => Kind::Link,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif" => Kind::Image,
        _ => Kind::File,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: Uuid,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub added_at: i64,
    pub position: i64,
}

pub struct Store {
    dir: PathBuf,
    index_path: PathBuf,
    entries: std::sync::Mutex<Vec<Entry>>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Only a bare filename, no separators or traversal. Returns an error message on rejection.
fn sanitize_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err("invalid file name");
    }
    if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return Err("invalid file name");
    }
    Ok(name.to_string())
}

fn unique_name(dir: &Path, wanted: &str) -> String {
    let path = Path::new(wanted);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = path.extension().and_then(|e| e.to_str());
    let mut candidate = wanted.to_string();
    let mut n = 2;
    while dir.join(&candidate).exists() {
        candidate = match ext {
            Some(ext) => format!("{stem}-{n}.{ext}"),
            None => format!("{stem}-{n}"),
        };
        n += 1;
    }
    candidate
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value).unwrap())?;
    fs::rename(&tmp, path)
}

impl Store {
    pub fn load(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        let index_path = dir.join(".mapaki-index.json");

        let mut entries: Vec<Entry> = match fs::read(&index_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        // Reconcile with the actual directory: drop entries whose file is gone,
        // and pick up files that were dropped in by hand (appended to the end).
        entries.retain(|e| dir.join(&e.name).is_file());
        let known: std::collections::HashSet<String> =
            entries.iter().map(|e| e.name.clone()).collect();
        let mut next_position = entries.iter().map(|e| e.position).max().unwrap_or(-1) + 1;
        if let Ok(read_dir) = fs::read_dir(&dir) {
            for e in read_dir.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || known.contains(name.as_str()) {
                    continue;
                }
                if !e.path().is_file() {
                    continue;
                }
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                entries.push(Entry {
                    id: Uuid::new_v4(),
                    kind: kind_for_name(&name),
                    name,
                    size,
                    added_at: now(),
                    position: next_position,
                });
                next_position += 1;
            }
        }

        let store = Store {
            dir,
            index_path,
            entries: std::sync::Mutex::new(entries),
        };
        store.save()?;
        Ok(store)
    }

    fn save(&self) -> io::Result<()> {
        let entries = self.entries.lock().unwrap();
        write_json_atomic(&self.index_path, &*entries)
    }

    pub fn list(&self) -> Vec<Entry> {
        let mut entries = self.entries.lock().unwrap().clone();
        entries.sort_by_key(|e| e.position);
        entries
    }

    pub fn content_path(&self, id: Uuid) -> Option<(PathBuf, String)> {
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| (self.dir.join(&e.name), e.name.clone()))
    }

    pub fn insert_new(&self, wanted_name: &str, bytes: &[u8]) -> Result<Entry, &'static str> {
        let wanted_name = sanitize_name(wanted_name)?;
        let name = unique_name(&self.dir, &wanted_name);
        fs::write(self.dir.join(&name), bytes).map_err(|_| "failed to write file")?;

        let mut entries = self.entries.lock().unwrap();
        for e in entries.iter_mut() {
            e.position += 1;
        }
        let entry = Entry {
            id: Uuid::new_v4(),
            kind: kind_for_name(&name),
            name,
            size: bytes.len() as u64,
            added_at: now(),
            position: 0,
        };
        entries.push(entry.clone());
        drop(entries);
        let _ = self.save();
        Ok(entry)
    }

    pub fn rename(&self, id: Uuid, new_name: &str) -> Result<Entry, &'static str> {
        let new_name = sanitize_name(new_name)?;
        let mut entries = self.entries.lock().unwrap();
        let idx = entries
            .iter()
            .position(|e| e.id == id)
            .ok_or("not found")?;
        let old_name = entries[idx].name.clone();
        if new_name != old_name {
            let final_name = unique_name(&self.dir, &new_name);
            fs::rename(self.dir.join(&old_name), self.dir.join(&final_name))
                .map_err(|_| "rename failed")?;
            entries[idx].name = final_name;
            entries[idx].kind = kind_for_name(&entries[idx].name);
        }
        let entry = entries[idx].clone();
        drop(entries);
        let _ = self.save();
        Ok(entry)
    }

    pub fn delete(&self, id: Uuid) -> Result<(), &'static str> {
        let mut entries = self.entries.lock().unwrap();
        let idx = entries.iter().position(|e| e.id == id).ok_or("not found")?;
        let removed = entries.remove(idx);
        let _ = fs::remove_file(self.dir.join(&removed.name));
        drop(entries);
        let _ = self.save();
        Ok(())
    }

    /// Rewrites positions 0..n from the given id order. Any known ids missing
    /// from `ids` are kept, appended after, in their previous relative order.
    pub fn set_order(&self, ids: &[Uuid]) -> Vec<Entry> {
        let mut entries = self.entries.lock().unwrap();
        entries.sort_by_key(|e| e.position);
        let mut ordered: Vec<Entry> = Vec::with_capacity(entries.len());
        for id in ids {
            if let Some(pos) = entries.iter().position(|e| e.id == *id) {
                ordered.push(entries.remove(pos));
            }
        }
        ordered.extend(entries.drain(..));
        for (i, e) in ordered.iter_mut().enumerate() {
            e.position = i as i64;
        }
        *entries = ordered.clone();
        drop(entries);
        let _ = self.save();
        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Store, tempdir::TempDir) {
        let dir = tempdir::TempDir::new("mapaki-test").unwrap();
        let store = Store::load(dir.path().to_path_buf()).unwrap();
        (store, dir)
    }

    #[test]
    fn new_files_pile_up_at_the_top() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("a.link", b"a").unwrap();
        let b = store.insert_new("b.link", b"b").unwrap();
        let list = store.list();
        assert_eq!(list[0].id, b.id);
        assert_eq!(list[1].id, a.id);
        assert_eq!(list[0].position, 0);
        assert_eq!(list[1].position, 1);
    }

    #[test]
    fn kind_is_inferred_from_extension() {
        let (store, _dir) = temp_store();
        let img = store.insert_new("shot.png", b"x").unwrap();
        let link = store.insert_new("site.link", b"x").unwrap();
        let other = store.insert_new("notes.txt", b"x").unwrap();
        assert_eq!(img.kind, Kind::Image);
        assert_eq!(link.kind, Kind::Link);
        assert_eq!(other.kind, Kind::File);
    }

    #[test]
    fn duplicate_names_are_deduped() {
        let (store, _dir) = temp_store();
        let first = store.insert_new("dup.link", b"1").unwrap();
        let second = store.insert_new("dup.link", b"2").unwrap();
        assert_eq!(first.name, "dup.link");
        assert_eq!(second.name, "dup-2.link");
    }

    #[test]
    fn set_order_rewrites_positions_and_keeps_missing_ids() {
        let (store, _dir) = temp_store();
        let a = store.insert_new("a.link", b"a").unwrap();
        let b = store.insert_new("b.link", b"b").unwrap();
        let c = store.insert_new("c.link", b"c").unwrap();
        // Only reorder a subset; c (unmentioned) should land at the end.
        let result = store.set_order(&[a.id, b.id]);
        assert_eq!(result.iter().map(|e| e.id).collect::<Vec<_>>(), vec![a.id, b.id, c.id]);
        assert_eq!(result[0].position, 0);
        assert_eq!(result[1].position, 1);
        assert_eq!(result[2].position, 2);
    }

    #[test]
    fn delete_removes_entry_and_file() {
        let (store, dir) = temp_store();
        let a = store.insert_new("a.link", b"a").unwrap();
        store.delete(a.id).unwrap();
        assert!(store.list().is_empty());
        assert!(!dir.path().join("a.link").exists());
    }

    #[test]
    fn path_traversal_names_are_rejected() {
        let (store, _dir) = temp_store();
        assert!(store.insert_new("../evil", b"x").is_err());
        assert!(store.insert_new(".hidden", b"x").is_err());
    }
}
