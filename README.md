# APK 管家 v5（Rust + Tauri 2）

> 一个双击即用的 Windows 小工具：把 APK 拖进窗口，就直接装进**腾讯移动应用引擎（应用宝 PC 版 / Androws）**的安卓模拟器；列表里点一下就能卸载，包括系统预装应用。全程直连 `adb`，绕开模拟器图形界面的「高风险应用」拦截。

## 这个软件解决什么问题

在 Windows 上用安卓模拟器跑自己打包或内测的 APK，常见的三个堵点：

| 堵点 | 具体表现 | APK 管家的处理 |
|---|---|---|
| 安装被拦 | 模拟器/应用宝的界面装第三方 APK 会提示「来源不明、高风险应用」并阻止安装 | 直连 `adb`，走 `pm install -r -t`，不经过任何拦截层 |
| 步骤繁琐 | 手动装要自己找 adb、连 `127.0.0.1:5555`、`push` 到 `/data/local/tmp`、再 `pm install`，十几条命令 | 拖进窗口即可，或点「选择文件」 |
| 卸载受限 | 预装应用 `pm uninstall` 直接返回 `DELETE_FAILED_INTERNAL_ERROR`，卸不掉 | 自动回退 `pm uninstall --user 0`，并对结果给出准确说明 |

## 使用场景

| 场景 | 谁在用 | 怎么用 |
|---|---|---|
| 自研 / 内测 APK 反复装卸 | 自己打包安卓应用、需要在 PC 模拟器上快速验证 | 把新构建出的 APK 拖进窗口 → 自动安装 → 可选自动打开 |
| 批量装机 | 一次拿到多个 APK（如分发给家人朋友的几个应用） | 多选文件一次性拖入，逐个安装并汇总结果 |
| 精简模拟器预装应用 | 觉得模拟器自带应用太杂 | 应用列表里长按目标应用 → 卸载（系统包走 `--user 0`，本体保留在系统分区） |
| 快捷方式 / 微信收到的 APK | 微信下载的 `.apk.1`、桌面 `.lnk` 快捷方式，路径带引号 | 直接拖进来，程序会归一化路径后再装 |
| 自动化 / 脚本验证 | 需要把安装动作写进脚本 | `apkmanager.exe --install D:\path\app.apk`、`--uninstall com.example.app` |
| 状态不确定时 | 忘了模拟器有没有起来 | 一键启动模拟器 + 状态灯每 3 秒轮询在线状态 |

## 谁适合用 / 不需要什么

- **适合**：在 Windows 上用安卓模拟器跑自己打包或内测 APK、需要快速装卸与清理预装应用，但不想记 `adb` 命令的人。
- **不需要**：手机 root、真机、改模拟器镜像、Python 环境。
- **需要**：Windows 10/11 + 已安装腾讯移动应用引擎（应用宝 PC 版）+ 系统自带 WebView2。

## 快速开始（普通用户）

1. 先装好**应用宝 PC 版 / 腾讯移动应用引擎**（模拟器的来源）。
2. 到本仓库 [Release](../../releases) 或 `release/` 目录下载 `ApkManager_5.0.0_x64-setup.exe`，双击安装（按当前用户安装，无需管理员权限）。
3. 打开模拟器，再打开 APK 管家；右下角状态灯显示「模拟器在线 127.0.0.1:5555」即就绪。若显示离线，点「启动模拟器」。
4. 把 APK 拖进窗口（或点「选择文件」）→ 日志区出现「安装完成」；勾选「自动打开」会在装完后直接拉起该应用。
5. 卸载：切到应用列表，长按目标应用 → 确认卸载。

> 首次运行若提示找不到 `adb`，说明应用宝安装路径非默认；在 `%LOCALAPPDATA%\ApkManager\settings.json` 里填写 `androws_root` / `adb` 两项即可（程序本身只读该文件，不会覆盖你的配置）。


