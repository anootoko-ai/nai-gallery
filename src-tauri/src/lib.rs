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
    {
        let conn = db.0.lock().unwrap();
        conn.execute("DELETE FROM images WHERE folder_id = ?1", [id])
            .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM folders WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
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
            let id = request.uri().path().trim_start_matches('/').trim_end_matches(".jpg").to_string();
            let dir = scanner::thumbs_dir(ctx.app_handle());
            serve_image(Ok(dir.join(format!("{id}.jpg"))))
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
            open_in_explorer
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
