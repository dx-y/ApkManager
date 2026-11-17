//! 安装 / 卸载 / 应用列表 / 模拟器启动——对齐 v4 `tasks.py` + `api.py` 主链路
//!
//! 业务链（不能省任何一步，都是真机踩出来的）：
//! 拖入 → 路径归一化 → 本地预检（存在/大小/APK 文件头）→ 设备在线确认（离线则自动拉起模拟器）
//! → adb push 到 /data/local/tmp → 校验远端字节数 → pm install -r -t → 可选自动打开。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::appnames;
use crate::ctx::Ctx;
use crate::deps;
use crate::paths as p2;
use crate::settings::{self, EMU_ADDR, MAX_WORKERS, REMOTE_DIR};
use crate::util;

// ------------------------------------------------------------ 并发限制

pub struct Pool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    n: Mutex<usize>,
    cv: Condvar,
    max: usize,
}

impl Pool {
    pub fn new(max: usize) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                n: Mutex::new(0),
                cv: Condvar::new(),
                max: max.max(1),
            }),
        }
    }

    pub fn spawn<F: FnOnce() + Send + 'static>(&self, f: F) {
        let inner = self.inner.clone();
        std::thread::spawn(move || {
            {
                let mut n = match inner.n.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                while *n >= inner.max {
                    n = match inner.cv.wait(n) {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                }
                *n += 1;
            }
            let guard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            if guard.is_err() {
                util::file_log("任务线程 panic 已捕获");
            }
            if let Ok(mut n) = inner.n.lock() {
                *n = n.saturating_sub(1);
                inner.cv.notify_one();
            }
        });
    }
}

// ------------------------------------------------------------ 设备在线保证

