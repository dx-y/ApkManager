//! adb / aapt 依赖探测——对齐 v4 `deps.py`
//!
//! 探测优先顺序（经验值，应用宝模拟器自带 adb 最省事）：
//! settings.json → 环境变量 ANDROID_HOME / ANDROID_SDK_ROOT → 应用宝版本目录 →
//! scoop / 常见 SDK 落点 → PATH。命中后必须跑 `adb version` 真校验，防止拿到坏 exe。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::settings::{self, Settings};
use crate::util;

pub struct Deps {
    pub adb: Option<PathBuf>,
    pub aapt: Option<PathBuf>,
    pub androws_root: Option<PathBuf>,
    pub androws_exe: Option<PathBuf>,
    pub notes: Vec<String>,
}

fn exists_file(p: &Path) -> bool {
    p.is_file()
}

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    if !v.iter().any(|x| x.to_string_lossy().eq_ignore_ascii_case(&p.to_string_lossy())) {
        v.push(p);
    }
}

fn env_paths(name: &str) -> Vec<PathBuf> {
    std::env::var(name)
        .ok()
        .map(|s| vec![PathBuf::from(s)])
        .unwrap_or_default()
}

fn adb_candidates(s: &Settings, androws_root: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(ref p) = s.adb {
        push_unique(&mut v, p.clone());
    }
    for base in env_paths("ANDROID_HOME")
        .into_iter()
        .chain(env_paths("ANDROID_SDK_ROOT"))
        .chain(env_paths("ANDROID_SDK"))
    {
        push_unique(&mut v, base.join("platform-tools").join("adb.exe"));
    }
    if let Some(root) = androws_root {
        // 应用宝：把 adb.exe 放在具体版本目录里（不是 platform-tools）
        if let Some(app) = root.join("Application").to_str().map(PathBuf::from) {
            if let Ok(rd) = std::fs::read_dir(&app) {
                let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
                dirs.sort();
                dirs.reverse();
                for d in dirs {
                    push_unique(&mut v, d.join("adb.exe"));
                }
            }
        }
        push_unique(&mut v, root.join("adb.exe"));
    }
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let candidates = [
        format!("{}\\scoop\\apps\\adb\\current\\platform-tools\\adb.exe", home),
        format!("{}\\AppData\\Local\\Android\\Sdk\\platform-tools\\adb.exe", home),
        r"C:\platform-tools\adb.exe".to_string(),
        r"D:\platform-tools\adb.exe".to_string(),
        r"C:\Android\platform-tools\adb.exe".to_string(),
        r"D:\Android\android-sdk\platform-tools\adb.exe".to_string(),
        r"H:\Program Files\Tencent\Androws\adb.exe".to_string(),
    ];
    for c in candidates {
        push_unique(&mut v, PathBuf::from(c));
    }
    // PATH 里的 adb
    if let Ok(path) = std::env::var("PATH") {
        for part in path.split(';') {
            if part.trim().is_empty() {
                continue;
            }
            let p = Path::new(part).join("adb.exe");
            if exists_file(&p) {
                push_unique(&mut v, p);
            }
        }
    }
    v
}

fn aapt_candidates(s: &Settings, androws_root: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(ref p) = s.aapt {
        push_unique(&mut v, p.clone());
    }
    for base in env_paths("ANDROID_HOME")
        .into_iter()
        .chain(env_paths("ANDROID_SDK_ROOT"))
    {
        let bt = base.join("build-tools");
        if let Ok(rd) = std::fs::read_dir(&bt) {
            let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            dirs.reverse();
            for d in dirs {
                push_unique(&mut v, d.join("aapt.exe"));
            }
        }
    }
    if let Some(root) = androws_root {
        if let Ok(rd) = std::fs::read_dir(root.join("Application")) {
            for e in rd.flatten() {
                push_unique(&mut v, e.path().join("aapt.exe"));
            }
        }
    }
    v
}

/// `adb version` 真校验：返回含 Android Debug Bridge 且退出码 0
pub fn adb_works(p: &Path) -> bool {
    let o = util::run(
        &p.to_string_lossy(),
        &["version".to_string()],
        Duration::from_secs(12),
    );
    o.code == 0 && o.out.contains("Android Debug Bridge")
}

pub fn detect(s: &Settings) -> Deps {
    let androws_root = settings::find_androws_root(s);
    let mut notes = Vec::new();

    let mut adb = None;
    for c in adb_candidates(s, androws_root.as_deref()) {
        if exists_file(&c) && adb_works(&c) {
            adb = Some(c);
            break;
        }
    }
    if adb.is_none() {
        notes.push("adb 缺失，APK 安装/卸载与包名解析都不可用".to_string());
    }

    let mut aapt = None;
    for c in aapt_candidates(s, androws_root.as_deref()) {
        if exists_file(&c) {
            aapt = Some(c);
            break;
        }
    }
    if aapt.is_none() {
        // 本机应用宝 adb 包里没有 aapt.exe：走自解析 manifest 兜底，不当作错误
        notes.push("未找到 aapt，包名解析走内置 AndroidManifest 解析".to_string());
    }

    let androws_exe = androws_root
        .as_deref()
        .and_then(|r| settings::find_in_app_dirs(r, "Androws.exe"));

    Deps {
        adb,
        aapt,
        androws_root,
        androws_exe,
        notes,
    }
}

/// 设备连接检查（deep=false 只 echo ok，deep=true 额外等 sys.boot_completed）
pub fn adb_check(adb: Option<&PathBuf>, deep: bool) -> (bool, String) {
    let adb = match adb {
        Some(a) => a,
        None => return (false, "未找到 adb".to_string()),
    };
    let dev = settings::EMU_ADDR.to_string();
    let o = util::run(
        &adb.to_string_lossy(),
        &[
            "-s".to_string(),
            dev.clone(),
            "shell".to_string(),
            "echo".to_string(),
            "ok".to_string(),
        ],
        Duration::from_secs(if deep { 12 } else { 8 }),
    );
    if o.code != 0 || !o.all().contains("ok") {
        return (false, "模拟器离线".to_string());
    }
    if deep {
        let b = util::run(
            &adb.to_string_lossy(),
            &[
                "-s".to_string(),
                dev,
                "shell".to_string(),
                "getprop".to_string(),
                "sys.boot_completed".to_string(),
            ],
            Duration::from_secs(10),
        );
        if b.code == 0 && !b.out.trim().contains('1') {
            return (false, "模拟器正在启动".to_string());
        }
    }
    (true, "模拟器在线".to_string())
}