把 Android 模拟器里的 APK 安装/卸载做成一个双击即用的小窗口程序：**拖入即装**，直连 `adb`，绕开应用宝的「高风险」拦截。

- 版本：5.0.0
- 技术栈：Rust（后端）+ Tauri 2（窗口/原生拖放）+ 原生 HTML/CSS/JS（前端，无构建步骤）
- 产物：`apkmanager.exe` 3.42 MB / 安装包 `ApkManager_5.0.0_x64-setup.exe` 1.32 MB
- 依赖：系统自带 WebView2（Windows 10/11 默认已装），**不需要** Python、不需要 PyInstaller 运行时

---

## 1. 为什么重写

| 维度 | v4（Python + pywebview + PyInstaller） | v5（Rust + Tauri 2） |
|---|---|---|
| 分发体积 | 约 58 MB（单文件打包，内含 Python 运行时） | **3.42 MB** exe；安装包 1.32 MB |
| 启动到窗口可用 | 需解包运行时 + 初始化 Python | **0.06–0.09 s**（本机实测 3 次：0.09 / 0.08 / 0.06） |
| 运行时依赖 | 自带（体积换独立） | 系统 WebView2（Win11 自带） |
| 中文编码坑 | adb / AndrowsSvr 输出 GBK，需手工探测解码 | 仍按字节读 + 多编码兜底（见 §6） |
| 后端并发 | 线程 + 全局状态 | 线程池（4 worker）+ `Arc<Ctx>` 原子状态 |
| 前端改动 | — | 复用原 `index.html`（987 行），仅加 91 行桥接层 `shim.js` |

结论：**体积降 94%，启动从"秒级等待"变成"即点即在"**，业务规则与前端交互与 v4 保持一致。

---

## 2. 目录结构

```
apk_manager_v5/
├─ ui/
│  ├─ index.html     # 界面（继承 v4，987 行）
│  └─ shim.js        # 桥接层：pywebview.api → Tauri invoke，事件 → window.__xxx（91 行）
└─ src-tauri/
   ├─ Cargo.toml
   ├─ tauri.conf.json      # 窗口 900×980，最小 680×600，可缩放
   └─ src/
      ├─ main.rs           # 入口（6 行）
      ├─ lib.rs            # 应用装配、拖放事件、状态轮询、CLI 入口（146 行）
      ├─ api.rs            # 暴露给前端的 7 个命令（183 行）
      ├─ tasks.rs          # 安装/卸载/应用列表/模拟器启动（809 行）
      ├─ appnames.rs       # 应用名解析与缓存（568 行）
      ├─ paths.rs          # 路径归一化（拖放/微信 .apk.1/.lnk/file://）（395 行）
      ├─ util.rs           # 命令执行、解码、转义、日志（299 行）
      ├─ deps.rs           # adb / 模拟器探测（209 行）
      ├─ settings.rs       # 配置读取（133 行）
      └─ ctx.rs            # 共享状态与事件推送（113 行）
```

---

## 3. 功能

### 安装
- 拖放到窗口 → 自动 `adb push` 到 `/data/local/tmp` → `pm install -r -t` → 可选自动打开。
- 支持多文件同时处理；支持 `.apk`、微信下载的 `.apk.1`、`.lnk` 快捷方式、`file://` 开头的拖放路径、带引号路径。
- 安装前三道本地校验：文件存在 / 非 0 字节 / 不超过 `max_apk_mb`（默认 2048）/ 文件头必须是 `PK\x03\x04`。
- push 后用 `stat -c %s` 比对远端字节数，不一致视为传输不完整并删除远端（老固件不支持 `stat -c` 时返回 -1 直接放行）。

### 卸载
- 应用列表长按应用 → 卸载；也可把 APK 拖到卸载区。
- 系统分区预装包 `pm uninstall` 会返回 `DELETE_FAILED_INTERNAL_ERROR`，此时**自动回退** `pm uninstall --user 0`，成功则提示「已从当前用户卸载（系统预装包，本体保留在系统分区）」。
- 卸载成功后清缓存条目 + 清理桌面快捷方式。