pub fn ensure_online(ctx: &Arc<Ctx>) -> bool {
    let adb = ctx.adb();
    if deps::adb_check(adb.as_ref(), true).0 {
        return true;
    }
    // 离线：先尝试无感启动，最多等 135 秒（对齐 v4）
    ctx.log("模拟器离线，正在尝试拉起…", "warn");
    boot_emulator(ctx, true);
    let deadline = Instant::now() + Duration::from_secs(135);
    while Instant::now() < deadline {
        if deps::adb_check(ctx.adb().as_ref(), true).0 {
            ctx.push_adb(true, "模拟器在线");
            return true;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    false
}

// ------------------------------------------------------------ 预检

fn precheck(ctx: &Arc<Ctx>, path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err("文件不存在".to_string());
    }
    let md = std::fs::metadata(p).map_err(|e| format!("无法读取文件信息：{}", e))?;
    if md.len() == 0 {
        return Err("文件是 0 字节".to_string());
    }
    let limit = settings::max_apk_bytes(&ctx.settings);
    if md.len() > limit {
        return Err(format!(
            "超过大小上限（{:.0} MB > {} MB），可在 settings.json 调 max_apk_mb",
            md.len() as f64 / 1048576.0,
            ctx.settings.max_apk_mb
        ));
    }
    use std::io::Read;
    let mut f = std::fs::File::open(p).map_err(|e| format!("无法打开文件：{}", e))?;
    let mut head = [0u8; 4];
    f.read_exact(&mut head).map_err(|_| "读取文件头失败".to_string())?;
    if &head[..2] != b"PK" {
        return Err("不是有效的 APK（缺少 ZIP 文件头）".to_string());
    }
    Ok(())
}

fn remote_size(adb: &Path, remote: &str) -> i64 {
    let o = util::run(
        &adb.to_string_lossy(),
        &[
            "-s".to_string(),
            EMU_ADDR.to_string(),
            "shell".to_string(),
            "stat".to_string(),
            "-c".to_string(),
            "%s".to_string(),
            remote.to_string(),
        ],
        Duration::from_secs(20),
    );
    o.out.trim().parse::<i64>().unwrap_or(-1)
}

fn rm_remote(adb: &Path, remote: &str) {
    let _ = util::run(
        &adb.to_string_lossy(),
        &[
            "-s".to_string(),
            EMU_ADDR.to_string(),
            "shell".to_string(),
            "rm".to_string(),
            "-f".to_string(),
            remote.to_string(),
        ],
        Duration::from_secs(15),
    );
}

fn launch_pkg(ctx: &Arc<Ctx>, adb: &Path, pkg: &str) {
    let o = util::run(
        &adb.to_string_lossy(),
        &[
            "-s".to_string(),
            EMU_ADDR.to_string(),
            "shell".to_string(),
            "monkey".to_string(),
            "-p".to_string(),
            pkg.to_string(),
            "-c".to_string(),
            "android.intent.category.LAUNCHER".to_string(),
            "1".to_string(),
        ],
        Duration::from_secs(30),
    );
    if o.all().to_ascii_lowercase().contains("error") {
        ctx.log(&format!("自动打开 {} 失败（不影响安装结果）", pkg), "warn");
    } else {
        ctx.log(&format!("已自动打开 {}", pkg), "ok");
    }
}

// ------------------------------------------------------------ 安装

pub fn submit_install(ctx: &Arc<Ctx>, pool: &Pool, paths: Vec<String>, from_pick: bool) {
    if paths.is_empty() {
        ctx.toast("没有拿到文件路径，请改用「选择 APK」按钮", "err");
        return;
    }
    for raw in paths {
        let id = util::short_id();
        let path = p2::normalize(&raw);
        let name = Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        ctx.task("install", &id, &name, 0, "add", "排队中…");
        let c = ctx.clone();
        pool.spawn(move || install_one(&c, &path, &id));
    }
    if from_pick {
        ctx.log("已选择 APK，开始安装", "info");
    }
}

fn fail(ctx: &Arc<Ctx>, id: &str, name: &str, msg: &str) {
    ctx.task("install", id, name, 100, "err", msg);
    ctx.prog("install", "idle", "", 0);
    ctx.toast(&format!("{}：{}", util::escape(name), util::escape(msg)), "err");
    ctx.badge("install", "失败", "err");
}

fn install_one(ctx: &Arc<Ctx>, path: &str, id: &str) {
    let name = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    let adb = match ctx.adb() {
        Some(a) => a,
        None => {
            fail(ctx, id, &name, "未找到 adb，请检查模拟器是否安装");
            return;
        }
    };
    if let Err(msg) = precheck(ctx, path) {
        fail(ctx, id, &name, &msg);
        return;
    }

    ctx.task("install", id, &name, 3, "running", "检查设备…");
    ctx.prog("install", "loading", "正在连接模拟器…", 0);
    if !ensure_online(ctx) {
        fail(ctx, id, &name, "模拟器未就绪，请手动启动后重试");
        return;
    }

    let remote = format!("{}/marvis_{}.apk", REMOTE_DIR, id);
    ctx.task("install", id, &name, 8, "running", "推送到模拟器…");
    ctx.prog("install", "bar", &name, 8);

    let local_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let c1 = ctx.clone();
    let nm1 = name.clone();
    let id1 = id.to_string();
    let push = util::run_stream(
        &adb.to_string_lossy(),
        &[
            "-s".to_string(),
            EMU_ADDR.to_string(),
            "push".to_string(),
            path.to_string(),
            remote.clone(),
        ],
        move |line| {
            if let Some(pct) = parse_push_pct(line) {
                let mapped = 8 + (pct as f64 * 0.72) as i32;
                c1.prog("install", "bar", &nm1, mapped);
                c1.task("install", &id1, &nm1, mapped, "running", "推送到模拟器…");
            }
        },
        Duration::from_secs(600),
    );

    if push.code != 0 {
        rm_remote(&adb, &remote);
        let msg = first_line(&push.all());
        fail(ctx, id, &name, &format!("推送失败：{}", if msg.is_empty() { "未知错误".into() } else { msg }));
        return;
    }

    // 远端字节数校验：老固件不支持 stat -c 时返回 -1，直接放行
    let rs = remote_size(&adb, &remote);
    if rs >= 0 && local_size > 0 && rs as u64 != local_size {
        rm_remote(&adb, &remote);
        fail(
            ctx,
            id,
            &name,
            &format!("传输不完整（远端 {} 字节 / 本地 {} 字节）", rs, local_size),
        );
        return;
    }

    ctx.task("install", id, &name, 82, "running", "正在安装…");
    ctx.prog("install", "bar", &name, 82);
    let o = util::run(
        &adb.to_string_lossy(),
        &[
            "-s".to_string(),
            EMU_ADDR.to_string(),
            "shell".to_string(),
            "pm".to_string(),
            "install".to_string(),
            "-r".to_string(),
            "-t".to_string(),
            remote.clone(),
        ],
        Duration::from_secs(300),
    );
    let all = o.all();
    if !all.contains("Success") {
        rm_remote(&adb, &remote);
        let msg = install_error(&all);
        fail(ctx, id, &name, &msg);
        return;
    }

    if ctx.auto_open.load(std::sync::atomic::Ordering::SeqCst) {
        if let Some(pkg) = appnames::package_of(&adb, &remote) {
            launch_pkg(ctx, &adb, &pkg);
        }
    }
    rm_remote(&adb, &remote);

    ctx.task("install", id, &name, 100, "done", "安装完成");
    ctx.prog("install", "idle", "", 0);
    ctx.toast(&format!("<b>{}</b> 安装完成", util::escape(&name)), "ok");
    ctx.badge("install", "完成", "ok");
    ctx.log(&format!("安装完成：{}", name), "ok");

    if p2::is_tmp_drop_copy(path) {
        let _ = std::fs::remove_file(path);
    }
}

fn parse_push_pct(line: &str) -> Option<i32> {
    // adb push 进度形如 "[ 45%] /data/local/tmp/x.apk"
    let i = line.find('%')?;
    let head = &line[..i];
    let digits: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse::<i32>().ok().map(|v| v.clamp(0, 100))
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

fn install_error(all: &str) -> String {
    let lower = all.to_ascii_lowercase();
    if lower.contains("insufficient_storage") {
        return "模拟器存储空间不足".to_string();
    }
    if lower.contains("signatures") {
        return "签名冲突，需先卸载模拟器里的同包应用".to_string();
    }
    if lower.contains("update_incompatible") {
        return "与已装版本不兼容，需先卸载旧版".to_string();
    }
    let tail: Vec<&str> = all
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('['))
        .collect();
    if tail.is_empty() {
        "安装失败（模拟器无返回信息）".to_string()
    } else {
        tail.join(" / ")
    }
}

// ------------------------------------------------------------ 卸载

pub fn submit_uninstall(ctx: &Arc<Ctx>, pool: &Pool, paths: Vec<String>) {
    if paths.is_empty() {
        ctx.toast("没有拿到文件路径，请改用「选择 APK」按钮", "err");
        return;
    }
    for raw in paths {
        let id = util::short_id();
        let pkg = resolve_pkg_from_path(&raw);
        ctx.log(&format!("卸载请求：{} → {}", raw, pkg), "info");
        let name = pkg.clone();
        ctx.task("uninstall", &id, &name, 0, "add", "排队中…");
        let c = ctx.clone();
        pool.spawn(move || uninstall_one(&c, &raw, &pkg, &id));
    }
}

fn resolve_pkg_from_path(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("pkg://") {
        return rest.trim().to_string();
    }
    let path = p2::normalize(raw);
    if path.to_ascii_lowercase().ends_with(".apk") {
        if let Some(pkg) = appnames::package_of_file(Path::new(&path)) {
            return pkg;
        }
    }
    p2::extract_pkg(&path).unwrap_or_else(|| path.clone())
}

fn fail_un(ctx: &Arc<Ctx>, id: &str, name: &str, msg: &str) {
    ctx.task("uninstall", id, name, 100, "err", msg);
    ctx.prog("uninstall", "idle", "", 0);
    ctx.toast(&format!("{}：{}", util::escape(name), util::escape(msg)), "err");
    ctx.badge("uninstall", "失败", "err");
}

fn uninstall_one(ctx: &Arc<Ctx>, _raw: &str, pkg: &str, id: &str) {
    if pkg.is_empty() || pkg == _raw && !pkg.contains('.') {
        fail_un(ctx, id, pkg, "无法识别包名，请拖入对应的 APK");
        return;
    }
    let adb = match ctx.adb() {
        Some(a) => a,
        None => {
            fail_un(ctx, id, pkg, "未找到 adb");
            return;
        }
    };
    ctx.task("uninstall", id, pkg, 10, "running", "连接设备…");
    if !ensure_online(ctx) {
        fail_un(ctx, id, pkg, "模拟器未就绪");
        return;
    }
    ctx.task("uninstall", id, pkg, 55, "running", "正在卸载…");
    ctx.log(&format!("正在卸载 {} …", pkg), "info");
    let o = util::run(
        &adb.to_string_lossy(),
        &[
            "-s".to_string(),
            EMU_ADDR.to_string(),
            "shell".to_string(),
            "pm".to_string(),
            "uninstall".to_string(),
            pkg.to_string(),
        ],
        Duration::from_secs(120),
    );
    let all = o.all();
    ctx.log(
        &format!("pm uninstall {} 返回：{}", pkg, first_line(&all)),
        if all.contains("Success") { "ok" } else { "err" },
    );
    if !all.contains("Success") {
        let msg = if all.contains("not installed") || all.contains("Unknown package") {
            "模拟器里没有这个应用".to_string()
        } else {
            first_line(&all)
        };
        // 系统分区预装包用 pm uninstall 会返回 DELETE_FAILED_*，退回按当前用户卸载
        if all.contains("DELETE_FAILED") || all.contains("Failure") {
            ctx.log("该应用在系统分区，尝试仅对当前用户卸载（--user 0）…", "warn");
            let o2 = util::run(
                &adb.to_string_lossy(),
                &[
                    "-s".to_string(),
                    EMU_ADDR.to_string(),
                    "shell".to_string(),
                    "pm".to_string(),
                    "uninstall".to_string(),
                    "--user".to_string(),
                    "0".to_string(),
                    pkg.to_string(),
                ],
                Duration::from_secs(120),
            );
            let all2 = o2.all();
            ctx.log(
                &format!("pm uninstall --user 0 {} 返回：{}", pkg, first_line(&all2)),
                if all2.contains("Success") { "ok" } else { "err" },
            );
            if all2.contains("Success") {
                appnames::forget(pkg);
                let removed2 = p2::remove_shortcuts_for(pkg, None);
                ctx.log(&format!("{} 已从当前用户卸载（系统预装包，本体保留在系统分区）", pkg), "ok");
                ctx.task("uninstall", id, pkg, 100, "done", "已卸载");
                ctx.prog("uninstall", "idle", "", 0);
                ctx.toast(
                    &format!("<b>{}</b> 已从当前用户卸载（系统预装包）", util::escape(pkg)),
                    "ok",
                );
                ctx.badge("uninstall", "完成", "ok");
                if !removed2.is_empty() {
                    ctx.log(&format!("已清理桌面快捷方式 {} 个", removed2.len()), "info");
                }
                return;
            }
            fail_un(ctx, id, pkg, &format!("系统预装应用无法卸载（{}）", first_line(&all2)));
            return;
        }
        fail_un(ctx, id, pkg, &msg);
        return;
    }
    appnames::forget(pkg);
    let removed = p2::remove_shortcuts_for(pkg, None);
    ctx.log(&format!("{} 卸载完成", pkg), "ok");
    ctx.task("uninstall", id, pkg, 100, "done", "已卸载");
    ctx.prog("uninstall", "idle", "", 0);
    ctx.toast(&format!("<b>{}</b> 已卸载", util::escape(pkg)), "ok");
    ctx.badge("uninstall", "完成", "ok");
    if !removed.is_empty() {
        ctx.log(&format!("已清理桌面快捷方式 {} 个", removed.len()), "info");
    }
}

// ------------------------------------------------------------ 应用列表

#[derive(Serialize, Clone)]
pub struct AppItem {
    pub pkg: String,
    pub label: String,
}

/// 取 `pm list packages [-3] -f` 的 (包名, APK 路径) 列表
fn pm_pairs(adb: &Path, only3: &str) -> Vec<(String, String)> {
    let mut args: Vec<String> = vec![
        "-s".into(),
        EMU_ADDR.into(),
        "shell".into(),
        "pm".into(),
        "list".into(),
        "packages".into(),
    ];
    if !only3.is_empty() {
        args.push(only3.into());
    }
    args.push("-f".into());
    let o = util::run(&adb.to_string_lossy(), &args, Duration::from_secs(60));
    let mut v: Vec<(String, String)> = Vec::new();
    for line in o.out.lines() {
        let l = line.trim();
        let body = match l.strip_prefix("package:") {
            Some(b) => b,
            None => continue,
        };
        // 形如 /data/app/xxx/base.apk=com.foo.bar
        match body.rsplit_once('=') {
            Some((apk, pkg)) => {
                let pkg = pkg.trim();
                if !pkg.is_empty() {
                    v.push((pkg.to_string(), apk.trim().to_string()));
                }
            }
            None => {
                let pkg = body.trim();
                if !pkg.is_empty() {
                    v.push((pkg.to_string(), String::new()));
                }
            }
        }
    }
    v
}

/// APK 落在系统分区时视为系统应用
fn is_system_apk(apk: &str) -> bool {
    if apk.is_empty() {
        return false;
    }
    const SYSTEM_DIRS: [&str; 7] = [
        "/system",
        "/system_ext",
        "/product",
        "/vendor",
        "/apex",
        "/odm",
        "/oem",
    ];
    SYSTEM_DIRS.iter().any(|d| apk.starts_with(d))
}

pub fn list_apps(ctx: &Arc<Ctx>) -> Vec<AppItem> {
    let adb = match ctx.adb() {
        Some(a) => a,
        None => {
            ctx.log("未找到 adb，无法读取应用列表", "warn");
            return Vec::new();
        }
    };
    if !deps::adb_check(Some(&adb), false).0 {
        ctx.push_adb(false, "模拟器离线");
        return Vec::new();
    }
    ctx.push_adb(true, "模拟器在线");
    let cache = match ctx.cache.lock() {
        Ok(g) => g.clone(),
        Err(_) => HashMap::new(),
    };
    // 优先按第三方应用取；个别模拟器镜像把用户应用也标成 system，导致 -3 为空，
    // 此时退回全量列表并按 APK 所在分区把系统包滤掉。
    let mut pairs = pm_pairs(&adb, "-3");
    if pairs.is_empty() {
        pairs = pm_pairs(&adb, "");
        pairs.retain(|(_, apk)| !is_system_apk(apk));
        if !pairs.is_empty() {
            ctx.log("该模拟器未区分第三方应用，已按 APK 分区过滤系统包", "info");
        }
    }
    let mut out: Vec<AppItem> = Vec::new();
    let mut apk_of: HashMap<String, String> = HashMap::new();
    for (pkg, apk) in pairs {
        apk_of.insert(pkg.clone(), apk);
        let label = cache.get(&pkg).cloned().unwrap_or_else(|| pkg.clone());
        out.push(AppItem { pkg, label });
    }
    out.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));

    // 缓存里没有的，后台补齐（首屏不阻塞）
    let todo: Vec<(String, String)> = out
        .iter()
        .filter(|a| !cache.contains_key(&a.pkg))
        .filter_map(|a| apk_of.get(&a.pkg).map(|apk| (a.pkg.clone(), apk.clone())))
        .collect();
    if !todo.is_empty() {
        start_names_background(ctx, todo);
    }
    ctx.log(
        &format!("应用列表：{} 个（已有名称 {} 个）", out.len(), cache.len().min(out.len())),
        "info",
    );
    out
}

