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

pub fn open(app_data_dir: &Path) -> rusqlite::Result<Db> {
    std::fs::create_dir_all(app_data_dir).ok();
    let conn = Connection::open(app_data_dir.join("library.sqlite"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
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
    pub album_id: Option<i64>,
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
         WHERE t.name LIKE ?1
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
        "SELECT t.name, t.category, it.source FROM image_tags it
         JOIN tags t ON t.id = it.tag_id WHERE it.image_id = ?1
         ORDER BY CASE t.category WHEN 'artist' THEN 0 WHEN 'general' THEN 1 ELSE 2 END, t.name",
    )?;
    detail.tags = stmt
        .query_map(params![id], |r| {
            Ok(TagDetail {
                name: r.get(0)?,
                category: r.get(1)?,
                source: r.get(2)?,
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
