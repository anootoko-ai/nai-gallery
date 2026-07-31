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

/// Read image dimensions from the header without decoding pixel data.
/// Sniffs the real format, so it works for misnamed files (e.g. a JPEG
/// saved as .png) that fail the PNG signature check in `nai::parse`.
fn probe_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Generate a thumbnail, overwriting any stale one. Writes to a temp file
/// and renames so a crash mid-write can't leave a truncated JPEG that would
/// be mistaken for a valid cached thumbnail. Returns true on success.
fn generate_thumbnail(data: &[u8], thumb_path: &Path) -> bool {
    let Ok(img) = image::load_from_memory(data) else {
        return false;
    };
    let thumb = img.thumbnail(THUMB_MAX_DIM, THUMB_MAX_DIM).into_rgb8();
    let tmp = thumb_path.with_extension("jpg.tmp");
    if thumb.save_with_format(&tmp, image::ImageFormat::Jpeg).is_err() {
        std::fs::remove_file(&tmp).ok();
        return false;
    }
    std::fs::rename(&tmp, thumb_path).is_ok()
}

/// Scan one folder root. Skips files already indexed with unchanged
/// size+mtime. Emits `scan:progress` events; returns files (re)indexed.
pub fn scan_folder(app: &AppHandle, folder_id: i64, root: &str) -> Result<usize, String> {
    let db = app.state::<Db>();
    let thumbs = thumbs_dir(app);

    // snapshot of already-indexed files -> (size, mtime, width)
    let known: std::collections::HashMap<String, (i64, i64, i64)> = {
        let conn = db.0.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT path, file_size, file_mtime, width FROM images WHERE folder_id = ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([folder_id], |r| {
                Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
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
        // width = 0 means an earlier scan failed to read dimensions;
        // re-index those files so they get healed
        let unchanged = known
            .get(&path)
            .is_some_and(|&(s, m, w)| (s, m) == (size, mtime) && w > 0);
        if !unchanged {
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
            let mut meta = nai::parse(&data);
            // nai::parse only understands real PNGs; fall back to a format-
            // sniffing header probe so misnamed JPEGs/WebPs still get real
            // dimensions instead of 0x0 (which renders as a square tile)
            if meta.as_ref().is_none_or(|m| m.width == 0 || m.height == 0) {
                if let Some((w, h)) = probe_dimensions(&data) {
                    let m = meta.get_or_insert_with(Default::default);
                    m.width = w;
                    m.height = h;
                }
            }
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
    // regenerate unconditionally: a cached thumb for a (re)scanned file is
    // stale by definition — ids can be reused after folder removal, so the
    // file on disk may belong to a different image entirely
    ids.par_iter().for_each(|(id, i)| {
        let thumb_path = thumbs.join(format!("{id}.jpg"));
        generate_thumbnail(&parsed[*i].1, &thumb_path);
    });

    // repair pass: images indexed but missing their thumbnail, e.g. when a
    // previous scan crashed after the DB commit but before thumbnails finished
    let missing: Vec<(i64, String)> = {
        let conn = db.0.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, path FROM images WHERE folder_id = ?1")
            .map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([folder_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter(|(id, _)| !thumbs.join(format!("{id}.jpg")).exists())
            .collect();
        rows
    };
    missing.par_iter().for_each(|(id, path)| {
        if let Ok(data) = std::fs::read(path) {
            generate_thumbnail(&data, &thumbs.join(format!("{id}.jpg")));
        }
    });

    // phash pass: hash anything still lacking one, from its cached
    // thumbnail (dHash downsamples to 9x8, so thumbnail resolution is
    // plenty and the small JPEGs decode far faster than originals).
    // Covers fresh inserts, content-changed files (the upsert resets
    // phash to NULL), and libraries indexed before hashing existed.
    // Failures stay NULL and are retried on the next scan.
    let unhashed: Vec<i64> = {
        let conn = db.0.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id FROM images WHERE folder_id = ?1 AND phash IS NULL")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([folder_id], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        rows
    };
    let hash_total = unhashed.len();
    let hash_counter = AtomicUsize::new(0);
    let hashes: Vec<(i64, i64)> = unhashed
        .par_iter()
        .filter_map(|id| {
            let data = std::fs::read(thumbs.join(format!("{id}.jpg"))).ok()?;
            let img = image::load_from_memory(&data).ok()?;
            let done = hash_counter.fetch_add(1, Ordering::Relaxed) + 1;
            if done % 100 == 0 || done == hash_total {
                app.emit(
                    "hash:progress",
                    ScanProgress {
                        done,
                        total: hash_total,
                        folder: folder_name.clone(),
                    },
                )
                .ok();
            }
            Some((*id, crate::hash::dhash(&img)))
        })
        .collect();
    {
        let mut conn = db.0.lock().unwrap();
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for (id, hash) in &hashes {
            crate::db::set_phash(&tx, *id, *hash).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
    }

    app.emit("scan:done", total).ok();
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(w: u32, h: u32, format: image::ImageFormat) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(w, h));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, format).unwrap();
        buf.into_inner()
    }

    #[test]
    fn probe_reads_dims_of_misnamed_formats() {
        // a JPEG or WebP saved with a .png extension fails nai::parse's
        // signature check but must still yield real dimensions
        let jpg = encode(30, 60, image::ImageFormat::Jpeg);
        assert!(nai::parse(&jpg).is_none());
        assert_eq!(probe_dimensions(&jpg), Some((30, 60)));

        let webp = encode(40, 20, image::ImageFormat::WebP);
        assert!(nai::parse(&webp).is_none());
        assert_eq!(probe_dimensions(&webp), Some((40, 20)));
    }

    #[test]
    fn thumbnail_overwrites_corrupt_leftover() {
        let dir = std::env::temp_dir().join("nai-gallery-test-thumbs");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1.jpg");
        // simulate a truncated thumb left behind by a crash mid-write
        std::fs::write(&path, b"not a jpeg").unwrap();

        let src = encode(100, 200, image::ImageFormat::Png);
        assert!(generate_thumbnail(&src, &path));
        let thumb = image::load_from_memory(&std::fs::read(&path).unwrap()).unwrap();
        // valid jpeg, 1:2 aspect ratio preserved within the 512 bound, no stale tmp file
        assert_eq!(thumb.height(), thumb.width() * 2);
        assert!(thumb.width().max(thumb.height()) <= THUMB_MAX_DIM);
        assert!(!path.with_extension("jpg.tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn thumbnail_decodes_webp_sources() {
        let dir = std::env::temp_dir().join("nai-gallery-test-thumbs-webp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2.jpg");
        let src = encode(64, 32, image::ImageFormat::WebP);
        assert!(generate_thumbnail(&src, &path));
        std::fs::remove_dir_all(&dir).ok();
    }
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
           raw_prompt=?13, raw_negative=?14, comment_json=?15,
           phash=NULL",
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
    // refresh metadata-derived tags only — user tags survive rescans
    conn.execute(
        "DELETE FROM image_tags WHERE image_id = ?1 AND source != 'user'",
        [image_id],
    )?;
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
