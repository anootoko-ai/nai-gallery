//! Folder scanning: walk directories, parse NovelAI metadata, generate
//! thumbnails, and upsert into the index. Parse/thumbnail work runs on
//! rayon; a single thread owns the DB writes.

use crate::{db::Db, nai};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tauri::{AppHandle, Emitter, Manager};
use walkdir::WalkDir;

pub const THUMB_MAX_DIM: u32 = 512;

#[derive(Clone, Serialize)]
pub struct ScanProgress {
    pub done: usize,
    pub total: usize,
    pub folder: String,
}

pub fn thumbs_dir(app: &AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir")
        .join("thumbs");
    std::fs::create_dir_all(&dir).ok();
    dir
}

struct ParsedFile {
    path: String,
    size: i64,
    mtime: i64,
    meta: Option<nai::NaiMetadata>,
}

fn file_mtime_secs(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Generate a thumbnail if missing. Returns true on success.
fn ensure_thumbnail(data: &[u8], thumb_path: &Path) -> bool {
    if thumb_path.exists() {
        return true;
    }
    let Ok(img) = image::load_from_memory(data) else {
        return false;
    };
    let thumb = img.thumbnail(THUMB_MAX_DIM, THUMB_MAX_DIM).into_rgb8();
    thumb
        .save_with_format(thumb_path, image::ImageFormat::Jpeg)
        .is_ok()
}

/// Scan one folder root. Skips files already indexed with unchanged
/// size+mtime. Emits `scan:progress` events; returns files (re)indexed.
pub fn scan_folder(app: &AppHandle, folder_id: i64, root: &str) -> Result<usize, String> {
    let db = app.state::<Db>();
    let thumbs = thumbs_dir(app);

    // snapshot of already-indexed files -> (size, mtime)
    let known: std::collections::HashMap<String, (i64, i64)> = {
        let conn = db.0.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT path, file_size, file_mtime FROM images WHERE folder_id = ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([folder_id], |r| {
                Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?)))
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        rows
    };

    let mut pending: Vec<(String, i64, i64)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let is_png = entry
            .path()
            .extension()
            .map(|e| e.eq_ignore_ascii_case("png"))
            .unwrap_or(false);
        if !is_png {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        let path = entry.path().to_string_lossy().into_owned();
        let (size, mtime) = (md.len() as i64, file_mtime_secs(&md));
        seen.insert(path.clone());
        if known.get(&path) != Some(&(size, mtime)) {
            pending.push((path, size, mtime));
        }
    }

    // remove records for files deleted on disk
    {
        let conn = db.0.lock().unwrap();
        for (path, _) in known.iter().filter(|(p, _)| !seen.contains(*p)) {
            conn.execute("DELETE FROM images WHERE path = ?1", [path]).ok();
        }
    }

    let total = pending.len();
    let counter = AtomicUsize::new(0);
    let folder_name = root.to_string();

    // parse + thumbnail in parallel, then hand results to the writer below
    let parsed: Vec<(ParsedFile, Vec<u8>)> = pending
        .par_iter()
        .filter_map(|(path, size, mtime)| {
            let data = std::fs::read(path).ok()?;
            let meta = nai::parse(&data);
            let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
            if done % 25 == 0 || done == total {
                app.emit(
                    "scan:progress",
                    ScanProgress {
                        done,
                        total,
                        folder: folder_name.clone(),
                    },
                )
                .ok();
            }
            Some((
                ParsedFile {
                    path: path.clone(),
                    size: *size,
                    mtime: *mtime,
                    meta,
                },
                data,
            ))
        })
        .collect();

    // single-writer insert pass
    {
        let mut conn = db.0.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for (file, _) in &parsed {
            insert_image(&tx, folder_id, file).map_err(|e| e.to_string())?;
        }
        tx.execute(
            "UPDATE folders SET last_scanned = strftime('%s','now') WHERE id = ?1",
            [folder_id],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    }

    // thumbnails (needs image ids, so after insert)
    let ids: Vec<(i64, usize)> = {
        let conn = db.0.lock().unwrap();
        parsed
            .iter()
            .enumerate()
            .filter_map(|(i, (f, _))| {
                crate::db::get_id_by_path(&conn, &f.path).ok().map(|id| (id, i))
            })
            .collect()
    };
    ids.par_iter().for_each(|(id, i)| {
        let thumb_path = thumbs.join(format!("{id}.jpg"));
        ensure_thumbnail(&parsed[*i].1, &thumb_path);
    });

    app.emit("scan:done", total).ok();
    Ok(total)
}

fn insert_image(conn: &rusqlite::Connection, folder_id: i64, f: &ParsedFile) -> rusqlite::Result<()> {
    let m = f.meta.as_ref();
    conn.execute(
        "INSERT INTO images(path, folder_id, file_size, file_mtime, width, height,
                            is_novelai, model, seed, sampler, steps, scale,
                            raw_prompt, raw_negative, comment_json)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
         ON CONFLICT(path) DO UPDATE SET
           file_size=?3, file_mtime=?4, width=?5, height=?6, is_novelai=?7,
           model=?8, seed=?9, sampler=?10, steps=?11, scale=?12,
           raw_prompt=?13, raw_negative=?14, comment_json=?15",
        rusqlite::params![
            f.path,
            folder_id,
            f.size,
            f.mtime,
            m.map(|m| m.width).unwrap_or(0),
            m.map(|m| m.height).unwrap_or(0),
            m.map(|m| m.is_novelai as i64).unwrap_or(0),
            m.and_then(|m| m.model.clone()),
            m.and_then(|m| m.seed),
            m.and_then(|m| m.sampler.clone()),
            m.and_then(|m| m.steps),
            m.and_then(|m| m.scale),
            m.and_then(|m| m.raw_prompt.clone()),
            m.and_then(|m| m.raw_negative.clone()),
            m.and_then(|m| m.comment_json.clone()),
        ],
    )?;
    let image_id: i64 = conn.query_row("SELECT id FROM images WHERE path = ?1", [&f.path], |r| r.get(0))?;
    conn.execute("DELETE FROM image_tags WHERE image_id = ?1", [image_id])?;
    if let Some(m) = m {
        for tag in &m.tags {
            conn.execute(
                "INSERT OR IGNORE INTO tags(name, category) VALUES (?1, ?2)",
                rusqlite::params![tag.name, tag.category],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO image_tags(image_id, tag_id, source)
                 SELECT ?1, id, ?3 FROM tags WHERE name = ?2",
                rusqlite::params![image_id, tag.name, tag.source],
            )?;
        }
    }
    Ok(())
}
