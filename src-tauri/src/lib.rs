mod db;
mod nai;
mod scanner;

use db::Db;
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

type WatcherMap = Mutex<HashMap<i64, notify_debouncer_mini::Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>>>;
struct Watchers(WatcherMap);

// ── commands ─────────────────────────────────────────────────

#[tauri::command]
fn query_images(db: State<Db>, query: db::Query) -> Result<db::QueryResult, String> {
    let conn = db.0.lock().unwrap();
    db::query_images(&conn, &query).map_err(|e| e.to_string())
}

#[tauri::command]
fn suggest_tags(db: State<Db>, prefix: String) -> Result<Vec<db::TagSuggestion>, String> {
    let conn = db.0.lock().unwrap();
    db::suggest_tags(&conn, &prefix, 20).map_err(|e| e.to_string())
}

#[tauri::command]
fn top_tags(db: State<Db>) -> Result<Vec<db::TagSuggestion>, String> {
    let conn = db.0.lock().unwrap();
    db::top_tags(&conn, 25).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_image(db: State<Db>, id: i64) -> Result<db::ImageDetail, String> {
    let conn = db.0.lock().unwrap();
    db::get_image(&conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_rating(db: State<Db>, id: i64, rating: i64) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::set_rating(&conn, id, rating).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_favorite(db: State<Db>, id: i64, favorite: bool) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::set_favorite(&conn, id, favorite).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_folders(db: State<Db>) -> Result<Vec<db::FolderInfo>, String> {
    let conn = db.0.lock().unwrap();
    db::list_folders(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn stats(db: State<Db>) -> Result<db::LibraryStats, String> {
    let conn = db.0.lock().unwrap();
    db::stats(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn add_folder(app: AppHandle, db: State<Db>, path: String) -> Result<i64, String> {
    if !std::path::Path::new(&path).is_dir() {
        return Err(format!("Not a folder: {path}"));
    }
    let folder_id: i64 = {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO folders(path) VALUES (?1) ON CONFLICT(path) DO NOTHING",
            [&path],
        )
        .map_err(|e| e.to_string())?;
        conn.query_row("SELECT id FROM folders WHERE path = ?1", [&path], |r| r.get(0))
            .map_err(|e| e.to_string())?
    };
    spawn_scan(app.clone(), folder_id, path.clone());
    watch_folder(&app, folder_id, path);
    Ok(folder_id)
}

#[tauri::command]
fn remove_folder(app: AppHandle, db: State<Db>, id: i64) -> Result<(), String> {
    // collect image ids first so their cached thumbnails can be deleted too —
    // sqlite reuses rowids, so leftover thumbs would show up attached to
    // unrelated images if the folder is re-added later
    let image_ids: Vec<i64> = {
        let conn = db.0.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id FROM images WHERE folder_id = ?1")
            .map_err(|e| e.to_string())?;
        let ids: Vec<i64> = stmt
            .query_map([id], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        drop(stmt);
        conn.execute("DELETE FROM images WHERE folder_id = ?1", [id])
            .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM folders WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
        ids
    };
    let thumbs = scanner::thumbs_dir(&app);
    for image_id in image_ids {
        std::fs::remove_file(thumbs.join(format!("{image_id}.jpg"))).ok();
    }
    app.state::<Watchers>().0.lock().unwrap().remove(&id);
    Ok(())
}

#[tauri::command]
fn rescan_all(app: AppHandle, db: State<Db>) -> Result<(), String> {
    let folders: Vec<(i64, String)> = {
        let conn = db.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, path FROM folders").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        rows
    };
    for (id, path) in folders {
        spawn_scan(app.clone(), id, path);
    }
    Ok(())
}

#[tauri::command]
fn set_hidden_bulk(db: State<Db>, ids: Vec<i64>, hidden: bool) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::set_hidden_bulk(&conn, &ids, hidden).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_favorite_bulk(db: State<Db>, ids: Vec<i64>, favorite: bool) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::set_favorite_bulk(&conn, &ids, favorite).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_rating_bulk(db: State<Db>, ids: Vec<i64>, rating: i64) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::set_rating_bulk(&conn, &ids, rating).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_albums(db: State<Db>) -> Result<Vec<db::AlbumInfo>, String> {
    let conn = db.0.lock().unwrap();
    db::list_albums(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn create_album(db: State<Db>, name: String) -> Result<i64, String> {
    let conn = db.0.lock().unwrap();
    db::create_album(&conn, &name).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_album(db: State<Db>, id: i64) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::delete_album(&conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
fn add_to_album(db: State<Db>, album_id: i64, ids: Vec<i64>) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::add_to_album(&conn, album_id, &ids).map_err(|e| e.to_string())
}

#[tauri::command]
fn remove_from_album(db: State<Db>, album_id: i64, ids: Vec<i64>) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::remove_from_album(&conn, album_id, &ids).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_saved_searches(db: State<Db>) -> Result<Vec<db::SavedSearch>, String> {
    let conn = db.0.lock().unwrap();
    db::list_saved_searches(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn create_saved_search(db: State<Db>, name: String, query_json: String) -> Result<i64, String> {
    let conn = db.0.lock().unwrap();
    db::create_saved_search(&conn, &name, &query_json).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_saved_search(db: State<Db>, id: i64) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::delete_saved_search(&conn, id).map_err(|e| e.to_string())
}

// ── phase 2.5: user tags, tag hiding, folder tree ────────────

/// Add tag(s) to images. Input goes through the same normalization as
/// prompts, so "My OC, cool outfit" adds two tags that merge with any
/// prompt-derived spelling.
#[tauri::command]
fn add_user_tag(db: State<Db>, ids: Vec<i64>, name: String) -> Result<Vec<String>, String> {
    let tags = nai::normalize_tags(&name, "user");
    if tags.is_empty() {
        return Err("empty tag".into());
    }
    let conn = db.0.lock().unwrap();
    let mut added = Vec::new();
    for tag in &tags {
        db::add_user_tag(&conn, &ids, &tag.name).map_err(|e| e.to_string())?;
        added.push(tag.name.clone());
    }
    Ok(added)
}

#[tauri::command]
fn remove_user_tag(db: State<Db>, ids: Vec<i64>, name: String) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::remove_user_tag(&conn, &ids, &name).map_err(|e| e.to_string())
}

#[tauri::command]
fn set_tag_hidden(db: State<Db>, name: String, hidden: bool) -> Result<(), String> {
    let conn = db.0.lock().unwrap();
    db::set_tag_hidden(&conn, &name, hidden).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_hidden_tags(db: State<Db>) -> Result<Vec<db::TagSuggestion>, String> {
    let conn = db.0.lock().unwrap();
    db::list_hidden_tags(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn folder_tree(db: State<Db>) -> Result<Vec<db::DirEntry>, String> {
    let conn = db.0.lock().unwrap();
    db::folder_tree(&conn).map_err(|e| e.to_string())
}

/// Move files to the OS recycle bin and drop them from the index.
/// The ONLY file-modifying operation in the app; triggered explicitly by the user.
#[tauri::command]
fn trash_images(app: AppHandle, db: State<Db>, ids: Vec<i64>) -> Result<usize, String> {
    let paths = {
        let conn = db.0.lock().unwrap();
        db::get_paths_for_ids(&conn, &ids).map_err(|e| e.to_string())?
    };
    let existing: Vec<&String> = paths
        .iter()
        .map(|(_, p)| p)
        .filter(|p| std::path::Path::new(p.as_str()).exists())
        .collect();
    trash::delete_all(&existing).map_err(|e| e.to_string())?;
    {
        let conn = db.0.lock().unwrap();
        db::delete_images(&conn, &ids).map_err(|e| e.to_string())?;
    }
    let thumbs = scanner::thumbs_dir(&app);
    for (id, _) in &paths {
        std::fs::remove_file(thumbs.join(format!("{id}.jpg"))).ok();
    }
    Ok(existing.len())
}

#[tauri::command]
fn open_in_explorer(path: String) -> Result<(), String> {
    std::process::Command::new("explorer")
        .args(["/select,", &path])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ── helpers ──────────────────────────────────────────────────

fn spawn_scan(app: AppHandle, folder_id: i64, path: String) {
    std::thread::spawn(move || {
        if let Err(e) = scanner::scan_folder(&app, folder_id, &path) {
            app.emit("scan:error", e).ok();
        }
    });
}

fn watch_folder(app: &AppHandle, folder_id: i64, path: String) {
    let handle = app.clone();
    let watch_path = path.clone();
    let result = new_debouncer(Duration::from_secs(2), move |res| {
        if let Ok(_events) = res {
            // incremental rescan is cheap: only changed files are re-parsed
            spawn_scan(handle.clone(), folder_id, watch_path.clone());
        }
    })
    .and_then(|mut d| {
        d.watcher()
            .watch(std::path::Path::new(&path), RecursiveMode::Recursive)?;
        Ok(d)
    });
    if let Ok(debouncer) = result {
        app.state::<Watchers>().0.lock().unwrap().insert(folder_id, debouncer);
    }
}

fn serve_image(path_result: Result<std::path::PathBuf, String>) -> tauri::http::Response<Vec<u8>> {
    use tauri::http::{header, Response, StatusCode};
    match path_result.and_then(|p| std::fs::read(&p).map_err(|e| e.to_string()).map(|d| (p, d))) {
        Ok((p, data)) => {
            let mime = if p.extension().map(|e| e.eq_ignore_ascii_case("png")).unwrap_or(false) {
                "image/png"
            } else {
                "image/jpeg"
            };
            Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, "max-age=31536000, immutable")
                .body(data)
                .unwrap()
        }
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Vec::new())
            .unwrap(),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // thumb://<id>  → cached thumbnail jpeg
        .register_uri_scheme_protocol("thumb", |ctx, request| {
            let id: Result<i64, String> = request
                .uri()
                .path()
                .trim_start_matches('/')
                .trim_end_matches(".jpg")
                .parse()
                .map_err(|_| "bad id".to_string());
            let dir = scanner::thumbs_dir(ctx.app_handle());
            serve_image(id.map(|id| dir.join(format!("{id}.jpg"))))
        })
        // orig://<id>  → full-resolution original from its indexed path
        .register_uri_scheme_protocol("orig", |ctx, request| {
            let id: Result<i64, String> = request
                .uri()
                .path()
                .trim_start_matches('/')
                .parse()
                .map_err(|_| "bad id".to_string());
            let path = id.and_then(|id| {
                let db = ctx.app_handle().state::<Db>();
                let conn = db.0.lock().unwrap();
                db::get_image_path(&conn, id).map_err(|e| e.to_string())
            });
            serve_image(path.map(std::path::PathBuf::from))
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let db = db::open(&data_dir)?;
            app.manage(db);
            app.manage(Watchers(Mutex::new(HashMap::new())));

            // one-time cache reset: builds before the .v2 marker could leave
            // truncated thumbnails (non-atomic writes) or thumbs attached to
            // the wrong image (rowid reuse). The startup rescan's repair pass
            // regenerates everything that's missing.
            let thumbs = scanner::thumbs_dir(app.handle());
            let marker = thumbs.join(".v2");
            if !marker.exists() {
                std::fs::remove_dir_all(&thumbs).ok();
                std::fs::create_dir_all(&thumbs).ok();
                std::fs::write(&marker, b"").ok();
            }

            // start watchers + a background incremental rescan of all folders
            let handle = app.handle().clone();
            let folders: Vec<(i64, String)> = {
                let db = handle.state::<Db>();
                let conn = db.0.lock().unwrap();
                let mut stmt = conn.prepare("SELECT id, path FROM folders")?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .filter_map(Result::ok)
                    .collect();
                rows
            };
            for (id, path) in folders {
                watch_folder(&handle, id, path.clone());
                spawn_scan(handle.clone(), id, path);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            query_images,
            suggest_tags,
            top_tags,
            get_image,
            set_rating,
            set_favorite,
            list_folders,
            stats,
            add_folder,
            remove_folder,
            rescan_all,
            open_in_explorer,
            set_hidden_bulk,
            set_favorite_bulk,
            set_rating_bulk,
            list_albums,
            create_album,
            delete_album,
            add_to_album,
            remove_from_album,
            list_saved_searches,
            create_saved_search,
            delete_saved_search,
            trash_images,
            add_user_tag,
            remove_user_tag,
            set_tag_hidden,
            list_hidden_tags,
            folder_tree
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
