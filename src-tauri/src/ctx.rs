//! 全局运行态与前端推送——对齐 v4 `app.py` 的 js()/log()/task()/prog() 系列

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::settings::{Settings, EMU_ADDR};
use crate::util;

pub struct Ctx {
    pub app: AppHandle,
    pub settings: Settings,
    pub adb: Mutex<Option<PathBuf>>,
    pub aapt: Mutex<Option<PathBuf>>,
    pub androws_root: Mutex<Option<PathBuf>>,
    pub androws_exe: Mutex<Option<PathBuf>>,
    pub cache: Mutex<HashMap<String, String>>,
    pub auto_open: AtomicBool,
    pub auto_emu: AtomicBool,
    /// 模拟器启动流程进行中
    pub booting: AtomicBool,
    /// 应用名解析进行中（去重，避免重复解析同一批包）
    pub names_busy: Mutex<HashSet<String>>,
}

impl Ctx {
    pub fn new(app: AppHandle, settings: Settings) -> Self {
        Self {
            app,
            settings,
            adb: Mutex::new(None),
            aapt: Mutex::new(None),
            androws_root: Mutex::new(None),
            androws_exe: Mutex::new(None),
            cache: Mutex::new(HashMap::new()),
            auto_open: AtomicBool::new(false),
            auto_emu: AtomicBool::new(false),
            booting: AtomicBool::new(false),
            names_busy: Mutex::new(HashSet::new()),
        }
    }

    pub fn emit(&self, ev: &str, v: serde_json::Value) {
        if let Err(e) = self.app.emit(ev, v) {
            util::file_log(&format!("事件推送失败 {}：{}", ev, e));
        }
    }

    pub fn log(&self, text: &str, cls: &str) {
        util::file_log(text);
        self.emit("ui:log", json!({ "text": text, "cls": cls }));
    }

    pub fn adb(&self) -> Option<PathBuf> {
        self.adb.lock().ok().and_then(|g| g.clone())
    }

    pub fn push_adb(&self, ok: bool, msg: &str) {
        self.emit(
            "ui:adb",
            json!({ "ok": ok, "msg": msg, "addr": EMU_ADDR }),
        );
    }

    /// 任务卡推送：state = add / running / done / err
    pub fn task(&self, kind: &str, id: &str, name: &str, pct: i32, state: &str, status: &str) {
        self.emit(
            "ui:task",
            json!({
                "kind": kind,
                "id": id,
                "name": name,
                "pct": pct.clamp(0, 100),
                "state": state,
                "status": status
            }),
        );
    }

    /// 单文件进度条：state = loading / bar / idle
    pub fn prog(&self, kind: &str, state: &str, text: &str, pct: i32) {
        self.emit(
            "ui:prog",
            json!({ "kind": kind, "state": state, "text": text, "pct": pct.clamp(0, 100) }),
        );
    }

    pub fn badge(&self, which: &str, text: &str, cls: &str) {
        self.emit("ui:badge", json!({ "which": which, "text": text, "cls": cls }));
    }

    pub fn toast(&self, html: &str, kind: &str) {
        self.emit("ui:toast", json!({ "html": html, "kind": kind }));
    }

    pub fn push_app_names(&self, names: &HashMap<String, String>) {
        if names.is_empty() {
            return;
        }
        self.emit("ui:appnames", json!({ "names": names }));
    }

    pub fn is_booting(&self) -> bool {
        self.booting.load(Ordering::SeqCst)
    }

    pub fn set_booting(&self, v: bool) {
        self.booting.store(v, Ordering::SeqCst);
    }
}
