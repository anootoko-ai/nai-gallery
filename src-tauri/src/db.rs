//! SQLite index. One connection behind a mutex — plenty at this scale;
//! the scanner batches writes in transactions.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

pub struct Db(pub Mutex<Connection>);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders(
  id INTEGER PRIMARY KEY,
  path TEXT UNIQUE NOT NULL,
  last_scanned INTEGER
);
CREATE TABLE IF NOT EXISTS images(
  id INTEGER PRIMARY KEY,
  path TEXT UNIQUE NOT NULL,
  folder_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
  file_size INTEGER NOT NULL,
  file_mtime INTEGER NOT NULL,
  width INTEGER NOT NULL DEFAULT 0,
  height INTEGER NOT NULL DEFAULT 0,
  is_novelai INTEGER NOT NULL DEFAULT 0,
  model TEXT, seed INTEGER, sampler TEXT, steps INTEGER, scale REAL,
  raw_prompt TEXT, raw_negative TEXT, comment_json TEXT,
  rating INTEGER NOT NULL DEFAULT 0,
  favorite INTEGER NOT NULL DEFAULT 0,
  hidden INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_images_mtime ON images(file_mtime);
CREATE INDEX IF NOT EXISTS idx_images_folder ON images(folder_id);
CREATE TABLE IF NOT EXISTS tags(
  id INTEGER PRIMARY KEY,
  name TEXT UNIQUE NOT NULL,
  category TEXT NOT NULL DEFAULT 'general'
);
CREATE TABLE IF NOT EXISTS image_tags(
  image_id INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  tag_id INTEGER NOT NULL REFERENCES tags(id),
  source TEXT NOT NULL DEFAULT 'base',
  PRIMARY KEY(image_id, tag_id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_image_tags_tag ON image_tags(tag_id);
CREATE TABLE IF NOT EXISTS albums(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  position INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS album_images(
  album_id INTEGER NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
  image_id INTEGER NOT NULL REFERENCES images(id) ON DELETE CASCADE,
  position INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(album_id, image_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS saved_searches(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  query_json TEXT NOT NULL
);
";

/// SCHEMA above is the frozen v0 baseline; all later changes are appended
/// here and applied in order via PRAGMA user_version. Fresh databases get
/// the baseline and then run every migration, so both paths converge.
const MIGRATIONS: &[&str] = &[
    // 1: per-user tag hiding (phase 2.5)
    "ALTER TABLE tags ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;",
    // 2: perceptual hash for near-duplicate detection (phase 3);
    // NULL = not yet computed, backfilled from thumbnails during scans
    "ALTER TABLE images ADD COLUMN phash INTEGER;",
];

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        conn.execute_batch(migration)?;
        conn.pragma_update(None, "user_version", (i + 1) as i64)?;
    }
    Ok(())
}

pub fn open(app_data_dir: &Path) -> rusqlite::Result<Db> {
    std::fs::create_dir_all(app_data_dir).ok();
    let conn = Connection::open(app_data_dir.join("library.sqlite"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(Db(Mutex::new(conn)))
}

// ── query types ──────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct Query {
    #[serde(default)]
    pub include_tags: Vec<String>,
    #[serde(default)]
    pub exclude_tags: Vec<String>,
    pub text: Option<String>,
    pub favorite: Option<bool>,
    pub min_rating: Option<i64>,
    pub folder_id: Option<i64>,
    /// Absolute directory prefix INCLUDING trailing separator, e.g.
    /// `D:\gallery\2026-07\` — scopes to a subfolder of a watched root.
    pub path_prefix: Option<String>,
    pub album_id: Option<i64>,
    /// Some(true) = only images in at least one album;
    /// Some(false) = only images in no album at all
    pub in_album: Option<bool>,
    /// false (default) = hide rejects; true = show ONLY rejects
    #[serde(default)]
    pub rejects: bool,
    #[serde(default)]
    pub sort: String, // "newest" | "oldest" | "rating"
    #[serde(default)]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}
fn default_limit() -> i64 {
    200
}

#[derive(Debug, Serialize)]
pub struct ImageCard {
    pub id: i64,
    pub width: u32,
    pub height: u32,
    pub seed: Option<i64>,
    pub rating: i64,
    pub favorite: bool,
}

#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub total: i64,
    pub cards: Vec<ImageCard>,
}

#[derive(Debug, Serialize)]
pub struct TagSuggestion {
    pub name: String,
    pub category: String,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct TagDetail {
    pub name: String,
    pub category: String,
    pub source: String,
    pub hidden: bool,
}

#[derive(Debug, Serialize)]
pub struct ImageDetail {
    pub id: i64,
    pub path: String,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    pub file_mtime: i64,
    pub is_novelai: bool,
    pub model: Option<String>,
    pub seed: Option<i64>,
    pub sampler: Option<String>,
    pub steps: Option<i64>,
    pub scale: Option<f64>,
    pub raw_prompt: Option<String>,
    pub raw_negative: Option<String>,
    pub rating: i64,
    pub favorite: bool,
    pub tags: Vec<TagDetail>,
}

#[derive(Debug, Serialize)]
pub struct FolderInfo {
    pub id: i64,
    pub path: String,
    pub image_count: i64,
}

#[derive(Debug, Serialize)]
pub struct LibraryStats {
    pub total: i64,
    pub novelai: i64,
    pub favorites: i64,
    pub rejects: i64,
}

#[derive(Debug, Serialize)]
pub struct AlbumInfo {
    pub id: i64,
    pub name: String,
    pub image_count: i64,
}

#[derive(Debug, Serialize)]
pub struct SavedSearch {
    pub id: i64,
    pub name: String,
    pub query_json: String,
}

// ── queries ──────────────────────────────────────────────────

/// Build WHERE clause + params for a Query. Tag filters use
/// group-by-count intersection (danbooru AND semantics).
fn build_where(q: &Query, params_out: &mut Vec<Box<dyn rusqlite::ToSql>>) -> String {
    let mut clauses = vec![if q.rejects {
        "images.hidden = 1".to_string()
    } else {
        "images.hidden = 0".to_string()
    }];
    if let Some(a) = q.album_id {
        clauses.push(format!(
            "images.id IN (SELECT image_id FROM album_images WHERE album_id = {a})"
        ));
    }
    match q.in_album {
        Some(true) => clauses.push("images.id IN (SELECT image_id FROM album_images)".into()),
        Some(false) => clauses.push("images.id NOT IN (SELECT image_id FROM album_images)".into()),
        None => {}
    }

    if !q.include_tags.is_empty() {
        let ph = q.include_tags.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        clauses.push(format!(
            "images.id IN (SELECT it.image_id FROM image_tags it
              JOIN tags t ON t.id = it.tag_id WHERE t.name IN ({ph})
              GROUP BY it.image_id HAVING COUNT(DISTINCT t.id) = {n})",
            n = q.include_tags.len()
        ));
        for t in &q.include_tags {
            params_out.push(Box::new(t.clone()));
        }
    }
    if !q.exclude_tags.is_empty() {
        let ph = q.exclude_tags.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        clauses.push(format!(
            "images.id NOT IN (SELECT it.image_id FROM image_tags it
              JOIN tags t ON t.id = it.tag_id WHERE t.name IN ({ph}))"
        ));
        for t in &q.exclude_tags {
            params_out.push(Box::new(t.clone()));
        }
    }
    if let Some(text) = q.text.as_ref().filter(|t| !t.is_empty()) {
        clauses.push("(images.raw_prompt LIKE ? OR images.raw_negative LIKE ?)".into());
        let like = format!("%{text}%");
        params_out.push(Box::new(like.clone()));
        params_out.push(Box::new(like));
    }
    if q.favorite == Some(true) {
        clauses.push("images.favorite = 1".into());
    }
    if let Some(r) = q.min_rating {
        clauses.push(format!("images.rating >= {r}"));
    }
    if let Some(f) = q.folder_id {
        clauses.push(format!("images.folder_id = {f}"));
    }
    // exact prefix compare instead of LIKE: Windows paths routinely contain
    // `_`, which LIKE would treat as a wildcard
    if let Some(prefix) = q.path_prefix.as_ref().filter(|p| !p.is_empty()) {
        clauses.push("substr(images.path, 1, length(?)) = ?".into());
        params_out.push(Box::new(prefix.clone()));
        params_out.push(Box::new(prefix.clone()));
    }
    clauses.join(" AND ")
}

pub fn query_images(conn: &Connection, q: &Query) -> rusqlite::Result<QueryResult> {
    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let where_sql = build_where(q, &mut params_vec);
    let order = match q.sort.as_str() {
        "oldest" => "images.file_mtime ASC, images.id ASC",
        "rating" => "images.rating DESC, images.file_mtime DESC",
        _ => "images.file_mtime DESC, images.id DESC",
    };

    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM images WHERE {where_sql}"),
        rusqlite::params_from_iter(params_vec.iter().map(|p| p.as_ref())),
        |r| r.get(0),
    )?;

    let sql = format!(
        "SELECT id, width, height, seed, rating, favorite FROM images
         WHERE {where_sql} ORDER BY {order} LIMIT {} OFFSET {}",
        q.limit, q.offset
    );
    let mut stmt = conn.prepare(&sql)?;
    let cards = stmt
        .query_map(
            rusqlite::params_from_iter(params_vec.iter().map(|p| p.as_ref())),
            |r| {
                Ok(ImageCard {
                    id: r.get(0)?,
                    width: r.get(1)?,
                    height: r.get(2)?,
                    seed: r.get(3)?,
                    rating: r.get(4)?,
                    favorite: r.get::<_, i64>(5)? != 0,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(QueryResult { total, cards })
}

pub fn suggest_tags(conn: &Connection, prefix: &str, limit: i64) -> rusqlite::Result<Vec<TagSuggestion>> {
    let mut stmt = conn.prepare(
        "SELECT t.name, t.category, COUNT(it.image_id) AS n
         FROM tags t JOIN image_tags it ON it.tag_id = t.id
         WHERE t.name LIKE ?1 AND t.hidden = 0
         GROUP BY t.id ORDER BY n DESC, t.name LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![format!("%{prefix}%"), limit], |r| {
            Ok(TagSuggestion {
                name: r.get(0)?,
                category: r.get(1)?,
                count: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn top_tags(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<TagSuggestion>> {
    suggest_tags(conn, "", limit)
}

pub fn get_image(conn: &Connection, id: i64) -> rusqlite::Result<ImageDetail> {
    let mut detail = conn.query_row(
        "SELECT id, path, width, height, file_mtime, is_novelai, model, seed, sampler,
                steps, scale, raw_prompt, raw_negative, rating, favorite
         FROM images WHERE id = ?1",
        params![id],
        |r| {
            let path: String = r.get(1)?;
            let file_name = Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(ImageDetail {
                id: r.get(0)?,
                file_name,
                path,
                width: r.get(2)?,
                height: r.get(3)?,
                file_mtime: r.get(4)?,
                is_novelai: r.get::<_, i64>(5)? != 0,
                model: r.get(6)?,
                seed: r.get(7)?,
                sampler: r.get(8)?,
                steps: r.get(9)?,
                scale: r.get(10)?,
                raw_prompt: r.get(11)?,
                raw_negative: r.get(12)?,
                rating: r.get(13)?,
                favorite: r.get::<_, i64>(14)? != 0,
                tags: Vec::new(),
            })
        },
    )?;
    // artists first, then general, then character tags
    let mut stmt = conn.prepare(
        "SELECT t.name, t.category, it.source, t.hidden FROM image_tags it
         JOIN tags t ON t.id = it.tag_id WHERE it.image_id = ?1
         ORDER BY CASE t.category WHEN 'artist' THEN 0 WHEN 'general' THEN 1 ELSE 2 END, t.name",
    )?;
    detail.tags = stmt
        .query_map(params![id], |r| {
            Ok(TagDetail {
                name: r.get(0)?,
                category: r.get(1)?,
                source: r.get(2)?,
                hidden: r.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(detail)
}

pub fn get_id_by_path(conn: &Connection, path: &str) -> rusqlite::Result<i64> {
    conn.query_row("SELECT id FROM images WHERE path = ?1", params![path], |r| r.get(0))
}

pub fn get_image_path(conn: &Connection, id: i64) -> rusqlite::Result<String> {
    conn.query_row("SELECT path FROM images WHERE id = ?1", params![id], |r| r.get(0))
}

pub fn set_rating(conn: &Connection, id: i64, rating: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE images SET rating = ?2 WHERE id = ?1", params![id, rating.clamp(0, 5)])?;
    Ok(())
}

pub fn set_favorite(conn: &Connection, id: i64, favorite: bool) -> rusqlite::Result<()> {
    conn.execute("UPDATE images SET favorite = ?2 WHERE id = ?1", params![id, favorite as i64])?;
    Ok(())
}

pub fn list_folders(conn: &Connection) -> rusqlite::Result<Vec<FolderInfo>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, COUNT(i.id) FROM folders f
         LEFT JOIN images i ON i.folder_id = f.id GROUP BY f.id ORDER BY f.path",
    )?;
    let folders = stmt
        .query_map([], |r| {
            Ok(FolderInfo {
                id: r.get(0)?,
                path: r.get(1)?,
                image_count: r.get(2)?,
            })
        })?
        .collect();
    folders
}

pub fn stats(conn: &Connection) -> rusqlite::Result<LibraryStats> {
    conn.query_row(
        "SELECT COALESCE(SUM(hidden = 0),0), COALESCE(SUM(is_novelai AND hidden = 0),0),
                COALESCE(SUM(favorite AND hidden = 0),0), COALESCE(SUM(hidden),0)
         FROM images",
        [],
        |r| {
            Ok(LibraryStats {
                total: r.get(0)?,
                novelai: r.get(1)?,
                favorites: r.get(2)?,
                rejects: r.get(3)?,
            })
        },
    )
}

// ── phase 2: bulk ops, albums, saved searches ───────────────

pub fn set_hidden_bulk(conn: &Connection, ids: &[i64], hidden: bool) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("UPDATE images SET hidden = ?2 WHERE id = ?1")?;
    for id in ids {
        stmt.execute(params![id, hidden as i64])?;
    }
    Ok(())
}

pub fn set_favorite_bulk(conn: &Connection, ids: &[i64], favorite: bool) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("UPDATE images SET favorite = ?2 WHERE id = ?1")?;
    for id in ids {
        stmt.execute(params![id, favorite as i64])?;
    }
    Ok(())
}

pub fn set_rating_bulk(conn: &Connection, ids: &[i64], rating: i64) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("UPDATE images SET rating = ?2 WHERE id = ?1")?;
    for id in ids {
        stmt.execute(params![id, rating.clamp(0, 5)])?;
    }
    Ok(())
}

pub fn list_albums(conn: &Connection) -> rusqlite::Result<Vec<AlbumInfo>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.name, COUNT(ai.image_id) FROM albums a
         LEFT JOIN album_images ai ON ai.album_id = a.id
         GROUP BY a.id ORDER BY a.position, a.name",
    )?;
    let albums = stmt
        .query_map([], |r| {
            Ok(AlbumInfo {
                id: r.get(0)?,
                name: r.get(1)?,
                image_count: r.get(2)?,
            })
        })?
        .collect();
    albums
}

pub fn create_album(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    conn.execute("INSERT INTO albums(name) VALUES (?1)", params![name])?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_album(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM albums WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn add_to_album(conn: &Connection, album_id: i64, ids: &[i64]) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO album_images(album_id, image_id, position)
         VALUES (?1, ?2, (SELECT COALESCE(MAX(position),0)+1 FROM album_images WHERE album_id = ?1))",
    )?;
    for id in ids {
        stmt.execute(params![album_id, id])?;
    }
    Ok(())
}

pub fn remove_from_album(conn: &Connection, album_id: i64, ids: &[i64]) -> rusqlite::Result<()> {
    let mut stmt =
        conn.prepare("DELETE FROM album_images WHERE album_id = ?1 AND image_id = ?2")?;
    for id in ids {
        stmt.execute(params![album_id, id])?;
    }
    Ok(())
}

pub fn list_saved_searches(conn: &Connection) -> rusqlite::Result<Vec<SavedSearch>> {
    let mut stmt = conn.prepare("SELECT id, name, query_json FROM saved_searches ORDER BY name")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(SavedSearch {
                id: r.get(0)?,
                name: r.get(1)?,
                query_json: r.get(2)?,
            })
        })?
        .collect();
    rows
}

pub fn create_saved_search(conn: &Connection, name: &str, query_json: &str) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO saved_searches(name, query_json) VALUES (?1, ?2)",
        params![name, query_json],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_saved_search(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM saved_searches WHERE id = ?1", params![id])?;
    Ok(())
}

// ── phase 2.5: user tags, tag hiding, folder tree ────────────

/// Attach a tag to images with source='user'. The name must already be
/// normalized (lowercased, whitespace collapsed) so it merges with
/// prompt-derived tags. If the tag already exists on an image with a
/// metadata source, that row is left untouched.
pub fn add_user_tag(conn: &Connection, ids: &[i64], name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO tags(name, category) VALUES (?1, 'general')",
        params![name],
    )?;
    let tag_id: i64 =
        conn.query_row("SELECT id FROM tags WHERE name = ?1", params![name], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO image_tags(image_id, tag_id, source) VALUES (?1, ?2, 'user')",
    )?;
    for id in ids {
        stmt.execute(params![id, tag_id])?;
    }
    Ok(())
}

/// Only rows with source='user' are removable — prompt-derived tags would
/// reappear on the next rescan anyway.
pub fn remove_user_tag(conn: &Connection, ids: &[i64], name: &str) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "DELETE FROM image_tags WHERE image_id = ?1 AND source = 'user'
         AND tag_id = (SELECT id FROM tags WHERE name = ?2)",
    )?;
    for id in ids {
        stmt.execute(params![id, name])?;
    }
    Ok(())
}

pub fn set_tag_hidden(conn: &Connection, name: &str, hidden: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE tags SET hidden = ?2 WHERE name = ?1",
        params![name, hidden as i64],
    )?;
    Ok(())
}

pub fn list_hidden_tags(conn: &Connection) -> rusqlite::Result<Vec<TagSuggestion>> {
    let mut stmt = conn.prepare(
        "SELECT t.name, t.category, COUNT(it.image_id) FROM tags t
         LEFT JOIN image_tags it ON it.tag_id = t.id
         WHERE t.hidden = 1 GROUP BY t.id ORDER BY t.name",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(TagSuggestion {
                name: r.get(0)?,
                category: r.get(1)?,
                count: r.get(2)?,
            })
        })?
        .collect();
    rows
}

#[derive(Debug, Serialize)]
pub struct DirEntry {
    pub folder_id: i64,
    /// Directory relative to the watched root, '' for the root itself.
    /// Uses the OS separator as stored in images.path.
    pub rel_dir: String,
    /// Images directly in this directory (not descendants).
    pub count: i64,
}

/// Directories under each watched root that contain at least one visible
/// image, derived from images.path — the schema does not model subfolders.
pub fn folder_tree(conn: &Connection) -> rusqlite::Result<Vec<DirEntry>> {
    let mut stmt = conn.prepare(
        "SELECT i.folder_id, f.path, i.path FROM images i
         JOIN folders f ON f.id = i.folder_id WHERE i.hidden = 0",
    )?;
    let mut counts: std::collections::HashMap<(i64, String), i64> = std::collections::HashMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    for row in rows {
        let (folder_id, root, path) = row?;
        let dir = Path::new(&path).parent().map(|p| p.to_string_lossy().into_owned());
        let rel = match dir {
            Some(d) if d.len() > root.len() && d.starts_with(&root) => {
                d[root.len()..].trim_start_matches(['\\', '/']).to_string()
            }
            _ => String::new(),
        };
        *counts.entry((folder_id, rel)).or_insert(0) += 1;
    }
    let mut out: Vec<DirEntry> = counts
        .into_iter()
        .map(|((folder_id, rel_dir), count)| DirEntry { folder_id, rel_dir, count })
        .collect();
    out.sort_by(|a, b| (a.folder_id, &a.rel_dir).cmp(&(b.folder_id, &b.rel_dir)));
    Ok(out)
}

// ── phase 3: near-duplicate detection ────────────────────────

#[derive(Debug, Serialize)]
pub struct DupImage {
    pub id: i64,
    pub path: String,
    pub file_name: String,
    pub width: u32,
    pub height: u32,
    pub file_size: i64,
    pub file_mtime: i64,
    pub seed: Option<i64>,
    pub rating: i64,
    pub favorite: bool,
    /// Hamming distance to the closest other member (0 = visual twin).
    pub distance: u32,
}

#[derive(Debug, Serialize)]
pub struct DupResult {
    /// Visible images with no hash yet (thumbnails still being generated);
    /// nonzero means groups may be incomplete.
    pub unhashed: i64,
    pub groups: Vec<Vec<DupImage>>,
}

/// Union-find grouping of hashes within `max_distance` of each other
/// (transitive: A~B and B~C group A,B,C even if A–C exceeds the limit).
/// Returns groups of (index, min distance to another member), ≥2 members.
fn group_hashes(hashes: &[i64], max_distance: u32) -> Vec<Vec<(usize, u32)>> {
    use rayon::prelude::*;
    let n = hashes.len();
    // O(n²) pair scan: ~1s at 50k images, and libraries are far smaller.
    // Revisit with BK-tree / multi-index only if that stops being true.
    let edges: Vec<(usize, usize, u32)> = (0..n)
        .into_par_iter()
        .flat_map_iter(|i| {
            let hi = hashes[i];
            (i + 1..n).filter_map(move |j| {
                let d = (hi ^ hashes[j]).count_ones();
                (d <= max_distance).then_some((i, j, d))
            })
        })
        .collect();

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    let mut parent: Vec<usize> = (0..n).collect();
    let mut min_dist = vec![u32::MAX; n];
    for &(i, j, d) in &edges {
        let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
        if ri != rj {
            parent[ri] = rj;
        }
        min_dist[i] = min_dist[i].min(d);
        min_dist[j] = min_dist[j].min(d);
    }
    let mut groups: std::collections::HashMap<usize, Vec<(usize, u32)>> =
        std::collections::HashMap::new();
    for i in 0..n {
        if min_dist[i] != u32::MAX {
            groups.entry(find(&mut parent, i)).or_default().push((i, min_dist[i]));
        }
    }
    groups.into_values().filter(|g| g.len() >= 2).collect()
}

pub fn find_duplicates(conn: &Connection, max_distance: u32) -> rusqlite::Result<DupResult> {
    let unhashed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM images WHERE hidden = 0 AND phash IS NULL",
        [],
        |r| r.get(0),
    )?;
    let (ids, hashes): (Vec<i64>, Vec<i64>) = {
        let mut stmt =
            conn.prepare("SELECT id, phash FROM images WHERE hidden = 0 AND phash IS NOT NULL")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().unzip()
    };

    let mut detail_stmt = conn.prepare(
        "SELECT path, width, height, file_size, file_mtime, seed, rating, favorite
         FROM images WHERE id = ?1",
    )?;
    let mut groups: Vec<Vec<DupImage>> = Vec::new();
    for members in group_hashes(&hashes, max_distance) {
        let mut group = Vec::with_capacity(members.len());
        for (idx, distance) in members {
            let id = ids[idx];
            let img = detail_stmt.query_row(params![id], |r| {
                let path: String = r.get(0)?;
                let file_name = Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(DupImage {
                    id,
                    file_name,
                    path,
                    width: r.get(1)?,
                    height: r.get(2)?,
                    file_size: r.get(3)?,
                    file_mtime: r.get(4)?,
                    seed: r.get(5)?,
                    rating: r.get(6)?,
                    favorite: r.get::<_, i64>(7)? != 0,
                    distance,
                })
            })?;
            group.push(img);
        }
        // likely keeper first: favorited, then rated, then biggest file
        group.sort_by(|a, b| {
            (b.favorite, b.rating, b.file_size, a.id).cmp(&(a.favorite, a.rating, a.file_size, b.id))
        });
        groups.push(group);
    }
    // most recent batches first
    groups.sort_by_key(|g| std::cmp::Reverse(g.iter().map(|i| i.file_mtime).max().unwrap_or(0)));
    Ok(DupResult { unhashed, groups })
}