fn start_names_background(ctx: &Arc<Ctx>, items: Vec<(String, String)>) {
    let fresh: Vec<(String, String)> = {
        let mut busy = match ctx.names_busy.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let mut v = Vec::new();
        for (pkg, apk) in items {
            if busy.insert(pkg.clone()) {
                v.push((pkg, apk));
            }
        }
        v
    };
    if fresh.is_empty() {
        return;
    }
    let adb = match ctx.adb() {
        Some(a) => a,
        None => return,
    };
    let c = ctx.clone();
    let keys: Vec<String> = fresh.iter().map(|(p, _)| p.clone()).collect();
    std::thread::spawn(move || {
        let c2 = c.clone();
        let got = appnames::fetch_batch(adb, fresh, 6, 6, move |batch| {
            {
                if let Ok(mut cache) = c2.cache.lock() {
                    for (k, v) in &batch {
                        cache.insert(k.clone(), v.clone());
                    }
                }
            }
            c2.push_app_names(&batch);
        });
        if !got.is_empty() {
            let snapshot = match c.cache.lock() {
                Ok(g) => g.clone(),
                Err(_) => got.clone(),
            };
            appnames::save_cache(&snapshot);
        }
        if let Ok(mut busy) = c.names_busy.lock() {
            for k in keys {
                busy.remove(&k);
            }
        }
    });
}

