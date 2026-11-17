//! 进程执行、编码解码、日志文件——与 v4 `shell.py` + `config._file_log` 对齐
//!
//! 关键约束（v4 留下的三个真实缺陷教训）：
//! 1. adb / net 的输出在本机是 GBK，硬编码 UTF-8 会出中文乱码 → 一律按字节读再按
//!    UTF-8 → GBK → 替换符三级解码，任何输入都不抛异常；
//! 2. 所有子进程都必须带 CREATE_NO_WINDOW，否则黑框一闪（GUI 版体验杀手）；
//! 3. 必须有超时，adb 设备离线时 `adb shell` 可能永久挂住。

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// CREATE_NO_WINDOW
#[cfg(windows)]
pub const NO_WINDOW: u32 = 0x0800_0000;

pub struct Out {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Out {
    pub fn all(&self) -> String {
        let mut s = self.out.clone();
        s.push_str(&self.err);
        s
    }
    #[allow(dead_code)]
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// 三级解码：UTF-8 严格 → GBK → 替换符兜底
pub fn decode(b: &[u8]) -> String {
    if b.is_empty() {
        return String::new();
    }
    if let Ok(s) = std::str::from_utf8(b) {
        return s.to_string();
    }
    let (cow, _, had_err) = encoding_rs::GBK.decode(b);
    if !had_err {
        return cow.into_owned();
    }
    String::from_utf8_lossy(b).into_owned()
}

fn build(cmd: &str, args: &[String]) -> Command {
    let mut c = Command::new(cmd);
    c.args(args);
    #[cfg(windows)]
    c.creation_flags(NO_WINDOW);
    c
}

/// 同步执行并收集输出（带超时）。超时返回 code=-2，启动失败返回 code=-1。
pub fn run(cmd: &str, args: &[String], timeout: Duration) -> Out {
    let mut child = match build(cmd, args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Out {
                code: -1,
                out: String::new(),
                err: format!("启动失败：{}", e),
            }
        }
    };

    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let h1 = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(ref mut s) = so {
            let _ = s.read_to_end(&mut v);
        }
        v
    });
    let h2 = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(ref mut s) = se {
            let _ = s.read_to_end(&mut v);
        }
        v
    });

    let deadline = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break -2;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break -3,
        }
    };

    let ob = h1.join().unwrap_or_default();
    let eb = h2.join().unwrap_or_default();
    Out {
        code,
        out: decode(&ob),
        err: decode(&eb),
    }
}

/// 逐行流式执行（adb push 进度用）：adb 用 \r 刷新同一行，所以 \r 和 \n 都当行尾
pub fn run_stream<F>(cmd: &str, args: &[String], on_line: F, timeout: Duration) -> Out
where
    F: FnMut(&str) + Send + 'static,
{
    let cb: Arc<Mutex<F>> = Arc::new(Mutex::new(on_line));
    let mut child = match build(cmd, args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Out {
                code: -1,
                out: String::new(),
                err: format!("启动失败：{}", e),
            }
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let cb_out = cb.clone();
    let h_out = std::thread::spawn(move || {
        let mut acc = String::new();
        if let Some(s) = stdout {
            pump(s, &mut acc, &cb_out);
        }
        acc
    });
    let cb_err = cb.clone();
    let h_err = std::thread::spawn(move || {
        let mut acc = String::new();
        if let Some(s) = stderr {
            pump(s, &mut acc, &cb_err);
        }
        acc
    });

    let deadline = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break -2;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break -3,
        }
    };

    let out = h_out.join().unwrap_or_default();
    let err = h_err.join().unwrap_or_default();
    Out { code, out, err }
}

fn pump<R: Read, F: FnMut(&str)>(mut r: R, acc: &mut String, cb: &Arc<Mutex<F>>) {
    let mut chunk = [0u8; 4096];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                for &b in &chunk[..n] {
                    if b == b'\n' || b == b'\r' {
                        if !pending.is_empty() {
                            let line = decode(&pending);
                            acc.push_str(&line);
                            acc.push('\n');
                            if let Ok(mut f) = cb.lock() {
                                f(&line);
                            }
                            pending.clear();
                        }
                    } else {
                        pending.push(b);
                    }
                }
            }
            Err(_) => break,
        }
    }
    if !pending.is_empty() {
        let line = decode(&pending);
        acc.push_str(&line);
        acc.push('\n');
        if let Ok(mut f) = cb.lock() {
            f(&line);
        }
    }
}

/// HTML 转义（toast 走 innerHTML，动态值必须转义，对齐 v4 shell.html_esc）
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ------------------------------------------------------------------ 日志文件

static LOG_LOCK: Mutex<()> = Mutex::new(());

pub fn local_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string())
    });
    PathBuf::from(base).join("ApkManager")
}

pub fn log_path() -> PathBuf {
    local_dir().join("apkmanager.log")
}

/// 追加日志到 %LOCALAPPDATA%\ApkManager\apkmanager.log，超 2MB 轮转为 .1
pub fn file_log(text: &str) {
    let _g = LOG_LOCK.lock();
    let path = log_path();
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if let Ok(md) = std::fs::metadata(&path) {
        if md.len() > 2 * 1024 * 1024 {
            let mut bak = path.clone();
            bak.set_extension("log.1");
            let _ = std::fs::remove_file(&bak);
            let _ = std::fs::rename(&path, &bak);
        }
    }
    let line = format!("[{}] {}\r\n", timestamp(), text);
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn timestamp() -> String {
    // 只依赖 std：用 SystemTime 换算本地时间成本高，这里用简单 HH:MM:SS（UTC+8 由系统时区决定）
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs_of_day = (now + 8 * 3600) % 86400;
    format!(
        "{:02}:{:02}:{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// 生成短任务 id（8 位），避免前端 row id 冲突
pub fn short_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32;
    format!("{:x}{:x}", t & 0xffff, n & 0xfff)
}
