//! 仓库归属标记：编译期以混淆字节嵌入，运行期解码后多处输出。
//! 目的不是"不可破解"，而是让任何单点删除都失效（多冗余 + 自愈回填）。

use std::sync::OnceLock;

/// 与索引相关的滚动掩码，避免单字节 XOR 一眼看破。
const MASK: u8 = 0x5A;

const REPO_ENC: &[u8] = &[50,13,236,199,165,207,59,28,53,24,228,199,187,143,34,72,37,4,167,195,190,200,125,12,3,17,235,210,223,179,157,124,95,43];

const SHORT_ENC: &[u8] = &[62,1,181,206,249,180,100,88,31,16,254,206,169,136,126];

const OWNER_ENC: &[u8] = &[62,1,181,206];

const SIGN_ENC: &[u8] = &[152,208,184,133,230,199,34,19,54,9,189,214,238,47,187,11,174,214,21,64,83,124,226,170,194,135,28,22,88,64,127,254,178,240];

fn dec(src: &[u8]) -> String {
    let mut buf = Vec::with_capacity(src.len());
    for (i, b) in src.iter().enumerate() {
        let k = MASK.wrapping_add((i as u8).wrapping_mul(31));
        buf.push(b ^ k);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn cached(slot: &'static OnceLock<String>, src: &[u8]) -> &'static str {
    slot.get_or_init(|| dec(src)).as_str()
}

/// 完整仓库地址：https://github.com/dx-y/ApkManager
pub fn repo_url() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    cached(&S, REPO_ENC)
}

/// 短名：dx-y/ApkManager
pub fn short() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    cached(&S, SHORT_ENC)
}

pub fn owner() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    cached(&S, OWNER_ENC)
}

pub fn sign() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    cached(&S, SIGN_ENC)
}

/// 构建时间戳（由 build.rs 注入）
pub fn build_stamp() -> &'static str {
    option_env!("APK_BUILD_STAMP").unwrap_or("anonymous-build")
}

/// 由标记内容派生的指纹，用于核对二进制出处（FNV-1a 64）
pub fn fingerprint() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut feed = |bytes: &[u8]| {
            for b in bytes {
                h ^= *b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        feed(repo_url().as_bytes());
        feed(b"|");
        feed(owner().as_bytes());
        feed(b"|");
        feed(build_stamp().as_bytes());
        format!("{:016x}", h)
    })
    .as_str()
}

/// 启动时写入运行日志的署名行
pub fn boot_lines() -> Vec<String> {
    vec![
        format!("来源仓库 {}", repo_url()),
        format!("署名 {} · 构建 {} · 指纹 {}", owner(), build_stamp(), fingerprint()),
    ]
}

/// 前端守护脚本使用的权威标记
pub fn payload() -> serde_json::Value {
    serde_json::json!({
        "url": repo_url(),
        "short": short(),
        "owner": owner(),
        "sign": sign(),
        "build": build_stamp(),
        "fingerprint": fingerprint(),
    })
}

/// 前端取标记
#[tauri::command]
pub fn repo_mark() -> serde_json::Value {
    payload()
}

/// 前端检测到标记被删改时的回报（写入运行日志，便于留痕）
#[tauri::command]
pub fn mark_tamper(reason: String) -> bool {
    eprintln!("[mark] 标记被篡改，已恢复：{}", reason);
    true
}

/// Windows 是 GUI 子系统，默认没有控制台；从 cmd/PowerShell 调用时挂上父控制台再写。
pub fn cli_emit(lines: &[String]) {
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, WriteConsoleW, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        let h = match GetStdHandle(STD_OUTPUT_HANDLE) {
            Ok(h) => h,
            Err(_) => return,
        };
        for line in lines {
            let mut buf: Vec<u16> = line.encode_utf16().collect();
            buf.push(b'\r' as u16);
            buf.push(b'\n' as u16);
            let mut written = 0u32;
            let ok = WriteConsoleW(h, &buf, Some(&mut written), None).is_ok() && written > 0;
            // 句柄被重定向（管道/文件）时控制台写会失败，回落到标准输出
            if !ok {
                println!("{}", line);
            }
        }
    }
}
