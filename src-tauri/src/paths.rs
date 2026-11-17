//! 拖放/选择路径归一化 + 快捷方式处理——对齐 v4 `paths.py` / `shortcuts.py`
//!
//! 覆盖的真实场景：正常 .apk、微信/QQ 下载被改名为 `xxx.apk.1`、桌面快捷方式 .lnk、
//! 浏览器拖出的 `file:///`、以及带引号/空格的路径。

use std::path::{Path, PathBuf};

fn is_apk_name(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.ends_with(".apk")
}

fn is_apk_n(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    if !l.contains(".apk.") {
        return false;
    }
    let tail = l.rsplit(".apk.").next().unwrap_or("");
    !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())
}

fn is_lnk(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".lnk")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(h) = h {
                if let Ok(v) = u8::from_str_radix(h, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 清理首尾引号/空白、file:// 前缀、URL 解码
pub fn clean_dropped(p: &str) -> String {
    let mut s = p.trim().trim_matches('"').trim_matches('\'').trim().to_string();
    if s.is_empty() {
        return s;
    }
    let low = s.to_ascii_lowercase();
    if low.starts_with("file:///") {
        s = percent_decode(&s[8..]);
        s = s.replace('/', "\\");
    } else if low.starts_with("file://") {
        s = percent_decode(&s[7..]);
        s = s.replace('/', "\\");
    }
    s.trim().trim_matches('"').trim_matches('\'').trim().to_string()
}

/// 把拖入/选中的路径规整为可交给安装流程的路径：
/// 1) *.apk → 原样；2) *.apk.N → 复制成临时 .apk；3) *.lnk → 解析目标或包名；4) 其余原样
pub fn normalize(p: &str) -> String {
    let path = Path::new(p);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    if is_apk_name(&name) {
        return p.to_string();
    }
    if is_apk_n(&name) && path.is_file() {
        let dst = std::env::temp_dir().join(format!("apkm_drop_{}.apk", hash_name(p)));
        match std::fs::copy(path, &dst) {
            Ok(_) => return dst.to_string_lossy().to_string(),
            Err(e) => {
                crate::util::file_log(&format!("临时副本创建失败 {}：{}", p, e));
                return p.to_string();
            }
        }
    }
    if is_lnk(&name) && path.is_file() {
        if let Some((target, args)) = parse_lnk(path) {
            if is_apk_name(&target) && Path::new(&target).is_file() {
                return target;
            }
            if let Some(pkg) = extract_pkg(&format!("{} {}", target, args)) {
                return format!("pkg://{}", pkg);
            }
        }
        return p.to_string();
    }
    p.to_string()
}

fn hash_name(s: &str) -> String {
    let mut h: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{:x}", h & 0xff_ffff)
}

pub fn is_tmp_drop_copy(p: &str) -> bool {
    let path = Path::new(p);
    let in_temp = path
        .parent()
        .map(|d| d == std::env::temp_dir())
        .unwrap_or(false);
    let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    in_temp && name.starts_with("apkm_drop_")
}

/// 兜底解析：找不到就按文件名在常见目录里找一次（WebView/浏览器沙箱有时不给真路径）
pub fn resolve_dropped(p: &str) -> String {
    let s = clean_dropped(p);
    if s.is_empty() {
        return s;
    }
    if Path::new(&s).is_file() {
        return s;
    }
    let base = Path::new(&s)
        .file_name()
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_default();
    if !(is_apk_name(&base) || is_apk_n(&base) || is_lnk(&base)) {
        return s;
    }
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let bases = [
        format!("{}\\Desktop", home),
        format!("{}\\Downloads", home),
        format!("{}\\Documents", home),
        home.clone(),
        std::env::var("TEMP").unwrap_or_default(),
        std::env::var("LOCALAPPDATA").unwrap_or_default(),
    ];
    for b in bases {
        if b.is_empty() || !Path::new(&b).is_dir() {
            continue;
        }
        if let Some(f) = walk_find(Path::new(&b), &base, 4) {
            return f.to_string_lossy().to_string();
        }
    }
    s
}

fn walk_find(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let rd = std::fs::read_dir(dir).ok()?;
    let target = name.to_ascii_lowercase();
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let fname = e.file_name().to_string_lossy().to_ascii_lowercase();
        if p.is_file() {
            if fname == target {
                return Some(p);
            }
        } else if p.is_dir() {
            subdirs.push(p);
        }
    }
    for d in subdirs {
        if let Some(f) = walk_find(&d, name, depth - 1) {
            return Some(f);
        }
    }
    None
}

// ------------------------------------------------------------------ .lnk 解析

/// 解析 .lnk：返回 (本地目标路径, 启动参数)。纯二进制解析 MS-SHLLINK，不依赖 COM。
pub fn parse_lnk(p: &Path) -> Option<(String, String)> {
    let data = std::fs::read(p).ok()?;
    if data.len() < 76 {
        return None;
    }
    let header_size = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
    if header_size != 0x4C {
        return None;
    }
    let flags = u32::from_le_bytes(data[20..24].try_into().ok()?);
    let has_idlist = flags & 0x1 != 0;
    let has_linkinfo = flags & 0x2 != 0;
    let is_unicode = flags & 0x80 != 0;
    let has_args = flags & 0x20 != 0;

    let mut off = header_size;
    if has_idlist {
        if off + 2 > data.len() {
            return None;
        }
        let idl_size = u16::from_le_bytes(data[off..off + 2].try_into().ok()?) as usize;
        off = off.checked_add(2 + idl_size)?;
    }
    if has_linkinfo {
        if off + 4 > data.len() {
            return None;
        }
        let li_end = off + u32::from_le_bytes(data[off..off + 4].try_into().ok()?) as usize;
        let li = &data[off..li_end.min(data.len())];
        if li.len() >= 28 {
            let li_header = u32::from_le_bytes(li[4..8].try_into().ok()?) as usize;
            let li_flags = u32::from_le_bytes(li[8..12].try_into().ok()?);
            let local_off = u32::from_le_bytes(li[16..20].try_into().ok()?) as usize;
            let suffix_off = u32::from_le_bytes(li[24..28].try_into().ok()?) as usize;
            let uni_local_off = if li_header >= 0x24 && li.len() >= 32 {
                Some(u32::from_le_bytes(li[28..32].try_into().ok()?) as usize)
            } else {
                None
            };
            let mut local = String::new();
            let uni = li_flags & 0x1 != 0 && is_unicode;
            if uni {
                if let Some(u) = uni_local_off {
                    local = read_utf16z(li, u);
                }
            }
            if local.is_empty() && local_off > 0 {
                local = read_ansiz(li, local_off);
            }
            let mut suffix = String::new();
            if suffix_off > 0 {
                suffix = if uni {
                    read_utf16z(li, suffix_off)
                } else {
                    read_ansiz(li, suffix_off)
                };
            }
            if !local.is_empty() {
                let target = if local.contains(':') {
                    format!("{}{}", local, suffix)
                } else {
                    format!("{}{}", local, suffix)
                };
                let args = if has_args {
                    read_string_data(&data, off + li.len(), is_unicode).unwrap_or_default()
                } else {
                    String::new()
                };
                return Some((target, args));
            }
        }
    }
    None
}

fn read_ansiz(buf: &[u8], off: usize) -> String {
    if off >= buf.len() {
        return String::new();
    }
    let end = buf[off..]
        .iter()
        .position(|&b| b == 0)
        .map(|i| off + i)
        .unwrap_or(buf.len());
    crate::util::decode(&buf[off..end])
}

fn read_utf16z(buf: &[u8], off: usize) -> String {
    if off + 1 >= buf.len() {
        return String::new();
    }
    let mut u: Vec<u16> = Vec::new();
    let mut i = off;
    while i + 1 < buf.len() {
        let v = u16::from_le_bytes([buf[i], buf[i + 1]]);
        if v == 0 {
            break;
        }
        u.push(v);
        i += 2;
    }
    String::from_utf16_lossy(&u)
}

fn read_string_data(buf: &[u8], off: usize, unicode: bool) -> Option<String> {
    if off + 2 > buf.len() {
        return None;
    }
    let chars = u16::from_le_bytes(buf[off..off + 2].try_into().ok()?) as usize;
    let start = off + 2;
    if unicode {
        let bytes = chars.checked_mul(2)?;
        if start + bytes > buf.len() {
            return None;
        }
        let mut u = Vec::with_capacity(chars);
        for i in 0..chars {
            let o = start + i * 2;
            u.push(u16::from_le_bytes([buf[o], buf[o + 1]]));
        }
        Some(String::from_utf16_lossy(&u))
    } else {
        if start + chars > buf.len() {
            return None;
        }
        Some(crate::util::decode(&buf[start..start + chars]))
    }
}

/// 从文本里嗅探安卓包名（`pkg=/package=` 显式标注优先，其次形如 com.x.y 的串）
pub fn extract_pkg(text: &str) -> Option<String> {
    let low = text.to_ascii_lowercase();
    for key in ["package=", "pkg=", "package:", "/pkg/"] {
        if let Some(i) = low.find(key) {
            let rest = &text[i + key.len()..];
            let cand: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_')
                .collect();
            if valid_pkg(&cand) {
                return Some(cand);
            }
        }
    }
    for tok in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_')) {
        if valid_pkg(tok) && tok.split('.').count() >= 2 {
            return Some(tok.to_string());
        }
    }
    None
}