// ── phase 3: tag analytics ───────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TagPair {
    pub a: String,
    pub b: String,
    /// Visible images carrying both tags.
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct DayCount {
    /// Local calendar day, ISO `YYYY-MM-DD`.
    pub day: String,
    pub count: i64,
}

/// Rows every analytics aggregate counts: visible images, tags the user
/// hasn't hidden, and nothing derived from a negative prompt.
const VISIBLE_TAGGING: &str = "i.hidden = 0 AND t.hidden = 0 AND it.source <> 'negative'";

/// How many of the most-used tags the co-occurrence pass considers. The pair
/// scan is quadratic in the tags per image, so the long tail — which never
/// reaches the top of the result anyway — is cut before the self-join.
const COOC_POOL: i64 = 120;

/// Most-used tags over the visible library, same exclusions as autocomplete.
pub fn tag_frequency(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<TagSuggestion>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT t.name, t.category, COUNT(*) AS n
         FROM image_tags it
         JOIN tags t ON t.id = it.tag_id
         JOIN images i ON i.id = it.image_id
         WHERE {VISIBLE_TAGGING}
         GROUP BY t.id ORDER BY n DESC, t.name LIMIT ?1"
    ))?;
    let rows = stmt
        .query_map(params![limit], |r| {
            Ok(TagSuggestion {
                name: r.get(0)?,
                category: r.get(1)?,
                count: r.get(2)?,
            })
        })?
        .collect();
    rows
}

