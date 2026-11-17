//! 前端可调用命令——对齐 v4 `api.py` 的 8 个暴露方法
//! 前端沿用 `window.pywebview.api.*`，由 `ui/shim.js` 转到 `__TAURI__.core.invoke`。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::State;

use crate::appnames;
use crate::ctx::Ctx;
use crate::deps;
use crate::paths as p2;
use crate::settings::EMU_ADDR;
use crate::tasks::{self, AppItem, Pool};
use crate::util;

pub struct AppState {
    pub ctx: Arc<Ctx>,
    pub pool: Arc<Pool>,
}

fn esc(s: &str) -> String {
    util::escape(s)
}

/// 重新探测 adb / aapt，并刷新状态灯
#[tauri::command(async)]
pub fn refresh_adb(state: State<'_, AppState>) -> Value {
    let ctx = state.ctx.clone();
    let (has_adb, has_aapt) = tasks::refresh_deps(&ctx);
    let (ok, msg) = deps::adb_check(ctx.adb().as_ref(), false);
    ctx.push_adb(ok, &msg);
    if has_adb {
        match ctx.adb() {
            Some(a) => ctx.log(&format!("adb 已就绪：{}", a.display()), "ok"),
            None => ctx.log("adb 未就绪", "err"),
        }
    } else {
        ctx.log("未找到 adb，请确认应用宝模拟器已安装", "err");
        ctx.toast("未找到 <b>adb</b>，请检查模拟器安装路径", "err");
    }
    ctx.log(
        if has_aapt { "aapt 已找到" } else { "未找到 aapt，改用内置 APK 解析" },
        if has_aapt { "ok" } else { "warn" },
    );
    json!({ "ok": ok, "msg": msg, "has_adb": has_adb, "has_aapt": has_aapt })
}

/// 弹系统文件选择框；选完直接进入流程（前端忽略返回值）
#[tauri::command(async)]
pub fn pick(state: State<'_, AppState>, kind: String) -> Vec<String> {
    let title = if kind == "uninstall" {
        "选择要卸载的应用对应的 APK"
    } else {
        "选择要安装的 APK"
    };
    let picked = rfd::FileDialog::new()
        .set_title(title)
        .add_filter("APK", &["apk"])
        .add_filter("所有文件", &["*"])
        .pick_files();
    let files: Vec<String> = match picked {
        Some(v) => v
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        None => return Vec::new(),
    };
    if files.is_empty() {
        return files;
    }
    if kind == "uninstall" {
        tasks::submit_uninstall(&state.ctx, &state.pool, files.clone());
    } else {
        tasks::submit_install(&state.ctx, &state.pool, files.clone(), true);
    }
    files
}

/// 拖放 / 按钮统一入口
#[tauri::command(async)]
pub fn start_task(state: State<'_, AppState>, kind: String, paths: Vec<String>) {
    if paths.is_empty() {
        state.ctx.toast("没有收到文件", "err");
        return;
    }
    let resolved: Vec<String> = paths
        .iter()
        .map(|p| {
            if Path::new(p).is_file() {
                p.clone()
            } else {
                p2::resolve_dropped(p)
            }
        })
        .collect();
    if kind == "uninstall" {
        tasks::submit_uninstall(&state.ctx, &state.pool, resolved);
    } else {
        tasks::submit_install(&state.ctx, &state.pool, resolved, false);
    }
}

#[tauri::command(async)]
pub fn start_emulator(state: State<'_, AppState>) {
    tasks::boot_emulator(&state.ctx, false);
}

#[tauri::command]
pub fn set_auto_open(state: State<'_, AppState>, value: bool) {
    state.ctx.auto_open.store(value, Ordering::SeqCst);
    state
        .ctx
        .log(&format!("安装后自动打开：{}", if value { "开" } else { "关" }), "info");
}

#[tauri::command]
pub fn set_auto_emu(state: State<'_, AppState>, value: bool) {
    state.ctx.auto_emu.store(value, Ordering::SeqCst);
    state
        .ctx
        .log(&format!("离线自动启动模拟器：{}", if value { "开" } else { "关" }), "info");
}

#[tauri::command(async)]
pub fn get_apps(state: State<'_, AppState>) -> Vec<AppItem> {
    tasks::list_apps(&state.ctx)
}

/// 应用列表里点删除
#[tauri::command(async)]
pub fn uninstall_pkg(state: State<'_, AppState>, pkg: String) -> Value {
    let ctx = state.ctx.clone();
    if !p2::valid_pkg(&pkg) {
        return json!({ "ok": false, "msg": "包名不合法" });
    }
    let adb = match ctx.adb() {
        Some(a) => a,
        None => return json!({ "ok": false, "msg": "未找到 adb" }),
    };
    if !deps::adb_check(Some(&adb), false).0 {
        ctx.push_adb(false, "模拟器离线");
        return json!({ "ok": false, "msg": "模拟器离线，请先启动" });
    }
    ctx.log(&format!("正在卸载 {}", pkg), "info");
    let o = util::run(
        &adb.to_string_lossy(),
        &[
            "-s".to_string(),
            EMU_ADDR.to_string(),
            "shell".to_string(),
            "pm".to_string(),
            "uninstall".to_string(),
            pkg.clone(),
        ],
        Duration::from_secs(120),
    );
    let all = o.all();
    if !all.contains("Success") {
        let msg = if all.contains("not installed") || all.contains("Unknown package") {
            "模拟器里没有这个应用".to_string()
        } else {
            all.lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .unwrap_or("卸载失败")
                .to_string()
        };
        ctx.log(&format!("卸载失败：{}", msg), "err");
        return json!({ "ok": false, "msg": msg });
    }
    appnames::forget(&pkg);
    let removed = p2::remove_shortcuts_for(&pkg, None);
    ctx.log(
        &format!("已卸载 {}{}", pkg, if removed.is_empty() { String::new() } else { format!("，清理快捷方式 {} 个", removed.len()) }),
        "ok",
    );
    ctx.toast(&format!("<b>{}</b> 已卸载", esc(&pkg)), "ok");
    json!({ "ok": true, "msg": "已卸载" })
}
