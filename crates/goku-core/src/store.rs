use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::commit::FramePlan;
use crate::error::RecorderError;
use crate::redact::redact_text;
use crate::types::{DataDir, FocusSnapshot, FrameId, RelativePath, SessionId, TriggerKind};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  status TEXT NOT NULL,
  config_json TEXT NOT NULL,
  os TEXT
);
CREATE TABLE IF NOT EXISTS frames (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  captured_at INTEGER NOT NULL,
  trigger TEXT NOT NULL,
  monitor_id INTEGER NOT NULL,
  app_name TEXT,
  app_bundle_id TEXT,
  window_title TEXT,
  browser_url TEXT,
  document_path TEXT,
  window_rect_json TEXT,
  image_path TEXT,
  thumb_path TEXT,
  image_width INTEGER,
  image_height INTEGER,
  image_bytes INTEGER,
  image_status TEXT NOT NULL,
  text TEXT,
  text_source TEXT,
  text_status TEXT NOT NULL,
  segments_json TEXT,
  content_hash TEXT,
  redaction_json TEXT,
  timings_json TEXT,
  error TEXT
);
CREATE INDEX IF NOT EXISTS idx_frames_session_time ON frames(session_id, captured_at DESC);
CREATE INDEX IF NOT EXISTS idx_frames_app_time ON frames(app_name, captured_at DESC);
CREATE INDEX IF NOT EXISTS idx_frames_trigger_time ON frames(trigger, captured_at DESC);
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameRow {
    pub id: FrameId,
    pub captured_at: i64,
    pub trigger: String,
    pub app_name: String,
    pub window_title: String,
    pub image_path: Option<String>,
    pub image_status: String,
    pub text: String,
}

pub struct Store {
    dir: DataDir,
    conn: Connection,
    session_id: SessionId,
    _lock: File,
}

