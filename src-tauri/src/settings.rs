//! 配置与常量——对齐 v4 `config.py`
//!
//! settings.json 只读不写回（v4 明确约束：程序不污染用户配置，`auto_emu` 不持久化）。

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::util;

pub const EMU_ADDR: &str = "127.0.0.1:5555";
pub const REMOTE_DIR: &str = "/data/local/tmp";
pub const ANDROWS_SVC: &str = "AndrowsSvr";
pub const MAX_WORKERS: usize = 3;
pub const DEFAULT_MAX_APK_MB: u64 = 2048;
pub const CACHE_VERSION: i64 = 1;

#[derive(Clone, Debug)]
pub struct Settings {
    pub androws_root: Option<PathBuf>,
    pub adb: Option<PathBuf>,
    pub aapt: Option<PathBuf>,
    pub max_apk_mb: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            androws_root: None,
            adb: None,
            aapt: None,
            max_apk_mb: DEFAULT_MAX_APK_MB,
        }
    }
}

pub fn settings_path() -> PathBuf {
    util::local_dir().join("settings.json")
}

fn as_path(v: Option<&Value>) -> Option<PathBuf> {
    v.and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// 读取 settings.json；文件缺失/损坏一律返回默认值，绝不阻塞启动
pub fn load() -> Settings {
    let mut s = Settings::default();
    let text = match std::fs::read_to_string(settings_path()) {
        Ok(t) => t,
        Err(_) => return s,
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            util::file_log(&format!("settings.json 解析失败，回退默认配置：{}", e));
            return s;
        }
    };
    s.androws_root = as_path(v.get("androws_root"));
    s.adb = as_path(v.get("adb"));
    s.aapt = as_path(v.get("aapt"));
    if let Some(n) = v.get("max_apk_mb").and_then(|x| x.as_u64()) {
        if n >= 1 {
            s.max_apk_mb = n;
        }
    }
    s
}

pub fn max_apk_bytes(s: &Settings) -> u64 {
    s.max_apk_mb.saturating_mul(1024 * 1024)
}

/// 应用宝模拟器安装根：settings → 常见盘符下的 Tencent\Androws
pub fn find_androws_root(s: &Settings) -> Option<PathBuf> {
    if let Some(ref r) = s.androws_root {
        if r.is_dir() {
            return Some(r.clone());
        }
    }
    let prefixes = [
        r"H:\Program Files\Tencent\Androws",
        r"C:\Program Files\Tencent\Androws",
        r"C:\Program Files (x86)\Tencent\Androws",
        r"D:\Program Files\Tencent\Androws",
        r"E:\Program Files\Tencent\Androws",
        r"D:\ProgramFiles\Tencent\Androws",
        r"E:\软件安装文件夹\Tencent\Androws",
    ];
    for p in prefixes {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return Some(pb);
        }
    }
    // 逐盘扫描 Tencent\Androws（只扫根下两层，避免拖慢启动）
    for d in ["C:", "D:", "E:", "F:", "G:", "H:"] {
        let cand = Path::new(d).join("Tencent").join("Androws");
        if cand.is_dir() {
            return Some(cand);
        }
    }
    None
}

/// 在应用宝根下找版本目录下的可执行文件（Application\<ver>\<name>.exe）
pub fn find_in_app_dirs(root: &Path, exe: &str) -> Option<PathBuf> {
    let app = root.join("Application");
    let mut worst: Option<(u64, PathBuf)> = None;
    if let Ok(rd) = std::fs::read_dir(&app) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let f = p.join(exe);
            if f.is_file() {
                // 取目录名最大（版本号最大）的那份
                let key = e
                    .file_name()
                    .to_string_lossy()
                    .chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u64>()
                    .unwrap_or(0);
                if worst.as_ref().map(|(k, _)| key > *k).unwrap_or(true) {
                    worst = Some((key, f));
                }
            }
        }
    }
    worst.map(|(_, p)| p)
}
