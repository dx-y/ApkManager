/* APK 管家 v5 —— Tauri 桥接层
 * 作用：把原 pywebview.api.* 前端契约接到 Rust 命令上，并把 Rust 事件
 * 翻译成 ui.html 里已有的 __log / __task / __adb / __appsNames 等钩子。
 * 目的：ui.html 一行不改，接口语义 100% 保持。
 */
(function () {
  var T = window.__TAURI__;
  if (!T || !T.core) {
    // 前端仍在，只是后端没接上：把状态点亮成"后端未就绪"，避免白屏误判
    console.error('[shim] __TAURI__ 不可用');
    var d = document.getElementById('stDot');
    if (d) { d.className = 'dot off'; }
    var t = document.getElementById('stTxt');
    if (t) { t.textContent = '后端未就绪'; }
    return;
  }
  var invoke = T.core.invoke;
  var listen = T.event.listen;

  /* ---------------- 1. pywebview.api 兼容层 ---------------- */
  window.pywebview = window.pywebview || {};
  window.pywebview.api = {
    refresh_adb: function () { return invoke('refresh_adb'); },
    pick: function (kind) { return invoke('pick', { kind: kind }); },
    start: function (kind, paths) { return invoke('start_task', { kind: kind, paths: paths || [] }); },
    start_emulator: function () { return invoke('start_emulator'); },
    set_auto_open: function (v) { return invoke('set_auto_open', { value: !!v }); },
    set_auto_emu: function (v) { return invoke('set_auto_emu', { value: !!v }); },
    get_apps: function () { return invoke('get_apps'); },
    uninstall_pkg: function (pkg) { return invoke('uninstall_pkg', { pkg: pkg }); }
  };

  /* ---------------- 2. Rust 事件 → 前端钩子 ---------------- */
  function on(name, fn) {
    listen(name, function (e) {
      try { fn(e.payload); } catch (err) { console.error('[shim]' + name, err); }
    });
  }

  on('ui:log', function (p) { if (window.__log) window.__log(p.text, p.cls); });
  on('ui:task', function (p) {
    if (window.__task) window.__task(p.kind, p.id, p.name, p.pct, p.state, p.status);
  });
  on('ui:adb', function (p) { if (window.__adb) window.__adb(p.ok, p.msg, p.addr); });
  on('ui:appnames', function (p) { if (window.__appsNames) window.__appsNames(p.names); });
  on('ui:toast', function (p) { if (window.toast) window.toast(p.html, p.kind); });
  on('ui:badge', function (p) { if (window.badgeShow) window.badgeShow(p.which, p.text, p.cls); });
  on('ui:prog', function (p) { if (window.prog) window.prog(p.kind, p.state, p.text, p.pct); });

  /* ---------------- 3. 原生拖放（Tauri 接管，替代 pywebview DOM 桥） ---------------- */
  function dpr() { return window.devicePixelRatio || 1; }
  // Tauri 给的是物理像素坐标，elementFromPoint 吃 CSS 像素
  function kindAt(x, y) {
    try {
      var el = document.elementFromPoint(x / dpr(), y / dpr());
      if (el && el.closest && el.closest('#dzUninstall')) return 'uninstall';
    } catch (e) { /* 坐标越界时按安装处理 */ }
    return 'install';
  }
  function zone(kind) {
    return document.getElementById(kind === 'uninstall' ? 'dzUninstall' : 'dzInstall');
  }
  function clearZone() {
    ['dzInstall', 'dzUninstall'].forEach(function (id) {
      var e = document.getElementById(id);
      if (e) e.classList.remove('drag-on');
    });
  }
  on('drag:over', function (p) { clearZone(); var z = zone(kindAt(p.x, p.y)); if (z) z.classList.add('drag-on'); });
  on('drag:leave', function () { clearZone(); });
  on('drag:drop', function (p) {
    var kind = kindAt(p.x, p.y);
    clearZone();
    var z = zone(kind);
    if (z) {
      z.classList.add('drop-bump');
      setTimeout(function () { z.classList.remove('drop-bump'); }, 580);
    }
    invoke('start_task', { kind: kind, paths: p.paths || [] });
  });

  /* ---------------- 4. 就绪信号（触发 ui.html 的 loadApps） ---------------- */
  function ready() {
    try { window.dispatchEvent(new Event('pywebviewready')); } catch (e) { console.error(e); }
  }
  if (document.readyState === 'complete' || document.readyState === 'interactive') {
    setTimeout(ready, 30);
  } else {
    window.addEventListener('DOMContentLoaded', function () { setTimeout(ready, 30); });
  }
})();