// ------------------------------------------------------------ 模拟器启动

pub fn boot_emulator(ctx: &Arc<Ctx>, auto: bool) {
    if ctx.is_booting() {
        if !auto {
            ctx.toast("正在启动中，请稍候…", "warn");
        }
        return;
    }
    ctx.set_booting(true);
    let c = ctx.clone();
    std::thread::spawn(move || {
        c.push_adb(false, "正在启动模拟器…");
        c.log("开始启动模拟器（应用宝 Androws）", "info");
        let addr = EMU_ADDR.to_string();
        if let Some(adb) = c.adb() {
            let _ = util::run(
                &adb.to_string_lossy(),
                &["connect".to_string(), addr.clone()],
                Duration::from_secs(15),
            );
        }
        if deps::adb_check(c.adb().as_ref(), true).0 {
            c.push_adb(true, "模拟器在线");
            c.set_booting(false);
            return;
        }

        // 1) 内核服务
        let svc = settings::ANDROWS_SVC;
        let q = util::run(
            "sc",
            &["query".to_string(), svc.to_string()],
            Duration::from_secs(15),
        );
        let running = q.all().to_ascii_uppercase().contains("RUNNING");
        if !running {
            let s = util::run(
                "net",
                &["start".to_string(), svc.to_string()],
                Duration::from_secs(60),
            );
            if s.code == 0 {
                c.log("已启动 Androws 内核服务", "info");
            } else {
                c.log(
                    &format!("Androws 内核服务启动失败（可能需要管理员权限）：{}", first_line(&s.all())),
                    "warn",
                );
            }
        }

        // 2) 模拟器主程序
        let exe = c
            .androws_exe
            .lock()
            .ok()
            .and_then(|g| g.clone());
        if let Some(exe) = exe {
            if !proc_running("Androws.exe") {
                match std::process::Command::new(&exe).spawn() {
                    Ok(_) => c.log("已拉起 Androws.exe", "info"),
                    Err(e) => c.log(&format!("拉起 Androws.exe 失败：{}", e), "err"),
                }
            } else {
                c.log("Androws.exe 已在运行，等待设备就绪", "info");
            }
        } else {
            c.log("未找到 Androws.exe，请手动启动应用宝模拟器", "warn");
        }

        // 3) 轮询等设备
        let deadline = Instant::now() + Duration::from_secs(135);
        let mut ok = false;
        while Instant::now() < deadline {
            if deps::adb_check(c.adb().as_ref(), true).0 {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_secs(3));
        }
        c.set_booting(false);
        if ok {
            c.push_adb(true, "模拟器在线");
            c.log("模拟器已就绪", "ok");
        } else {
            c.push_adb(false, "模拟器离线");
            c.log("等待模拟器超时（135 秒），请手动启动", "err");
        }
    });
}