/// Tag pairs sharing an image, most frequent first. `a.tag_id < b.tag_id`
/// keeps each unordered pair once.
pub fn tag_cooccurrence(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<TagPair>> {
    let mut stmt = conn.prepare(&format!(
        "WITH visible AS (
           SELECT it.image_id, it.tag_id FROM image_tags it
           JOIN tags t ON t.id = it.tag_id
           JOIN images i ON i.id = it.image_id
           WHERE {VISIBLE_TAGGING}
         ),
         pool AS (
           SELECT image_id, tag_id FROM visible
           WHERE tag_id IN (
             SELECT tag_id FROM visible GROUP BY tag_id ORDER BY COUNT(*) DESC LIMIT ?2
           )
         )
         SELECT ta.name, tb.name, COUNT(*) AS n
         FROM pool a
         JOIN pool b ON b.image_id = a.image_id AND a.tag_id < b.tag_id
         JOIN tags ta ON ta.id = a.tag_id
         JOIN tags tb ON tb.id = b.tag_id
         GROUP BY a.tag_id, b.tag_id
         ORDER BY n DESC, ta.name, tb.name LIMIT ?1"
    ))?;
    let rows = stmt
        .query_map(params![limit, COOC_POOL], |r| {
            Ok(TagPair {
                a: r.get(0)?,
                b: r.get(1)?,
                count: r.get(2)?,
            })
        })?
        .collect();
    rows
}