### 应用列表与应用名
- `pm list packages -3 -f` 取第三方应用；若该模拟器不区分第三方（`-3` 为空），退回全量列表并按 APK 分区过滤系统包，并在日志说明。
- 首屏只返回「包名 + 缓存名」，未命中名字的应用交后台补齐（每批 6 个），通过 `ui:appnames` 事件就地替换文字与图标首字。
- 名字来源：`adb exec-out unzip -p <apk> AndroidManifest.xml` → 内置 AXML 解析 → 资源 ID 解析 `resources.arsc` → 优先中文。
- 缓存：`%LOCALAPPDATA%\ApkManager\app_names.json`（原子写入 + 版本号校验）。

### 其他
- 状态灯：模拟器在线 / 离线（每 3 秒轮询 `127.0.0.1:5555`）。
- 「一键启动模拟器」：探测 Androws/应用宝安装目录并拉起（`auto_emu` 默认开，不持久化）。
- 日志面板：清空、按级别着色；日志文件 `%LOCALAPPDATA%\ApkManager\apkmanager.log`，超 2 MB 轮转为 `.1`。
- 配置：`%LOCALAPPDATA%\ApkManager\settings.json`（`androws_root` / `adb` / `aapt` / `max_apk_mb`），程序**只读不写**。

---

## 4. 前端契约（重写时必须保持）

`shim.js` 把原有 `pywebview.api.*` 映射为 Tauri 命令，前端 HTML 不需要改：

| 前端调用 | Tauri 命令 |
|---|---|
| `pywebview.api.refresh_adb()` | `refresh_adb` |
| `pywebview.api.pick(kind)` | `pick` |
| `pywebview.api.start(kind, paths)` | `start_task` |
| `pywebview.api.start_emulator()` | `start_emulator` |
| `pywebview.api.set_auto_open(bool)` | `set_auto_open` |
| `pywebview.api.set_auto_emu(bool)` | `set_auto_emu` |
| `pywebview.api.get_apps()` | `get_apps` |
| `pywebview.api.uninstall_pkg(pkg)` | `start_task`（kind = uninstall） |

后端 → 前端事件（`shim.js` 转成原有全局函数）：

| 事件 | 前端钩子 |
|---|---|
| `ui:log` | `window.__log` |
| `ui:task` | `window.__task` |
| `ui:adb` | `window.__adb` |
| `ui:appnames` | `window.__appsNames` |
| `ui:toast` | `window.__toast` |
| `ui:badge` | `window.__badgeShow` |
| `ui:prog` | `window.__prog` |

`pywebviewready` 事件由 `shim.js` 在 DOMContentLoaded 后 30 ms 触发，前端沿用原来的初始化时机。

原生拖放：`drag:over` / `drag:leave` / `drag:drop`，落点用 `document.elementFromPoint` 判定是安装区还是卸载区。

---

## 5. 构建

```powershell
# 开发（带热重载窗口）
npx tauri dev

# 发布：先出 exe，再压 NSIS 安装包
npx tauri build

# 产物
#   src-tauri\target\release\apkmanager.exe
#   src-tauri\target\release\bundle\nsis\ApkManager_5.0.0_x64-setup.exe
```

前置：Rust stable（msvc）+ VS Build Tools（C++ 桌面开发）+ WebView2 Runtime（Win11 自带）+ Node（仅打包用 `npx @tauri-apps/cli`）。

### 命令行入口（便于自动化验证，与界面同一条链路）

```powershell
apkmanager.exe --install  D:\path\app.apk     # 等同于界面拖入安装
apkmanager.exe --uninstall com.example.app    # 等同于列表里点卸载
```

启动后延时 1.2 s 执行，之后仍会显示窗口。

---

## 6. 实现要点（踩过的坑）