fn proc_running(image: &str) -> bool {
    let o = util::run(
        "tasklist",
        &[
            "/FI".to_string(),
            format!("IMAGENAME eq {}", image),
            "/NH".to_string(),
        ],
        Duration::from_secs(20),
    );
    o.all().to_ascii_lowercase().contains(&image.to_ascii_lowercase())
}

// ------------------------------------------------------------ 状态轮询

pub fn start_status_loop(ctx: &Arc<Ctx>) {
    let c = ctx.clone();
    std::thread::spawn(move || loop {
        let (ok, msg) = deps::adb_check(c.adb().as_ref(), false);
        c.push_adb(ok, &msg);
        std::thread::sleep(Duration::from_secs(3));
    });
}

pub fn worker_count() -> usize {
    MAX_WORKERS
}

/// 探测 adb / aapt / 模拟器路径并写回运行态；返回 (有 adb, 有 aapt)
pub fn refresh_deps(ctx: &Arc<Ctx>) -> (bool, bool) {
    let d = deps::detect(&ctx.settings);
    if let Ok(mut g) = ctx.adb.lock() {
        *g = d.adb.clone();
    }
    if let Ok(mut g) = ctx.aapt.lock() {
        *g = d.aapt.clone();
    }
    if let Ok(mut g) = ctx.androws_root.lock() {
        *g = d.androws_root.clone();
    }
    if let Ok(mut g) = ctx.androws_exe.lock() {
        *g = d.androws_exe.clone();
    }
    for n in &d.notes {
        util::file_log(n);
    }
    (d.adb.is_some(), d.aapt.is_some())
}
