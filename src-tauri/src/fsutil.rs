//! Path sanitization and save paths for received files.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

/// Chunk size used by the data plane; resume dirty-tail alignment uses the same value.
pub const CHUNK: u64 = 256 * 1024;

/// Keep only the final path component; reject empty / dangerous names.
pub fn safe_basename(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("empty file name".into());
    }
    let base = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim();
    if base.is_empty() || base == "." || base == ".." {
        return Err("invalid file name".into());
    }
    if base.contains('/') || base.contains('\\') || base.contains('\0') {
        return Err("invalid file name characters".into());
    }
    Ok(base.to_string())
}

pub fn default_save_dir() -> Result<PathBuf, String> {
    // macOS/Linux: $HOME/Downloads · Windows: %USERPROFILE%\Downloads
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| "home directory not set (HOME / USERPROFILE)".to_string())?;
    Ok(PathBuf::from(home).join("Downloads").join("jotainchatttttttt"))
}

/// True if final path or its `.partial` already exists (counts as taken for naming).
pub fn path_or_partial_exists(candidate: &Path) -> bool {
    candidate.exists() || partial_path(candidate).exists()
}

/// Ensure dir exists; return unique path if `name` already exists (name (1).ext …).
/// Treats existing `*.partial` as taken so resume reservations do not collide.
pub fn unique_dest(dir: &Path, basename: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create save dir: {e}"))?;
    let candidate = dir.join(basename);
    if !path_or_partial_exists(&candidate) {
        return Ok(candidate);
    }
    let stem = Path::new(basename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = Path::new(basename)
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    for i in 1..10_000 {
        let name = format!("{stem} ({i}){ext}");
        let p = dir.join(&name);
        if !path_or_partial_exists(&p) {
            return Ok(p);
        }
    }
    Err("could not allocate unique file name".into())
}

pub fn partial_path(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_os_string();
    s.push(".partial");
    PathBuf::from(s)
}

/// Reserve exclusive final + partial paths for an Accept.
///
/// Creates a 0-byte placeholder at `dest` and an empty `dest.partial` with `create_new`.
/// If partial creation fails after dest was created, rolls back the dest placeholder.
pub fn reserve_dest(dir: &Path, basename: &str) -> Result<(PathBuf, PathBuf), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create save dir: {e}"))?;

    // Try basename then stem (i).ext like unique_dest.
    let stem = Path::new(basename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = Path::new(basename)
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();

    let mut last_err = String::from("could not reserve file name");
    for i in 0..10_000 {
        let name = if i == 0 {
            basename.to_string()
        } else {
            format!("{stem} ({i}){ext}")
        };
        let dest = dir.join(&name);
        if path_or_partial_exists(&dest) {
            continue;
        }
        match try_create_reservation(&dest) {
            Ok(partial) => return Ok((dest, partial)),
            Err(e) => {
                last_err = e;
                // Race or transient; try next candidate.
                continue;
            }
        }
    }
    Err(last_err)
}

fn try_create_reservation(dest: &Path) -> Result<PathBuf, String> {
    // Step 1: exclusive placeholder at final path.
    match OpenOptions::new().write(true).create_new(true).open(dest) {
        Ok(f) => drop(f),
        Err(e) => return Err(format!("create dest placeholder: {e}")),
    }
    let partial = partial_path(dest);
    // Step 2: empty partial; on failure roll back dest.
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
    {
        Ok(f) => {
            drop(f);
            Ok(partial)
        }
        Err(e) => {
            let _ = fs::remove_file(dest);
            Err(format!("create partial: {e}"))
        }
    }
}

/// Align resume offset down to CHUNK boundary (dirty-tail truncate target).
pub fn align_resume_offset(partial_len: u64) -> u64 {
    (partial_len / CHUNK) * CHUNK
}

/// Truncate partial file to chunk-aligned length. Returns the aligned offset.
pub fn prepare_partial_for_resume(partial: &Path) -> Result<u64, String> {
    let meta = fs::metadata(partial).map_err(|e| format!("stat partial: {e}"))?;
    let len = meta.len();
    let aligned = align_resume_offset(len);
    if aligned < len {
        let f = OpenOptions::new()
            .write(true)
            .open(partial)
            .map_err(|e| format!("open partial for truncate: {e}"))?;
        f.set_len(aligned)
            .map_err(|e| format!("truncate partial: {e}"))?;
    }
    Ok(aligned)
}

/// Remove transfer reservation files (partial + empty placeholder dest).
pub fn cleanup_reservation(dest: Option<&Path>, partial: Option<&Path>) {
    if let Some(p) = partial {
        let _ = fs::remove_file(p);
    }
    if let Some(d) = dest {
        // Only remove if still a placeholder (0 bytes) or missing content.
        // If rename already completed, dest is the real file — do not delete.
        match fs::metadata(d) {
            Ok(m) if m.len() == 0 => {
                let _ = fs::remove_file(d);
            }
            Ok(_) => {
                // Non-empty final file: leave it (completed transfer or user file).
            }
            Err(_) => {}
        }
        // If partial still existed and dest is empty placeholder, already handled.
        // If only dest exists as empty after partial removed, clean it.
        if !partial_path(d).exists() {
            if let Ok(m) = fs::metadata(d) {
                if m.len() == 0 {
                    let _ = fs::remove_file(d);
                }
            }
        }
    }
}

/// Promote finished partial over placeholder dest.
pub fn finalize_partial_to_dest(partial: &Path, dest: &Path) -> Result<(), String> {
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    fs::rename(partial, dest).map_err(|e| format!("rename: {e}"))
}

/// Open reserved partial for write (R1: rewrite from offset 0 into reserved empty partial).
pub fn open_partial_write(partial: &Path) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .read(true)
        .open(partial)
        .map_err(|e| format!("open partial: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn basename_strips_traversal() {
        assert_eq!(safe_basename("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(safe_basename("photo.png").unwrap(), "photo.png");
        assert!(safe_basename("..").is_err());
        assert!(safe_basename("").is_err());
    }

    #[test]
    fn align_resume_offset_edges() {
        assert_eq!(align_resume_offset(0), 0);
        assert_eq!(align_resume_offset(CHUNK - 1), 0);
        assert_eq!(align_resume_offset(CHUNK), CHUNK);
        assert_eq!(align_resume_offset(CHUNK + 1), CHUNK);
        assert_eq!(align_resume_offset(CHUNK * 3 + 100), CHUNK * 3);
    }

    #[test]
    fn reserve_dest_exclusive_and_partial_counts() {
        let dir = std::env::temp_dir().join(format!("jotain-fsutil-{}", uuid_like()));
        fs::create_dir_all(&dir).unwrap();
        let (d1, p1) = reserve_dest(&dir, "movie.mp4").unwrap();
        assert!(d1.exists());
        assert!(p1.exists());
        assert_eq!(fs::metadata(&d1).unwrap().len(), 0);
        assert_eq!(fs::metadata(&p1).unwrap().len(), 0);

        let (d2, p2) = reserve_dest(&dir, "movie.mp4").unwrap();
        assert_ne!(d1, d2);
        assert_ne!(p1, p2);

        // Existing partial alone blocks basename.
        let alone = dir.join("alone.bin");
        let alone_partial = partial_path(&alone);
        File::create(&alone_partial).unwrap();
        let (d3, _) = reserve_dest(&dir, "alone.bin").unwrap();
        assert_ne!(d3, alone);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reserve_dest_partial_fail_rolls_back_dest() {
        // Simulate by reserving normally then verifying cleanup helper.
        let dir = std::env::temp_dir().join(format!("jotain-fsutil-rb-{}", uuid_like()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("x.dat");
        // Create dest only (like step1), then call try path via cleanup.
        File::create(&dest).unwrap();
        // Force partial create_new to fail by pre-creating partial.
        let partial = partial_path(&dest);
        File::create(&partial).unwrap();
        let err = try_create_reservation(&dest);
        assert!(err.is_err());
        // dest was re-created? try_create_reservation create_new on dest fails if exists.
        // Instead: delete dest, create dest via create_new, partial pre-exists.
        let _ = fs::remove_file(&dest);
        let _ = fs::remove_file(&partial);
        File::create(&partial).unwrap(); // partial exists → step2 fails after step1
        // Actually step1 create_new dest ok, step2 create_new partial fails → dest rolled back.
        let r = try_create_reservation(&dest);
        assert!(r.is_err());
        assert!(!dest.exists(), "dest placeholder must be rolled back");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_partial_truncates_dirty_tail() {
        let dir = std::env::temp_dir().join(format!("jotain-fsutil-align-{}", uuid_like()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.partial");
        let mut f = File::create(&p).unwrap();
        let n = (CHUNK + 100) as usize;
        f.write_all(&vec![1u8; n]).unwrap();
        drop(f);
        let aligned = prepare_partial_for_resume(&p).unwrap();
        assert_eq!(aligned, CHUNK);
        assert_eq!(fs::metadata(&p).unwrap().len(), CHUNK);
        let _ = fs::remove_dir_all(&dir);
    }

    fn uuid_like() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
    }
}