impl Store {
    pub fn open(dir: DataDir) -> Result<Self, RecorderError> {
        let lock = lock_data_dir(dir.as_path())?;
        ensure_owned_dir(&dir.as_path().join("tmp"))?;
        ensure_owned_dir(&dir.as_path().join("sessions"))?;
        let conn = Connection::open(dir.as_path().join("db.sqlite")).map_err(db_err)?;
        conn.busy_timeout(std::time::Duration::from_secs(3))
            .map_err(db_err)?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(db_err)?;
        let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()));
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        let now = now_ms();
        conn.execute(
            "UPDATE sessions SET status = 'interrupted', ended_at = ?1 WHERE ended_at IS NULL",
            [now],
        )
        .map_err(db_err)?;
        wipe_tmp(dir.as_path())?;
        delete_orphan_jpegs(&dir, &conn)?;
        let session_id = SessionId::fresh();
        let os = std::env::consts::OS;
        conn.execute(
            "INSERT INTO sessions (id, started_at, ended_at, status, config_json, os) VALUES (?1, ?2, NULL, 'recording', '{}', ?3)",
            params![session_id.as_str(), now, os],
        )
        .map_err(db_err)?;
        let store = Self {
            dir,
            conn,
            session_id,
            _lock: lock,
        };
        Ok(store)
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn frame_path(&self, id: FrameId) -> Result<RelativePath, RecorderError> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT image_path FROM frames WHERE id = ?1",
                [id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?
            .flatten();
        let path = path.ok_or_else(|| RecorderError::StoreFailed("frame has no image".into()))?;
        image_rel(&path)
    }

    pub fn retain(&self, max_age_days: u32, max_bytes: u64) -> Result<(), RecorderError> {
        if max_age_days > 0 {
            let cutoff =
                now_ms().saturating_sub(i64::from(max_age_days).saturating_mul(86_400_000));
            let old = self.frame_ids("captured_at < ?1", (cutoff,))?;
            for (id, path) in old {
                self.delete_owned_frame(id, path.as_deref())?;
            }
        }
        if max_bytes > 0 {
            loop {
                let total: i64 = self
                    .conn
                    .query_row(
                        "SELECT COALESCE(SUM(image_bytes), 0) FROM frames",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(db_err)?;
                if total <= 0 || (total as u64) <= max_bytes {
                    break;
                }
                let oldest: Option<(i64, Option<String>)> = self
                    .conn
                    .query_row(
                        "SELECT id, image_path FROM frames ORDER BY captured_at ASC, id ASC LIMIT 1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(db_err)?;
                let Some((id, path)) = oldest else {
                    break;
                };
                self.delete_owned_frame(id, path.as_deref())?;
            }
        }
        Ok(())
    }

    fn frame_ids(
        &self,
        clause: &str,
        cutoff: (i64,),
    ) -> Result<Vec<(i64, Option<String>)>, RecorderError> {
        let sql = format!("SELECT id, image_path FROM frames WHERE {clause}");
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(params![cutoff.0], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err)?);
        }
        Ok(out)
    }

    fn delete_owned_frame(&self, id: i64, image_path: Option<&str>) -> Result<(), RecorderError> {
        if let Some(raw) = image_path {
            if let Ok(rel) = image_rel(raw) {
                if let Ok(full) = self.dir.resolve(&rel) {
                    if full.starts_with(self.dir.as_path()) {
                        let _ = fs::remove_file(full);
                    }
                }
            }
        }
        self.conn
            .execute("DELETE FROM frames WHERE id = ?1", [id])
            .map_err(db_err)?;
        Ok(())
    }

    pub fn finish(&self) -> Result<(), RecorderError> {
        self.conn
            .execute(
                "UPDATE sessions SET ended_at = ?1, status = 'completed' WHERE id = ?2 AND ended_at IS NULL",
                params![now_ms(), self.session_id.as_str()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn recent_frames(&self, limit: u32) -> Result<Vec<FrameRow>, RecorderError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = limit.min(500);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, captured_at, trigger, COALESCE(app_name, ''), COALESCE(window_title, ''), image_path, image_status, COALESCE(text, '')
                 FROM frames ORDER BY captured_at DESC, id DESC LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(FrameRow {
                    id: FrameId(row.get(0)?),
                    captured_at: row.get(1)?,
                    trigger: row.get(2)?,
                    app_name: row.get(3)?,
                    window_title: row.get(4)?,
                    image_path: row.get(5)?,
                    image_status: row.get(6)?,
                    text: row.get(7)?,
                })
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err)?);
        }
        Ok(out)
    }

    pub fn commit(
        &self,
        plan: &FramePlan,
        trigger: TriggerKind,
        focus: &FocusSnapshot,
        jpeg: &[u8],
    ) -> Result<FrameId, RecorderError> {
        let FramePlan::Row {
            text,
            labels,
            image,
            image_status,
        } = plan
        else {
            return Err(RecorderError::capture("discarded frame is not stored"));
        };
        if image.is_some() && jpeg.is_empty() {
            return Err(RecorderError::StoreFailed(
                "ready frame has no jpeg bytes".into(),
            ));
        }
        let captured_at = now_ms();
        let rel = if image.is_some() {
            Some(self.frame_rel(captured_at, focus.display_id)?)
        } else {
            None
        };
        let written = if let Some(rel) = &rel {
            Some(write_jpeg(&self.dir, rel, jpeg)?)
        } else {
            None
        };
        let title = redact_text(&focus.window_title);
        let stored_text = redact_text(text).text;
        let mut label_names = labels.clone();
        for label in title.labels {
            if !label_names.contains(&label) {
                label_names.push(label);
            }
        }
        let redaction_json = serde_json::json!({
            "labels": label_names,
            "count": label_names.len(),
        })
        .to_string();
        let rect_json = serde_json::json!({
            "x": focus.rect.x,
            "y": focus.rect.y,
            "width": focus.rect.width,
            "height": focus.rect.height,
            "scale": focus.rect.scale,
        })
        .to_string();
        let (width, height, nbytes) = match image {
            Some(img) => (
                Some(i64::from(img.width())),
                Some(i64::from(img.height())),
                Some(jpeg.len() as i64),
            ),
            None => (None, None, None),
        };
        let image_path = rel.as_ref().map(|path| path.as_str().to_string());
        let inserted = self.conn.execute(
            "INSERT INTO frames (
                session_id, captured_at, trigger, monitor_id,
                app_name, app_bundle_id, window_title, browser_url, document_path,
                window_rect_json, image_path, thumb_path, image_width, image_height, image_bytes,
                image_status, text, text_source, text_status, segments_json, content_hash,
                redaction_json, timings_json, error
             ) VALUES (
                ?1, ?2, ?3, ?4,
                ?5, ?6, ?7, NULL, NULL,
                ?8, ?9, NULL, ?10, ?11, ?12,
                ?13, ?14, NULL, 'complete', NULL, NULL,
                ?15, NULL, NULL
             )",
            params![
                self.session_id.as_str(),
                captured_at,
                trigger.as_str(),
                i64::from(focus.display_id),
                focus.app_name,
                focus.identity.app_id,
                title.text,
                rect_json,
                image_path,
                width,
                height,
                nbytes,
                image_status.as_str(),
                stored_text,
                redaction_json,
            ],
        );
        if let Err(err) = inserted {
            if let Some(path) = written {
                let _ = fs::remove_file(path);
            }
            return Err(db_err(err));
        }
        let id = self.conn.last_insert_rowid();
        if id <= 0 {
            if let Some(path) = written {
                let _ = fs::remove_file(path);
            }
            return Err(RecorderError::StoreFailed(
                "frame insert did not return an id".into(),
            ));
        }
        Ok(FrameId(id))
    }

    fn frame_rel(&self, captured_at: i64, monitor_id: u32) -> Result<RelativePath, RecorderError> {
        static N: AtomicU64 = AtomicU64::new(1);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let raw = format!(
            "sessions/{}/frames/{}/{captured_at}_{monitor_id}_{n}.jpg",
            self.session_id.as_str(),
            ymd(captured_at),
        );
        let rel = image_rel(&raw)?;
        self.dir.resolve(&rel)?;
        Ok(rel)
    }
}