/// Visible images per local calendar day over the last `days` days, oldest
/// first. Only days that have images are returned — the caller fills the gaps.
pub fn images_per_day(conn: &Connection, days: i64) -> rusqlite::Result<Vec<DayCount>> {
    let since = format!("-{} days", days.clamp(1, 3650) - 1);
    let mut stmt = conn.prepare(
        "SELECT date(file_mtime, 'unixepoch', 'localtime') AS d, COUNT(*)
         FROM images
         WHERE hidden = 0 AND date(file_mtime, 'unixepoch', 'localtime') >= date('now', 'localtime', ?1)
         GROUP BY d ORDER BY d",
    )?;
    let rows = stmt
        .query_map(params![since], |r| {
            Ok(DayCount {
                day: r.get(0)?,
                count: r.get(1)?,
            })
        })?
        .collect();
    rows
}

pub fn set_phash(conn: &Connection, id: i64, phash: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE images SET phash = ?2 WHERE id = ?1", params![id, phash])?;
    Ok(())
}

/// Remove images from the index (rows + thumbs are handled by caller for files).
pub fn get_paths_for_ids(conn: &Connection, ids: &[i64]) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT id, path FROM images WHERE id = ?1")?;
    let mut out = Vec::new();
    for id in ids {
        if let Ok(row) = stmt.query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?))) {
            out.push(row);
        }
    }
    Ok(out)
}

