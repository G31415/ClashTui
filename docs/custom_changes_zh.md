# ClashTui 自定义改动说明

> 本文档记录基于上游 [JohanChane/clashtui](https://github.com/JohanChane/clashtui) 的所有个性化改动，包括 TUI 源码功能增强、系统托盘启动器、配置调整与部署方式。改动日期：2026-08-03。

---

## 一、改动总览

| 模块 | 位置 | 改动 |
|---|---|---|
| TUI 源码 | 本仓库 `src/` | 节点延迟显示、测速超时、排序持久化、GoToNow、默认 tab、打开网页端 |
| 系统托盘 | 本仓库 `tray/` | 新增独立托盘程序 `clashtui_tray.exe` |
| 配置文件 | `C:\Programs\Portable\ClashTui\data\` | keymap、config、Template 订阅等 |
| 部署目录 | `C:\Programs\Portable\ClashTui\` | 编译产物 + 启动脚本 + 使用说明 |

---

## 二、TUI 源码功能改动（`src/`）

### 1. Provider 节点延迟显示
**文件：** `src/functions/restful/proxies.rs`

mihomo smart 内核不会把 proxy-provider 的叶节点注册进全局 `/proxies` map，导致这些节点在 TUI 中看不到延迟。

- `fetch_proxies()` 额外请求 `/providers/proxies`，把 provider 暴露的叶节点按名称合并回顶层 map。
- 已在全局 map 中的节点跳过，避免覆盖真实数据。

### 2. 测速超时修复
**文件：** `src/tui/tab/proxies/handlers.rs`

- 单节点测速 `timeout`：10s → **3s**（`TEST_DELAY_TIMEOUT_MS = 3000`）
- 分组整体等待：13s → **20s**（`TEST_WAIT_SECS = 20`）
- 大分组测速不再中途超时。

### 3. 分组排序持久化
**文件：** `src/tui/tab/proxies/tree.rs`、`content.rs`

- 每个分组可分别按名称 / 按延迟 / 不排序。
- 排序模式保存到 `data/sort_state.yaml`，重启后保留。
- 快捷键：
  - `s+n` 按名称 / `s+d` 按延迟 / `s+r` 重置（当前分组）
  - `S+n` `S+d` `S+r` 全局排序（所有分组）

### 4. GoToNow 快速定位
**文件：** `src/tui/tab/proxies.rs`、`content.rs`、`tree.rs`

- 新增 `g n` 快捷键：光标在分组上时，自动展开该分组并跳到当前选中的（`now`）节点。

### 5. 默认启动 tab
**文件：** `src/config/core.rs`、`src/tui/app.rs`

- `config.yaml` 中 `extra.default_tab: 2`，启动直接进入 Proxies 页。
- tab 编号：0=Status 1=File 2=Proxies 3=Connections 4=Logs 5=Settings 6=CoreSrvCtl。

### 6. 打开网页端
**文件：** `src/config/core.rs`、`src/functions/command.rs`、`src/tui/app.rs`

- 新增 `extra.open_web_cmd` 配置项（留空用默认浏览器打开）。
- TUI 内按 `Ctrl+g` 再按 `w` 打开 `http://127.0.0.1:9090/ui/`。

---

## 三、系统托盘程序（`tray/`）

### 1. 工程结构
```
tray/
├── Cargo.toml          # 依赖：tray-icon 0.19, muda 0.15, winit 0.30,
│                       #       image, winreg, reqwest, serde_json
├── assets/
│   ├── tray_white.png      # 普通模式图标（白）
│   ├── tray_tun.png        # TUN 模式图标（黄）
│   └── tray_sysproxy.png   # 系统代理图标（蓝）
└── src/main.rs
```
- 图标通过 `include_bytes!` 嵌入 exe，**自包含，部署时无需 assets 文件夹**。
- 图标源为高分辨率 PNG，编译时缩放到 256px；已裁掉透明边距并**保持宽高比**（不拉伸）。
- `#![windows_subsystem = "windows"]`：以 GUI 子系统运行，**无控制台窗口**。

### 2. 交互方式
| 操作 | 行为 |
|---|---|
| 左键单击 / 双击 | 切换 clashtui：未运行则启动，运行中则关闭 |
| 右键菜单 | 见下方菜单项 |
| 鼠标悬停 | tooltip 显示「当前模式名 + 实测延迟」，如 `智能选择 123ms` |

### 3. 右键菜单
| 菜单项 | 行为 |
|---|---|
| 仪表盘 | 启动 `clashtui.exe` |
| 打开网页端 | 浏览器打开 `http://127.0.0.1:9090/ui/` |
| 重启内核 | `clashtui service restart` |
| 停止内核 | `clashtui service stop` |
| TUN 模式 | 切换 TUN；已开启时再点取消回普通模式 |
| 系统代理 | 切换系统代理；已开启时再点取消回普通模式 |
| 关闭代理 | `clashtui service stop` 后退出托盘 |
| 退出 | 仅退出托盘，不影响内核 |

### 4. 模式切换实现
| 模式 | 实现 |
|---|---|
| TUN | `PATCH /configs` 设置 `tun.enable = true` |
| 系统代理 | 写注册表 `Internet Settings` 的 `ProxyEnable` + `InternetSetOptionW` 通知 WinInet |
| 普通模式 | 同时关闭 TUN 与系统代理 |
- 图标颜色跟随当前模式自动变化（白 / 黄 / 蓝）。
- 两种模式都被取消时回到普通模式（白色图标）。

### 5. 内核启停
托盘以**普通权限**运行，通过调用 `clashtui service restart|stop` 控制内核，由 clashtui 自己处理 UAC 提权，避免托盘提权后出现难看的新控制台。

### 6. 悬停 tooltip 延迟测量
- 通过代理实测：`reqwest` 走 `http://127.0.0.1:7890`，请求 `https://cp.cloudflare.com/generate_204`。
- 超时 **5000ms**，超时显示 `FALSE`。
- 悬停时立即刷新，平时每 30 秒自动刷新一次。
- 模式名动态读取 `/proxies` 下「总体模式」分组的 `now` 字段，不写死。

---

## 四、配置改动（部署目录 `data/`）

### 1. `config.yaml`
```yaml
extra:
  edit_cmd: code "%s"
  open_dir_cmd: explorer "%s"
  open_web_cmd: ""        # 打开网页端命令，留空用默认浏览器
  default_tab: 2          # 默认进入 Proxies 页
timeout: 10
```

### 2. `keymap.yaml`
- proxies 区域新增 `<Esc>` → `CollapseAll`（折叠全部节点）。
- 注意：keymap 是**替换语义**，需保留完整的默认绑定；按键名必须带尖括号（`"<Esc>"`），否则会被解析成字符 `e`。

### 3. 订阅（Profile）重写
- G31415 订阅按 clashtui 规范重写为 **Template 类型**，provider 放入 `template_proxy_providers.yaml`。
- 文件位置：
  - `data/mihomo/templates/G31415.yaml`
  - `data/mihomo/template_proxy_providers.yaml`
- 内核侧 provider 复制到 `mihomo/data/proxy_provider/鸡场.yaml`（20 个节点）。

### 4. 排序记忆文件
- `data/sort_state.yaml`：由 TUI 自动生成 / 维护，记录各分组排序模式。

---

## 五、部署方式（`C:\Programs\Portable\ClashTui\`）

```
C:\Programs\Portable\ClashTui\
├── clashtui.exe          # TUI 主程序（含上述源码改动）
├── clashtui_tray.exe     # 系统托盘程序
├── 启动托盘.bat          # 双击启动托盘
├── 使用说明.md
├── data/                 # clashtui 数据（config/keymap/theme/sort_state 等）
├── mihomo/               # mihomo 内核 + 配置文件 + provider
└── sing-box/             # sing-box 内核目录（备用）
```

**启动托盘.bat：**
```bat
@echo off
cd /d "%~dp0"
start "" "%~dp0clashtui_tray.exe"
```

---

## 六、开机自启动（已删除）

> ⚠️ 自启动功能最终**已移除**，本文档留档说明原因。

**背景：** 托盘曾实现「开机自启」菜单项，写入注册表 `HKCU\...\CurrentVersion\Run`，键名 `ClashtuiTray`。

**问题：** Windows 的 `HKCU\...\Explorer\StartupApproved\Run` 存在对应禁用标志（首字节 `0x02`）。系统上的 **Lenovo Vantage 启动优化**会在登录时把这个标志反复改回 `0x02`（禁用），导致 Run 项被静默跳过、托盘不启动——即使手动清除标志也会很快复发。

**处理：** 移除「开机自启」菜单项及全部相关代码（`autostart_enabled()` / `set_autostart()` / 相关常量），并清理注册表中的 Run 项与禁用标志。托盘目前需要手动启动。

**替代方案（未采纳）：** 启动文件夹 `.bat` 或任务计划程序（登录触发）可绕开 StartupApproved 对 Run 项的干预，如需恢复自启动可考虑。

---

## 七、关键文件索引

| 文件 | 作用 |
|---|---|
| `src/functions/restful/proxies.rs` | provider 延迟合并 |
| `src/tui/tab/proxies/tree.rs` | 排序持久化、GoToNow 定位 |
| `src/tui/tab/proxies/content.rs` | 排序/GoToNow 按键分发 |
| `src/tui/tab/proxies/handlers.rs` | 测速超时参数 |
| `src/tui/tab/proxies.rs` | GoToNow 按键定义 |
| `src/config/core.rs` | `extra.open_web_cmd`、`extra.default_tab` |
| `src/functions/command.rs` | `open_web()` |
| `src/tui/app.rs` | `Ctrl+g w`、默认 tab 应用 |
| `tray/src/main.rs` | 托盘程序全部逻辑 |
| `tray/assets/*.png` | 三种模式图标 |