pub struct ReadStore {
    dir: DataDir,
    conn: Connection,
}

impl ReadStore {
    pub fn open(dir: &DataDir) -> Result<Self, RecorderError> {
        let path = dir.as_path().join("db.sqlite");
        if !path.is_file() {
            return Err(RecorderError::StoreFailed("no database".into()));
        }
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(db_err)?;
        Ok(Self {
            dir: dir.clone(),
            conn,
        })
    }

    pub fn summary(&self) -> Result<(u64, Option<String>, Option<String>), RecorderError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM frames", [], |row| row.get(0))
            .unwrap_or(0);
        let session: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM sessions ORDER BY started_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let trigger: Option<String> = self
            .conn
            .query_row(
                "SELECT trigger FROM frames ORDER BY captured_at DESC, id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        Ok((count.max(0) as u64, session, trigger))
    }

    pub fn recent(
        &self,
        query: &crate::types::RecentFramesQuery,
    ) -> Result<Vec<crate::types::FrameSummary>, RecorderError> {
        let limit = query.limit.clamp(1, 500);
        let mut sql = String::from(
            "SELECT id, session_id, captured_at, trigger, COALESCE(app_name, ''), COALESCE(window_title, ''), image_path, image_status, COALESCE(text, '') FROM frames WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(before) = query.before {
            sql.push_str(" AND captured_at < ?");
            args.push(Box::new(before));
        }
        if let Some(session) = &query.session_id {
            sql.push_str(" AND session_id = ?");
            args.push(Box::new(session.clone()));
        }
        if let Some(app) = &query.app {
            sql.push_str(" AND app_name = ?");
            args.push(Box::new(app.clone()));
        }
        if let Some(trigger) = &query.trigger {
            sql.push_str(" AND trigger = ?");
            args.push(Box::new(trigger.clone()));
        }
        sql.push_str(" ORDER BY captured_at DESC, id DESC LIMIT ?");
        args.push(Box::new(i64::from(limit)));
        let mut stmt = self.conn.prepare(&sql).map_err(db_err)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> = args.iter().map(|arg| arg.as_ref()).collect();
        let rows = stmt
            .query_map(refs.as_slice(), |row| {
                let text: String = row.get(8)?;
                let preview: String = text.chars().take(200).collect();
                let image_path: Option<String> = row.get(6)?;
                Ok(crate::types::FrameSummary {
                    id: FrameId(row.get(0)?),
                    session_id: row.get(1)?,
                    captured_at: row.get(2)?,
                    trigger: row.get(3)?,
                    app_name: row.get(4)?,
                    window_title: row.get(5)?,
                    image_status: row.get(7)?,
                    image_available: image_path.is_some(),
                    text_preview: preview,
                })
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err)?);
        }
        Ok(out)
    }

    pub fn frame(&self, id: FrameId) -> Result<crate::types::Frame, RecorderError> {
        self.conn
            .query_row(
                "SELECT id, session_id, captured_at, trigger, COALESCE(app_name, ''), COALESCE(app_bundle_id, ''), COALESCE(window_title, ''), image_path, image_width, image_height, image_status, COALESCE(text, ''), text_source, redaction_json FROM frames WHERE id = ?1",
                [id.0],
                |row| {
                    Ok(crate::types::Frame {
                        id: FrameId(row.get(0)?),
                        session_id: row.get(1)?,
                        captured_at: row.get(2)?,
                        trigger: row.get(3)?,
                        app_name: row.get(4)?,
                        app_bundle_id: row.get(5)?,
                        window_title: row.get(6)?,
                        image_path: row.get(7)?,
                        image_width: row.get::<_, Option<i64>>(8)?.map(|n| n as u32),
                        image_height: row.get::<_, Option<i64>>(9)?.map(|n| n as u32),
                        image_status: row.get(10)?,
                        text: row.get(11)?,
                        text_source: row.get(12)?,
                        redaction_json: row.get(13)?,
                    })
                },
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| RecorderError::StoreFailed("frame not found".into()))
    }

    pub fn image_bytes(&self, id: FrameId) -> Result<Vec<u8>, RecorderError> {
        let frame = self.frame(id)?;
        let raw = frame
            .image_path
            .ok_or_else(|| RecorderError::StoreFailed("frame has no image".into()))?;
        let rel = image_rel(&raw)?;
        let full = self.dir.resolve(&rel)?;
        fs::read(&full).map_err(io_err)
    }
}