1. **中文乱码**：`adb` 与 `AndrowsSvr` 输出是 GBK，不能硬编码 UTF-8。`util::run` 按**字节**读取，再依次尝试 UTF-8 → GBK(mbcs) → 替换符兜底，任何输入（含 None / 空 / 垃圾字节）都不抛异常。
2. **系统预装包卸载失败**：`DELETE_FAILED_INTERNAL_ERROR` 是系统分区的固有行为，不是 bug；回退 `--user 0` 才能成功，且必须向用户说明「本体仍在系统分区」。
3. **`-3` 可能为空**：部分模拟器镜像把用户应用也算作 system，导致「应用列表恒为 0」。改为回退全量 + 按 APK 路径分区过滤。
4. **首屏慢**：不要同步解析全部应用名。先返回包名 + 缓存名，后台补齐。
5. **分批渲染不能用 `requestAnimationFrame`**：窗口不可见时 rAF 不触发，列表会停在空白；改用 `setTimeout` 切片。
6. **XSS**：前端多处以 `innerHTML` 写入动态值，转义统一在后端做（`util::escape`），toast 里的 `<b>` 由后端自己拼。

---

## 7. 验收记录（真实 GUI，非仅单测）

| 项 | 结果 |
|---|---|
| 启动 | 窗口正常渲染，无白屏；日志输出 `APK 管家 v5.0.0 已启动（Rust + Tauri2）` |
| 状态灯 | 「模拟器在线 127.0.0.1:5555」 |
| 界面选文件安装 | 点「选择文件」→ 系统对话框 → 选定 APK → 日志「已选择 APK，开始安装」→「安装完成：third.apk」→「已自动打开 com.example.testapp」 |
| 设备端落地 | `pm path com.example.testapp` → `/data/app/~~...==/base.apk` |
| 应用列表 | 刷新后列出第三方应用并显示中文名（如「相机」），日志「应用列表：1 个（已有名称 1 个）」 |
| 卸载（用户应用） | `pm uninstall` → Success → 「卸载完成」→ `pm path` 无输出 |
| 卸载（系统预装包） | `DELETE_FAILED_INTERNAL_ERROR` → 自动回退 `--user 0` → Success → 提示「已从当前用户卸载（系统预装包，本体保留在系统分区）」 |
| 编译告警 | `cargo build --release` 零 warning |

测试样本：`third.apk` 由模拟器内 `Camera2.apk` 复制后等长替换包名（`com.android.camera2` → `com.example.testapp`）并重新签名构造，用于验证第三方包从安装到卸载的完整闭环。

---

## 8. 已知限制

1. 仅面向腾讯移动应用引擎（应用宝 PC 版 / Androws），设备地址固定 `127.0.0.1:5555`；换真机需改 `EMU_ADDR`。
2. 应用图标仍用「首字色块」方案，未取真实图标位图。
3. 应用列表一次性返回，未做分页；装到上千个包时需要分页。
4. 系统预装包的 `--user 0` 卸载是「对当前用户隐藏」，`pm list packages -3` 里仍可能残留条目。
5. 安装过程中的进度是阶段式（推送 → 安装 → 收尾），不是按字节的真实进度。

---

## 9. 变更记录（相对 v4）

1. 全量 Rust 重写：12 个 Python 模块 → 10 个 Rust 模块（约 2861 行），业务规则逐项对齐。
2. 体积 58 MB → 3.42 MB；启动 0.06–0.09 s。
3. 前端 `index.html` 原样复用，新增 91 行 `shim.js` 桥接，`pywebview.api` 与 `window.__*` 契约不变。
4. 新增系统预装包卸载回退（`--user 0`）与明确文案。
5. 新增 CLI 入口（`--install` / `--uninstall`），与界面共用同一任务链路，便于自动化验证。
6. 修复「应用列表恒为 0」：`-3` 为空时回退全量并按 APK 分区过滤系统包。