pub fn valid_pkg(s: &str) -> bool {
    s.len() >= 5
        && s.len() <= 80
        && s.contains('.')
        && s.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
        && !s.ends_with('.')
        && s.split('.').all(|p| !p.is_empty())
}

// ------------------------------------------------------- 桌面/开始菜单快捷方式清理

pub fn desktop_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(up) = std::env::var("USERPROFILE") {
        v.push(Path::new(&up).join("Desktop"));
    }
    if let Ok(pd) = std::env::var("PUBLIC") {
        v.push(Path::new(&pd).join("Desktop"));
    }
    v
}

/// 卸载后清掉指向该包的桌面快捷方式（名称前缀通常是包名或应用名）
pub fn remove_shortcuts_for(pkg: &str, app_label: Option<&str>) -> Vec<String> {
    let mut removed = Vec::new();
    for dir in desktop_dirs() {
        if !dir.is_dir() {
            continue;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() || !is_lnk(&p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()) {
                continue;
            }
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let mut hit = stem.eq_ignore_ascii_case(pkg);
            if let Some(label) = app_label {
                if !label.is_empty() && stem.eq_ignore_ascii_case(label) {
                    hit = true;
                }
            }
            if !hit {
                if let Some((_, args)) = parse_lnk(&p) {
                    if args.to_ascii_lowercase().contains(&pkg.to_ascii_lowercase()) {
                        hit = true;
                    }
                }
            }
            if hit {
                if std::fs::remove_file(&p).is_ok() {
                    removed.push(stem);
                }
            }
        }
    }
    removed
}
