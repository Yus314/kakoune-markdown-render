use std::path::PathBuf;
use std::os::unix::fs::PermissionsExt;

/// FNV-1a ハッシュ（64bit）。クレート全体で使用するため pub(crate)。
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn session_hash(session: &str) -> String {
    format!("{:016x}", fnv1a(session.as_bytes()))
}

/// daemon ソケットのパスを返す。
/// 優先: $XDG_RUNTIME_DIR/mkdr/<hash>/daemon.sock（tmpfs、0700、systemd管理）
/// 次点: /tmp/mkdr-<uid>/<hash>/daemon.sock（0700 で作成）
pub fn socket_path(session: &str) -> PathBuf {
    let hash = session_hash(session);
    let base = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/tmp/mkdr-{}", get_uid())));
    base.join("mkdr").join(&hash).join("daemon.sock")
}

/// ディレクトリを 0700 で作成（セキュリティ上必須）。
pub fn ensure_session_dir(session: &str) -> anyhow::Result<PathBuf> {
    let path = socket_path(session)
        .parent()
        .expect("socket_path has parent")
        .to_owned();
    std::fs::create_dir_all(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

#[cfg(unix)]
fn get_uid() -> u32 {
    // SAFETY: getuid() は常に成功し副作用もない
    unsafe { libc::getuid() }
}