fn image_rel(raw: &str) -> Result<RelativePath, RecorderError> {
    RelativePath::parse(raw)
}

fn write_jpeg(dir: &DataDir, rel: &RelativePath, jpeg: &[u8]) -> Result<PathBuf, RecorderError> {
    let final_path = dir.resolve(rel)?;
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
    }
    let tmp = dir.as_path().join("tmp");
    fs::create_dir_all(&tmp).map_err(io_err)?;
    let partial = tmp.join(format!("{}.jpg.partial", unique_id()));
    let write_result = (|| {
        let mut file = File::create(&partial).map_err(io_err)?;
        file.write_all(jpeg).map_err(io_err)?;
        file.sync_all().map_err(io_err)?;
        Ok(())
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_file(&partial);
        return Err(err);
    }
    if let Err(err) = fs::rename(&partial, &final_path) {
        let _ = fs::remove_file(&partial);
        return Err(io_err(err));
    }
    Ok(final_path)
}

fn ensure_owned_dir(path: &Path) -> Result<(), RecorderError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(RecorderError::config(
            "refusing to follow a symlink directory",
        )),
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(RecorderError::config("expected a directory")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(io_err)
        }
        Err(err) => Err(io_err(err)),
    }
}

fn lock_data_dir(dir: &Path) -> Result<File, RecorderError> {
    let path = dir.join(".store.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(io_err)?;
    lock_exclusive(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn lock_exclusive(file: &File) -> Result<(), RecorderError> {
    use std::os::unix::io::AsRawFd;
    const LOCK_EX: i32 = 2;
    let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
    if rc != 0 {
        return Err(io_err(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(unix)]
extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(windows)]
fn lock_exclusive(file: &File) -> Result<(), RecorderError> {
    use std::os::windows::io::AsRawHandle;
    let rc = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            2,
            0,
            u32::MAX,
            u32::MAX,
            std::ptr::null_mut(),
        )
    };
    if rc == 0 {
        return Err(io_err(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(windows)]
extern "system" {
    fn LockFileEx(
        file: *mut std::ffi::c_void,
        flags: u32,
        reserved: u32,
        bytes_low: u32,
        bytes_high: u32,
        overlapped: *mut std::ffi::c_void,
    ) -> i32;
}

fn wipe_tmp(dir: &Path) -> Result<(), RecorderError> {
    let tmp = dir.join("tmp");
    fs::create_dir_all(&tmp).map_err(io_err)?;
    for entry in fs::read_dir(&tmp).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let path = entry.path();
        let ft = entry.file_type().map_err(io_err)?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            fs::remove_dir_all(&path).map_err(io_err)?;
        } else {
            fs::remove_file(&path).map_err(io_err)?;
        }
    }
    Ok(())
}

fn delete_orphan_jpegs(dir: &DataDir, conn: &Connection) -> Result<(), RecorderError> {
    let mut known = std::collections::HashSet::new();
    let mut stmt = conn
        .prepare("SELECT image_path FROM frames WHERE image_path IS NOT NULL")
        .map_err(db_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(db_err)?;
    for row in rows {
        known.insert(row.map_err(db_err)?);
    }
    let mut files = Vec::new();
    walk_files(dir.as_path(), &mut files)?;
    for path in files {
        if !is_jpeg(&path) {
            continue;
        }
        let Some(rel) = path.strip_prefix(dir.as_path()).ok() else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if known.contains(&rel) {
            continue;
        }
        if path.starts_with(dir.as_path()) {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), RecorderError> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let path = entry.path();
        let ft = entry.file_type().map_err(io_err)?;
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            walk_files(&path, out)?;
        } else if ft.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn is_jpeg(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("jpg") | Some("jpeg")
    )
}

fn unique_id() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    format!("{}-{}", now_ms(), N.fetch_add(1, Ordering::Relaxed))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn ymd(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    format!("{y:04}-{m:02}-{d:02}")
}

fn db_err(err: rusqlite::Error) -> RecorderError {
    RecorderError::StoreFailed(err.to_string())
}

fn io_err(err: std::io::Error) -> RecorderError {
    if matches!(err.raw_os_error(), Some(28) | Some(112)) {
        RecorderError::DiskFull
    } else {
        RecorderError::StoreFailed(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::FramePlan;
    use crate::imagebuf::{prepare_image, RawImage};
    use crate::types::{FocusIdentity, FrameId, ImageStatus, PixelPolicy, WindowRect};

    fn temp_dir() -> DataDir {
        static N: AtomicU64 = AtomicU64::new(1);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("goku-store-{n}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        DataDir::create(&path).unwrap()
    }

    fn focus(title: &str) -> FocusSnapshot {
        FocusSnapshot {
            identity: FocusIdentity {
                pid: 7,
                window_id: 9,
                app_id: "com.example.notes".into(),
            },
            app_name: "Notes".into(),
            window_title: title.into(),
            rect: WindowRect {
                x: 1,
                y: 2,
                width: 30,
                height: 40,
                scale: 2,
            },
            display_id: 1,
        }
    }

    fn ready(text: &str) -> (FramePlan, Vec<u8>) {
        let raw = RawImage::new(1, 1, vec![8, 9, 10, 255]).unwrap();
        let image = prepare_image(raw, &[], &[], PixelPolicy::Off).unwrap();
        let jpeg = vec![0xff, 0xd8, 0xff, 0xd9];
        (
            FramePlan::Row {
                text: text.into(),
                labels: vec![],
                image: Some(image),
                image_status: ImageStatus::Ready,
            },
            jpeg,
        )
    }

    fn jpeg_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        walk_files(dir, &mut files).unwrap();
        files.into_iter().filter(|path| is_jpeg(path)).collect()
    }

    #[test]
    fn open_creates_the_layout_and_a_second_open_is_safe() {
        let dir = temp_dir();
        let store = Store::open(dir.clone()).unwrap();
        assert!(dir.as_path().join("db.sqlite").is_file());
        assert!(dir.as_path().join("tmp").is_dir());
        assert!(dir.as_path().join("sessions").is_dir());
        let id = store.session_id().as_str().to_string();
        drop(store);
        let store = Store::open(dir).unwrap();
        assert_ne!(store.session_id().as_str(), id);
        let status: String = store
            .conn
            .query_row("SELECT status FROM sessions WHERE id = ?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "interrupted");
    }

    #[test]
    fn commit_writes_a_jpeg_and_recent_frames_returns_it() {
        let dir = temp_dir();
        let store = Store::open(dir.clone()).unwrap();
        let (plan, jpeg) = ready("hello");
        let id = store
            .commit(&plan, TriggerKind::Manual, &focus("notes"), &jpeg)
            .unwrap();
        let rows = store.recent_frames(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].trigger, "manual");
        assert_eq!(rows[0].text, "hello");
        assert_eq!(rows[0].image_status, "ready");
        let rel = store.frame_path(id).unwrap();
        let full = dir.resolve(&rel).unwrap();
        assert!(full.is_file());
        assert!(full.canonicalize().unwrap().starts_with(dir.as_path()));
        assert!(rel
            .as_str()
            .starts_with(&format!("sessions/{}/frames/", store.session_id().as_str())));
    }

    #[test]
    fn a_final_jpeg_with_no_row_is_deleted_on_open() {
        let dir = temp_dir();
        let store = Store::open(dir.clone()).unwrap();
        let (plan, jpeg) = ready("keep");
        let id = store
            .commit(&plan, TriggerKind::Click, &focus("notes"), &jpeg)
            .unwrap();
        let full = dir.resolve(&store.frame_path(id).unwrap()).unwrap();
        assert!(full.exists());
        store.conn.execute("DELETE FROM frames", []).unwrap();
        drop(store);
        let _store = Store::open(dir).unwrap();
        assert!(!full.exists());
    }

    #[test]
    fn a_partial_file_is_wiped_on_open() {
        let dir = temp_dir();
        let store = Store::open(dir.clone()).unwrap();
        drop(store);
        let partial = dir.as_path().join("tmp").join("crash.jpg.partial");
        fs::write(&partial, b"partial").unwrap();
        let _store = Store::open(dir.clone()).unwrap();
        assert!(fs::read_dir(dir.as_path().join("tmp"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn a_jpeg_with_a_row_survives_reopen() {
        let dir = temp_dir();
        let store = Store::open(dir.clone()).unwrap();
        let (plan, jpeg) = ready("stay");
        let id = store
            .commit(&plan, TriggerKind::Idle, &focus("notes"), &jpeg)
            .unwrap();
        let full = dir.resolve(&store.frame_path(id).unwrap()).unwrap();
        drop(store);
        let store = Store::open(dir).unwrap();
        assert!(full.exists());
        assert_eq!(store.recent_frames(1).unwrap()[0].text, "stay");
    }

    #[test]
    fn an_escaping_relative_path_is_rejected() {
        for raw in [
            "../outside.jpg",
            "sessions/../../outside.jpg",
            "/tmp/x.jpg",
            "C:\\x.jpg",
        ] {
            assert!(image_rel(raw).is_err(), "{raw}");
        }
        assert!(image_rel("sessions/abc/frames/a.jpg").is_ok());
        let dir = temp_dir();
        let store = Store::open(dir).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO frames (session_id, captured_at, trigger, monitor_id, image_status, text_status, image_path)
                 VALUES (?1, 1, 'manual', 1, 'ready', 'complete', '../outside.jpg')",
                [store.session_id().as_str()],
            )
            .unwrap();
        let id = FrameId(store.conn.last_insert_rowid());
        assert!(store.frame_path(id).is_err());
    }

    #[test]
    fn a_failed_insert_removes_the_renamed_jpeg() {
        let dir = temp_dir();
        let store = Store::open(dir.clone()).unwrap();
        store.conn.execute_batch("DROP TABLE frames;").unwrap();
        let (plan, jpeg) = ready("nope");
        let err = store
            .commit(&plan, TriggerKind::Manual, &focus("notes"), &jpeg)
            .unwrap_err();
        assert!(matches!(err, RecorderError::StoreFailed(_)));
        assert!(jpeg_files(dir.as_path()).is_empty());
    }

    #[test]
    fn a_withheld_plan_writes_no_jpeg_and_redacts_the_title() {
        let dir = temp_dir();
        let store = Store::open(dir.clone()).unwrap();
        let plan = FramePlan::Row {
            text: "password=hunter2".into(),
            labels: vec!["PASSWORD".into()],
            image: None,
            image_status: ImageStatus::WithheldRedaction,
        };
        let id = store
            .commit(
                &plan,
                TriggerKind::Manual,
                &focus("password=hunter2"),
                b"jpeg",
            )
            .unwrap();
        assert!(store.frame_path(id).is_err());
        assert!(jpeg_files(dir.as_path()).is_empty());
        let row = &store.recent_frames(1).unwrap()[0];
        assert_eq!(row.image_status, "withheld_redaction");
        assert!(!row.text.contains("hunter2"));
        assert!(row.text.contains("[REDACTED:PASSWORD]"));
        assert!(!row.window_title.contains("hunter2"));
        assert!(row.window_title.contains("password=[REDACTED:PASSWORD]"));
    }

    #[test]
    fn recent_frames_returns_the_newest_first() {
        let dir = temp_dir();
        let store = Store::open(dir).unwrap();
        let (first, jpeg) = ready("one");
        let (second, jpeg2) = ready("two");
        store
            .commit(&first, TriggerKind::Click, &focus("a"), &jpeg)
            .unwrap();
        store
            .commit(&second, TriggerKind::Manual, &focus("b"), &jpeg2)
            .unwrap();
        let rows = store.recent_frames(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "two");
        assert_eq!(store.recent_frames(0).unwrap().len(), 0);
    }

    #[test]
    fn finish_marks_the_session_completed_and_reopen_keeps_it() {
        let dir = temp_dir();
        let store = Store::open(dir.clone()).unwrap();
        store.finish().unwrap();
        let id = store.session_id().as_str().to_string();
        drop(store);
        let store = Store::open(dir).unwrap();
        let status: String = store
            .conn
            .query_row("SELECT status FROM sessions WHERE id = ?1", [&id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "completed");
    }

    #[test]
    fn unix_epoch_is_1970_01_01() {
        assert_eq!(ymd(0), "1970-01-01");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_outside_the_data_dir_is_not_deleted() {
        let dir = temp_dir();
        let outside = std::env::temp_dir().join(format!(
            "goku-outside-{}-{}.jpg",
            std::process::id(),
            unique_id()
        ));
        fs::write(&outside, b"keep").unwrap();
        fs::create_dir_all(dir.as_path().join("sessions")).unwrap();
        let link = dir.as_path().join("sessions").join("escape.jpg");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let _store = Store::open(dir).unwrap();
        assert!(outside.exists(), "orphan cleanup followed a symlink");
        let _ = fs::remove_file(&outside);
    }
}
