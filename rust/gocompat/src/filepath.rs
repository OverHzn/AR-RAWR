//! Faithful port of Go's `path/filepath` (Unix) subset — `Clean`, `Rel`, `Dir`,
//! `IsAbs`, `Abs`, `Join`, plus `os.TempDir`/`os.Executable`. Unix semantics only.

#![allow(clippy::result_unit_err)]

use std::env;

// path/filepath (Unix)
// ===========================================================================

/// Go's `os.TempDir()` on Unix: $TMPDIR (if non-empty) else "/tmp".
pub fn temp_dir() -> String {
    match env::var("TMPDIR") {
        Ok(v) if !v.is_empty() => v,
        _ => "/tmp".to_string(),
    }
}

/// Go's `path/filepath.Clean` (Unix).
pub fn path_clean(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let rooted = path.starts_with('/');
    let b = path.as_bytes();
    let n = b.len();
    let mut out: Vec<u8> = Vec::with_capacity(n + 1);
    let mut r = 0usize;
    let mut dotdot = 0usize;
    if rooted {
        out.push(b'/');
        r = 1;
        dotdot = 1;
    }
    while r < n {
        if b[r] == b'/' || (b[r] == b'.' && (r + 1 == n || b[r + 1] == b'/')) {
            // Skip a separator or a "." element.
            r += 1;
        } else if b[r] == b'.' && b[r + 1] == b'.' && (r + 2 == n || b[r + 2] == b'/') {
            r += 2;
            if out.len() > dotdot {
                let mut w = out.len() - 1;
                while w > dotdot && out[w] != b'/' {
                    w -= 1;
                }
                out.truncate(w);
            } else if !rooted {
                if !out.is_empty() {
                    out.push(b'/');
                }
                out.push(b'.');
                out.push(b'.');
                dotdot = out.len();
            }
        } else {
            if (rooted && out.len() != 1) || (!rooted && !out.is_empty()) {
                out.push(b'/');
            }
            while r < n && b[r] != b'/' {
                out.push(b[r]);
                r += 1;
            }
        }
    }
    if out.is_empty() {
        return ".".to_string();
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Go's `path/filepath.Rel` (Unix).
pub fn path_rel(basepath: &str, targpath: &str) -> Result<String, ()> {
    let base = path_clean(basepath);
    let targ = path_clean(targpath);
    if targ == base {
        return Ok(".".to_string());
    }
    let base = base.as_str();
    let targ = targ.as_str();
    let base_slashed = base.starts_with('/');
    let targ_slashed = targ.starts_with('/');
    if base_slashed != targ_slashed {
        return Err(());
    }
    let bb = base.as_bytes();
    let tb = targ.as_bytes();
    let bl = bb.len();
    let tl = tb.len();
    let (mut b0, mut bi, mut t0, mut ti) = (0usize, 0usize, 0usize, 0usize);
    loop {
        while bi < bl && bb[bi] != b'/' {
            bi += 1;
        }
        while ti < tl && tb[ti] != b'/' {
            ti += 1;
        }
        if targ[t0..ti] != base[b0..bi] {
            break;
        }
        if bi < bl {
            bi += 1;
        }
        if ti < tl {
            ti += 1;
        }
        b0 = bi;
        t0 = ti;
    }
    if &base[b0..bi] == ".." {
        return Err(());
    }
    if b0 != bl {
        // Base elements left: go up before going down.
        let seps = base[b0..bl].bytes().filter(|&c| c == b'/').count();
        let mut buf = String::from("..");
        for _ in 0..seps {
            buf.push_str("/..");
        }
        if t0 != tl {
            buf.push('/');
            buf.push_str(&targ[t0..]);
        }
        return Ok(buf);
    }
    Ok(targ[t0..].to_string())
}

/// Go's `path/filepath.Dir` (Unix) = Clean(path[:lastSlash]).
pub fn path_dir(path: &str) -> String {
    let vol_end = 0usize;
    let mut i = path.len();
    // Strip trailing separators.
    while i > vol_end && !path.is_char_boundary(i) {
        i -= 1;
    }
    let bytes = path.as_bytes();
    while i > vol_end && bytes.get(i - 1) == Some(&b'/') {
        i -= 1;
    }
    // Scan backward over element.
    while i > vol_end && bytes.get(i - 1) != Some(&b'/') {
        i -= 1;
    }
    // Skip separators.
    while i > vol_end && bytes.get(i - 1) == Some(&b'/') {
        i -= 1;
    }
    if i == vol_end {
        if path.starts_with('/') {
            return "/".to_string();
        }
        return ".".to_string();
    }
    // i>0: cut trailing separator.
    path_clean(&path[..i])
}

/// Go's `path/filepath.IsAbs` (Unix) = strings.HasPrefix(path, "/").
pub fn path_is_abs(path: &str) -> bool {
    path.starts_with('/')
}

/// Go's `path/filepath.Abs` (Unix).
pub fn path_abs(path: &str) -> Result<String, ()> {
    if path_is_abs(path) {
        return Ok(path_clean(path));
    }
    let wd = env::current_dir().map_err(|_| ())?;
    let wd = wd.to_string_lossy().into_owned();
    Ok(path_clean(&format!("{wd}/{path}")))
}

/// Go's `path/filepath.Join(elem...)` (Unix).
pub fn path_join(elems: &[&str]) -> String {
    let mut size: isize = 0;
    for e in elems {
        size += e.len() as isize;
    }
    if size == 0 {
        return String::new();
    }
    let mut buf = String::with_capacity(size as usize);
    for (i, e) in elems.iter().enumerate() {
        if i > 0 && !buf.is_empty() && !buf.ends_with('/') {
            buf.push('/');
        }
        buf.push_str(e);
    }
    path_clean(&buf)
}

/// Go's `os.Executable()` → `std::env::current_exe()`.
pub fn os_executable() -> Result<String, String> {
    env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| "executable path unavailable".to_string())
}

/// Go `path.Base` (the slash-based `path` package, not `filepath`): the last
/// element of `path`, with trailing slashes removed. Empty -> "."; all-slashes
/// -> "/".
pub fn path_base(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let mut p = path;
    while !p.is_empty() && p.ends_with('/') {
        p = &p[..p.len() - 1];
    }
    if let Some(i) = p.rfind('/') {
        p = &p[i + 1..];
    }
    if p.is_empty() {
        return "/".to_string();
    }
    p.to_string()
}
