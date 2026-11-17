//! 应用显示名解析与缓存——对齐 v4 `appnames.py`，但把 pyaxmlparser 换成内置解析器
//!
//! 背景：本机 Androws 的 `pm list packages` / `dumpsys package` 都不给 label，只能自己
//! 从 APK 里读：`AndroidManifest.xml` 的 `<application android:label>` 拿到资源 ID，
//! 再到 `resources.arsc` 里按资源 ID 取字符串（优先中文）。
//! 全程只从设备抽这两个条目（几十 KB ~ 几 MB），不 pull 整包。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crate::settings::CACHE_VERSION;
use crate::util;

// ---------------------------------------------------------------- 缓存

pub fn cache_path() -> PathBuf {
    util::local_dir().join("app_names.json")
}

pub fn load_cache() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let text = match std::fs::read_to_string(cache_path()) {
        Ok(t) => t,
        Err(_) => return map,
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return map,
    };
    if v.get("v").and_then(|x| x.as_i64()) != Some(CACHE_VERSION) {
        return map;
    }
    if let Some(m) = v.get("m").and_then(|x| x.as_object()) {
        for (k, val) in m {
            if let Some(s) = val.as_str() {
                if !s.is_empty() {
                    map.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    map
}

/// 原子写：先写 .tmp 再 rename，避免中途崩溃留下半个文件
pub fn save_cache(map: &HashMap<String, String>) -> bool {
    let path = cache_path();
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let mut obj = serde_json::Map::new();
    for (k, v) in map {
        obj.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    let mut root = serde_json::Map::new();
    root.insert("v".to_string(), serde_json::Value::from(CACHE_VERSION));
    root.insert("m".to_string(), serde_json::Value::Object(obj));
    let text = serde_json::Value::Object(root).to_string();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text.as_bytes()).is_err() {
        return false;
    }
    std::fs::rename(&tmp, &path).is_ok()
}

pub fn forget(pkg: &str) {
    let mut map = load_cache();
    if map.remove(pkg).is_some() {
        save_cache(&map);
    }
}

// ---------------------------------------------------------------- 设备取字节

fn entry_bytes(adb: &Path, apk: &str, entry: &str) -> Vec<u8> {
    let mut args = vec![
        "-s".to_string(),
        crate::settings::EMU_ADDR.to_string(),
        "exec-out".to_string(),
        "unzip".to_string(),
        "-p".to_string(),
        apk.to_string(),
        entry.to_string(),
    ];
    // exec-out 输出是二进制，util::run 会走解码丢弃字节，所以这里必须单独走 raw 通道
    match raw_run(adb, &mut args, Duration::from_secs(90)) {
        Some(b) => b,
        None => Vec::new(),
    }
}

fn raw_run(exe: &Path, args: &mut Vec<String>, timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut c = Command::new(exe);
    c.args(args.iter())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(util::NO_WINDOW);
    }
    let mut child = c.spawn().ok()?;
    let mut so = child.stdout.take()?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        let _ = tx.send(v);
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
    rx.recv_timeout(Duration::from_secs(5)).ok().filter(|b| !b.is_empty())
}

// ---------------------------------------------------------------- 二进制解析

struct StringPool {
    offsets: Vec<u32>,
    data: Vec<u8>,
    utf8: bool,
}

fn u16at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn parse_string_pool(buf: &[u8], off: usize) -> Option<StringPool> {
    let typ = u16at(buf, off)?;
    if typ != 0x0001 {
        return None;
    }
    let header_size = u16at(buf, off + 2)? as usize;
    let count = u32at(buf, off + 8)? as usize;
    let flags = u32at(buf, off + 16)?;
    let strings_start = u32at(buf, off + 20)? as usize;
    if count == 0 || count > 200_000 {
        return None;
    }
    let base = off + strings_start;
    let end = buf.len();
    if base >= end {
        return None;
    }
    let mut offsets = Vec::with_capacity(count);
    for i in 0..count {
        let o = off + header_size + i * 4;
        offsets.push(u32at(buf, o)?);
    }
    Some(StringPool {
        offsets,
        data: buf[base..end].to_vec(),
        utf8: flags & 0x100 != 0,
    })
}

impl StringPool {
    fn get(&self, idx: usize) -> Option<String> {
        let off = *self.offsets.get(idx)? as usize;
        let d = &self.data;
        if off >= d.len() {
            return None;
        }
        if self.utf8 {
            let mut i = off;
            // 先读 UTF-16 长度（占位），再读字节长度
            let (_l16, n1) = read_len8(d, i)?;
            i += n1;
            let (len, n2) = read_len8(d, i)?;
            i += n2;
            let bytes = d.get(i..i + len)?;
            Some(String::from_utf8_lossy(bytes).into_owned())
        } else {
            let mut i = off;
            let (len, n) = read_len16(d, i)?;
            i += n;
            let mut u = Vec::with_capacity(len);
            for k in 0..len {
                let o = i + k * 2;
                u.push(u16at(d, o)?);
            }
            Some(String::from_utf16_lossy(&u))
        }
    }
}

fn read_len8(d: &[u8], i: usize) -> Option<(usize, usize)> {
    let b0 = *d.get(i)?;
    if b0 & 0x80 != 0 {
        let b1 = *d.get(i + 1)?;
        Some(((((b0 & 0x7F) as usize) << 8) | b1 as usize, 2))
    } else {
        Some((b0 as usize, 1))
    }
}

fn read_len16(d: &[u8], i: usize) -> Option<(usize, usize)> {
    let w = u16at(d, i)?;
    if w & 0x8000 != 0 {
        let w2 = u16at(d, i + 2)?;
        Some(((((w & 0x7FFF) as usize) << 16) | w2 as usize, 4))
    } else {
        Some((w as usize, 2))
    }
}

enum LabelVal {
    Text(String),
    ResId(u32),
}

/// 在二进制 AndroidManifest.xml 中定位指定 tag 的指定属性
fn axml_attr(mf: &[u8], want_tag: &str, want_attr: &str) -> Option<LabelVal> {
    if mf.len() < 8 || u16at(mf, 0)? != 0x0003 {
        return None;
    }
    let header_size = u16at(mf, 2)? as usize;
    let mut pool: Option<StringPool> = None;
    let mut off = header_size;
    let mut guard = 0;
    while off + 8 <= mf.len() && guard < 50_000 {
        guard += 1;
        let typ = u16at(mf, off)?;
        let csize = u32at(mf, off + 4)? as usize;
        if csize < 8 {
            return None;
        }
        if typ == 0x0001 {
            pool = parse_string_pool(mf, off);
        } else if typ == 0x0102 {
            let p = pool.as_ref()?;
            let ext = off + u16at(mf, off + 2)? as usize;
            let name_idx = u32at(mf, ext + 4)? as usize;
            let tag_name = p.get(name_idx).unwrap_or_default();
            if tag_name == want_tag {
                let attr_start = u16at(mf, ext + 8)? as usize;
                let attr_size = u16at(mf, ext + 10)? as usize;
                let attr_count = u16at(mf, ext + 12)? as usize;
                let step = if attr_size >= 20 { attr_size } else { 20 };
                for i in 0..attr_count {
                    let a = ext + attr_start + i * step;
                    let an = u32at(mf, a + 4)? as usize;
                    let aname = p.get(an).unwrap_or_default();
                    if aname != want_attr {
                        continue;
                    }
                    let raw = u32at(mf, a + 8)? as usize;
                    let data_type = *mf.get(a + 15)?;
                    let data = u32at(mf, a + 16)?;
                    match data_type {
                        0x03 => {
                            if let Some(s) = p.get(data as usize) {
                                return Some(LabelVal::Text(s));
                            }
                        }
                        0x01 => {
                            if data > 0 {
                                return Some(LabelVal::ResId(data));
                            }
                        }
                        _ => {
                            if raw != 0xFFFF_FFFF {
                                if let Some(s) = p.get(raw) {
                                    if !s.is_empty() {
                                        return Some(LabelVal::Text(s));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        off = off.checked_add(csize)?;
    }
    None
}

fn axml_label(mf: &[u8]) -> Option<LabelVal> {
    axml_attr(mf, "application", "label")
}

fn axml_package(mf: &[u8]) -> Option<String> {
    match axml_attr(mf, "manifest", "package")? {
        LabelVal::Text(s) => {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        }
        LabelVal::ResId(_) => None,
    }
}

/// 本地 APK 文件取包名（卸载拖入 APK 时用）
pub fn package_of_file(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let mf = zip_entry(&data, "AndroidManifest.xml")?;
    axml_package(&mf)
}

/// 设备上的 APK 取包名（安装后自动打开用）
pub fn package_of(adb: &Path, apk: &str) -> Option<String> {
    let mf = entry_bytes(adb, apk, "AndroidManifest.xml");
    if mf.is_empty() {
        return None;
    }
    axml_package(&mf)
}

fn zip_entry(data: &[u8], name: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let cur = std::io::Cursor::new(data);
    let mut z = zip::ZipArchive::new(cur).ok()?;
    let mut f = z.by_name(name).ok()?;
    let mut v = Vec::new();
    f.read_to_end(&mut v).ok()?;
    Some(v)
}

/// 在 resources.arsc 中按资源 ID 取字符串，优先中文
fn arsc_pick(arsc: &[u8], res_id: u32) -> Option<String> {
    if arsc.len() < 12 || u16at(arsc, 0)? != 0x0002 {
        return None;
    }
    let pkg_id = ((res_id >> 24) & 0xFF) as u8;
    let type_id = ((res_id >> 16) & 0xFF) as u8;
    let entry_idx = (res_id & 0xFFFF) as usize;

    let mut global: Option<StringPool> = None;
    let mut off = u16at(arsc, 2)? as usize;
    let mut best_zh: Option<String> = None;
    let mut best_default: Option<String> = None;
    let mut best_any: Option<String> = None;
    let mut guard = 0;

    while off + 8 <= arsc.len() && guard < 10_000 {
        guard += 1;
        let typ = u16at(arsc, off)?;
        let header_size = u16at(arsc, off + 2)? as usize;
        let csize = u32at(arsc, off + 4)? as usize;
        if csize < 8 {
            return None;
        }
        if typ == 0x0001 {
            global = parse_string_pool(arsc, off);
        } else if typ == 0x0200 {
            // package chunk
            let pid = u32at(arsc, off + 8)? as u8;
            let type_strings = u32at(arsc, off + 12 + 256)? as usize;
            let key_strings = u32at(arsc, off + 12 + 264)? as usize;
            let type_pool = if type_strings > 0 {
                parse_string_pool(arsc, off + type_strings)
            } else {
                None
            };
            let _ = key_strings;
            let mut inner = off + header_size;
            let pkg_end = off + csize;
            let mut guard2 = 0;
            while inner + 8 <= arsc.len() && inner < pkg_end && guard2 < 20_000 {
                guard2 += 1;
                let it = u16at(arsc, inner)?;
                let ihs = u16at(arsc, inner + 2)? as usize;
                let isz = u32at(arsc, inner + 4)? as usize;
                if isz < 8 {
                    break;
                }
                if it == 0x0201 {
                    let tid = *arsc.get(inner + 8)?;
                    let flags = *arsc.get(inner + 9)?;
                    let entry_count = u32at(arsc, inner + 12)? as usize;
                    let entries_start = u32at(arsc, inner + 16)? as usize;
                    // config locale（相对 chunk 起点 20 处开始是 ResTable_config）
                    let cfg = inner + 20;
                    let lang = arsc.get(cfg + 8..cfg + 10).unwrap_or(&[0, 0]).to_vec();
                    let is_zh = lang == b"zh".to_vec();
                    let is_default = lang == vec![0u8, 0u8];

                    if tid == type_id && (pid == pkg_id || pkg_id == 0) {
                        if let Some(entry_off) = find_entry_offset(arsc, inner + ihs, entry_count, entry_idx, flags) {
                            let eo = inner + entries_start + entry_off;
                            if let Some(v) = read_entry_value(arsc, eo) {
                                if let Some(p) = global.as_ref() {
                                    if v.0 == 0x03 {
                                        if let Some(s) = p.get(v.1 as usize) {
                                            if !s.is_empty() {
                                                if is_zh {
                                                    best_zh.get_or_insert(s);
                                                } else if is_default {
                                                    best_default.get_or_insert(s);
                                                } else {
                                                    best_any.get_or_insert(s);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // type 池只在调试时需要，这里显式忽略避免未使用告警
                    let _ = type_pool.as_ref().map(|p| p.offsets.len());
                }
                inner = inner.checked_add(isz)?;
            }
        }
        off = off.checked_add(csize)?;
    }
    best_zh.or(best_default).or(best_any)
}

/// 在 Type chunk 的 entry offsets 表里定位目标 entry 的偏移
fn find_entry_offset(
    arsc: &[u8],
    start: usize,
    entry_count: usize,
    want: usize,
    flags: u8,
) -> Option<usize> {
    if flags & 0x01 != 0 {
        // sparse：{ idx u16, off u16 }，off 以 4 字节为单位
        for i in 0..entry_count {
            let p = start + i * 4;
            let idx = u16at(arsc, p)? as usize;
            let of = u16at(arsc, p + 2)? as usize;
            if idx == want {
                return Some(of * 4);
            }
        }
        None
    } else if flags & 0x02 != 0 {
        // offset16：u16，0xFFFF 表示缺省
        let v = u16at(arsc, start + want * 2)? as usize;
        if v == 0xFFFF {
            None
        } else {
            Some(v * 4)
        }
    } else {
        if want >= entry_count {
            return None;
        }
        let v = u32at(arsc, start + want * 4)?;
        if v == 0xFFFF_FFFF {
            None
        } else {
            Some(v as usize)
        }
    }
}

/// 读 entry 的 Res_value，返回 (dataType, data)
fn read_entry_value(arsc: &[u8], eo: usize) -> Option<(u8, u32)> {
    let size = u16at(arsc, eo)? as usize;
    let eflags = u16at(arsc, eo + 2)?;
    let _key = u32at(arsc, eo + 4)?;
    let mut val = eo + size.max(8);
    if eflags & 0x0001 != 0 {
        // COMPLEX：parent + count + map，取第一项
        val += 8;
        let dt = *arsc.get(val + 3)?;
        let dd = u32at(arsc, val + 4)?;
        return Some((dt, dd));
    }
    let dt = *arsc.get(val + 3)?;
    let dd = u32at(arsc, val + 4)?;
    Some((dt, dd))
}

/// 解析单个包的应用名（失败返回 None，不抛错）
pub fn label_of(adb: &Path, apk: &str) -> Option<String> {
    let mf = entry_bytes(adb, apk, "AndroidManifest.xml");
    if mf.is_empty() {
        return None;
    }
    match axml_label(&mf)? {
        LabelVal::Text(s) => {
            let t = s.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        }
        LabelVal::ResId(id) => {
            let arsc = entry_bytes(adb, apk, "resources.arsc");
            if arsc.is_empty() {
                return None;
            }
            arsc_pick(&arsc, id).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        }
    }
}

/// 并发解析一批包名，每凑满 batch_size 回调一次（前端就地刷新，不重排列表）
pub fn fetch_batch<F: FnMut(HashMap<String, String>)>(
    adb: PathBuf,
    items: Vec<(String, String)>,
    max_workers: usize,
    batch_size: usize,
    mut on_batch: F,
) -> HashMap<String, String> {
    let items: Vec<(String, String)> = items
        .into_iter()
        .filter(|(_, apk)| !apk.trim().is_empty())
        .collect();
    if items.is_empty() {
        return HashMap::new();
    }
    let workers = max_workers.max(1).min(items.len());
    let (tx, rx) = mpsc::channel::<(String, String)>();
    let chunks: Vec<Vec<(String, String)>> = {
        let mut c: Vec<Vec<(String, String)>> = (0..workers).map(|_| Vec::new()).collect();
        for (i, it) in items.into_iter().enumerate() {
            c[i % workers].push(it);
        }
        c
    };
    for chunk in chunks {
        if chunk.is_empty() {
            continue;
        }
        let tx = tx.clone();
        let adb = adb.clone();
        std::thread::spawn(move || {
            for (pkg, apk) in chunk {
                if let Some(name) = label_of(&adb, &apk) {
                    let _ = tx.send((pkg, name));
                }
            }
        });
    }
    drop(tx);

    let mut got: HashMap<String, String> = HashMap::new();
    let mut pending: HashMap<String, String> = HashMap::new();
    let bs = batch_size.max(1);
    while let Ok((pkg, name)) = rx.recv() {
        pending.insert(pkg.clone(), name.clone());
        got.insert(pkg, name);
        if pending.len() >= bs {
            on_batch(std::mem::take(&mut pending));
        }
    }
    if !pending.is_empty() {
        on_batch(pending);
    }
    got
}