pub fn delete_images(conn: &Connection, ids: &[i64]) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("DELETE FROM images WHERE id = ?1")?;
    for id in ids {
        stmt.execute(params![id])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_chain_transitively_and_skip_singletons() {
        // 0b0000 – 0b0001 – 0b0011 chain within distance 1; 0b0011 is
        // distance 2 from 0b0000 but still joins the group transitively.
        // The far-away hash stays out entirely.
        let hashes = [0b0000, 0b0001, 0b0011, 0x7AF0_F0F0_F0F0_F0F0_u64 as i64];
        let groups = group_hashes(&hashes, 1);
        assert_eq!(groups.len(), 1);
        let mut members: Vec<usize> = groups[0].iter().map(|&(i, _)| i).collect();
        members.sort();
        assert_eq!(members, vec![0, 1, 2]);
        // middle member touches both neighbors at distance 1
        let dist_of = |idx: usize| groups[0].iter().find(|&&(i, _)| i == idx).unwrap().1;
        assert_eq!(dist_of(1), 1);
        assert_eq!(dist_of(0), 1);
    }

    #[test]
    fn no_groups_when_nothing_close() {
        let hashes = [0, 0x00FF_FF00_1234_5678_u64 as i64, -1];
        assert!(group_hashes(&hashes, 4).is_empty());
    }

    /// Baseline + every migration, exactly like `open()` does on disk.
    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn add_image(conn: &Connection, id: i64, mtime: i64, hidden: i64) {
        conn.execute(
            "INSERT INTO images(id, path, file_size, file_mtime, hidden)
             VALUES (?1, ?2, 100, ?3, ?4)",
            params![id, format!("D:\\g\\{id}.png"), mtime, hidden],
        )
        .unwrap();
    }

    fn tag_image(conn: &Connection, image_id: i64, name: &str, source: &str, hidden: i64) {
        conn.execute(
            "INSERT OR IGNORE INTO tags(name, category, hidden) VALUES (?1, 'general', ?2)",
            params![name, hidden],
        )
        .unwrap();
        let tag_id: i64 = conn
            .query_row("SELECT id FROM tags WHERE name = ?1", params![name], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO image_tags(image_id, tag_id, source) VALUES (?1, ?2, ?3)",
            params![image_id, tag_id, source],
        )
        .unwrap();
    }

    #[test]
    fn tag_frequency_counts_only_visible_and_unhidden() {
        let conn = mem_db();
        add_image(&conn, 1, 1_700_000_000, 0);
        add_image(&conn, 2, 1_700_000_000, 0);
        add_image(&conn, 3, 1_700_000_000, 1); // rejected — must not count
        for id in [1, 2, 3] {
            tag_image(&conn, id, "1girl", "base", 0);
        }
        tag_image(&conn, 1, "smile", "base", 0);
        tag_image(&conn, 1, "masterpiece", "base", 1); // hidden tag
        tag_image(&conn, 1, "bad hands", "negative", 0); // negative-prompt row

        let freq = tag_frequency(&conn, 10).unwrap();
        let names: Vec<(&str, i64)> = freq.iter().map(|t| (t.name.as_str(), t.count)).collect();
        assert_eq!(names, vec![("1girl", 2), ("smile", 1)]);
    }

    #[test]
    fn cooccurrence_pairs_each_combination_once() {
        let conn = mem_db();
        add_image(&conn, 1, 1_700_000_000, 0);
        add_image(&conn, 2, 1_700_000_000, 0);
        for id in [1, 2] {
            tag_image(&conn, id, "1girl", "base", 0);
            tag_image(&conn, id, "smile", "base", 0);
        }
        tag_image(&conn, 1, "blue hair", "base", 0);

        let pairs = tag_cooccurrence(&conn, 10).unwrap();
        assert_eq!(pairs.len(), 3);
        let top = &pairs[0];
        assert_eq!(top.count, 2);
        assert_eq!(
            [top.a.as_str(), top.b.as_str()].iter().copied().collect::<std::collections::HashSet<_>>(),
            ["1girl", "smile"].into_iter().collect()
        );
    }

    #[test]
    fn images_per_day_buckets_by_local_day_and_window() {
        let conn = mem_db();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        add_image(&conn, 1, now, 0);
        add_image(&conn, 2, now, 0);
        add_image(&conn, 3, now - 3 * 86_400, 0);
        add_image(&conn, 4, now, 1); // rejected
        add_image(&conn, 5, now - 400 * 86_400, 0); // outside the window

        let rows = images_per_day(&conn, 90).unwrap();
        let total: i64 = rows.iter().map(|r| r.count).sum();
        assert_eq!(total, 3);
        assert_eq!(rows.last().unwrap().count, 2);
        // ascending by day, and the far-past image never shows up
        assert!(rows.windows(2).all(|w| w[0].day < w[1].day));
    }
}
