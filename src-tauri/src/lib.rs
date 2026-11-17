mod api;
mod appnames;
mod ctx;
mod deps;
mod paths;
mod settings;
mod tasks;
mod util;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use tauri::{Emitter, Manager};

use crate::api::AppState;
use crate::ctx::Ctx;

fn to_strs(v: &[std::path::PathBuf]) -> Vec<String> {
    v.iter().map(|p| p.to_string_lossy().to_string()).collect()
}

pub fn run() {
    let settings = settings::load();
    tauri::Builder::default()
        .setup(move |app| {
            let handle = app.handle().clone();
            let ctx = Arc::new(Ctx::new(handle, settings.clone()));
            if let Ok(mut c) = ctx.cache.lock() {
                *c = appnames::load_cache();
            }
            // 前端两个开关默认都是开（对齐 ui.html 的 checked 初始态）
            ctx.auto_open.store(true, Ordering::SeqCst);
            ctx.auto_emu.store(true, Ordering::SeqCst);

            let pool = Arc::new(tasks::Pool::new(tasks::worker_count()));
            app.manage(AppState {
                ctx: ctx.clone(),
                pool: pool.clone(),
            });

            // 依赖探测（先探测再绑定，否则真机上明明有 adb 却报"未找到"）
            tasks::refresh_deps(&ctx);
            // 命令行直入：--install <apk…> / --uninstall <apk|包名…>
            // 走与界面完全相同的后端链路，便于脚本化调用与自动化冒烟
            let cli: Vec<String> = std::env::args().skip(1).collect();
            if let Some(pos) = cli
                .iter()
                .position(|a| a == "--install" || a == "--uninstall")
            {
                let kind = cli[pos].trim_start_matches('-').to_string();
                let files: Vec<String> = cli[pos + 1..]
                    .iter()
                    .filter(|a| !a.starts_with('-'))
                    .cloned()
                    .collect();
                if files.is_empty() {
                    ctx.log("命令行缺少文件参数：--install <apk 路径…>", "err");
                } else {
                    let cc = ctx.clone();
                    let cp = pool.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(1200));
                        cc.log(&format!("命令行触发 {}：{} 项", kind, files.len()), "info");
                        if kind == "install" {
                            tasks::submit_install(&cc, &cp, files, false);
                        } else {
                            tasks::submit_uninstall(&cc, &cp, files);
                        }
                    });
                }
            }

            // 原生拖放 -> 前端（shim.js 转成高亮与安装动作）
            if let Some(win) = app.get_webview_window("main") {
                let w = win.clone();
                win.on_window_event(move |ev| match ev {
                    tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Enter { paths, position }) => {
                        let _ = w.emit(
                            "drag:over",
                            json!({ "x": position.x, "y": position.y, "paths": to_strs(paths) }),
                        );
                    }
                    tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Over { position }) => {
                        let _ = w.emit("drag:over", json!({ "x": position.x, "y": position.y }));
                    }
                    tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, position }) => {
                        let _ = w.emit(
                            "drag:drop",
                            json!({ "x": position.x, "y": position.y, "paths": to_strs(paths) }),
                        );
                    }
                    tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Leave) => {
                        let _ = w.emit("drag:leave", json!({}));
                    }
                    _ => {}
                });
            }

            // 自检播报 + 状态灯 + 可选自动启动（等前端就绪后再推，避免事件丢失）
            let c = ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(800));
                c.log(
                    &format!("APK 管家 v{} 已启动（Rust + Tauri2）", env!("CARGO_PKG_VERSION")),
                    "info",
                );
                match c.adb() {
                    Some(a) => c.log(&format!("adb：{}", a.display()), "ok"),
                    None => c.log("未找到 adb，请确认应用宝模拟器已安装", "err"),
                }
                let has_aapt = c.aapt.lock().map(|g| g.is_some()).unwrap_or(false)
                    || tasks::refresh_deps(&c).1;
                if has_aapt {
                    c.log("aapt 已找到", "ok");
                } else {
                    c.log("未找到 aapt，包名与图标名走内置 APK 解析", "warn");
                }
                c.log("等待模拟器连接 127.0.0.1:5555 …", "info");
                tasks::start_status_loop(&c);

                if c.auto_emu.load(Ordering::SeqCst) {
                    let (ok, msg) = deps::adb_check(c.adb().as_ref(), true);
                    if !ok {
                        c.log(&format!("{}，尝试自动启动模拟器", msg), "warn");
                        tasks::boot_emulator(&c, true);
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            api::refresh_adb,
            api::pick,
            api::start_task,
            api::start_emulator,
            api::set_auto_open,
            api::set_auto_emu,
            api::get_apps,
            api::uninstall_pkg
        ])
        .run(tauri::generate_context!())
        .expect("APK 管家启动失败");
}
