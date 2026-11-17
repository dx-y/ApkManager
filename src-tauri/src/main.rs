// 发布版隐藏控制台窗口（对齐用户要求：桌面 GUI EXE，不弹黑框）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    apkmanager_lib::run()
}
