//! System tray launcher for clashtui.
//! Icon color reflects the core mode: white = normal, yellow = TUN,
//! blue = system proxy. Left-click launches clashtui.exe; the right-click
//! menu offers launch / web dashboard / autostart toggle / exit.

#![cfg(windows)]
// Run as a GUI app so no console window opens alongside the tray icon.
#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::Arc;

use muda::CheckMenuItem;
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use winit::application::ApplicationHandler;
use winit::event::{WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};

const CREATE_NEW_CONSOLE: u32 = 0x00000010;
const CREATE_NO_WINDOW: u32 = 0x08000000;
const CORE_API: &str = "http://127.0.0.1:9090";

#[derive(Debug)]
enum UserEvent {
    TrayIcon(TrayIconEvent),
    Menu(muda::MenuEvent),
}

/// App mode used to pick the tray icon color.
#[derive(Clone, Copy, PartialEq)]
enum IconMode {
    Normal,
    Tun,
    SystemProxy,
}

fn exe_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn clashtui_path() -> std::path::PathBuf {
    let here = exe_dir();
    let candidates = [here.join("clashtui.exe"), here.join("..").join("clashtui.exe")];
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .unwrap_or_else(|| here.join("clashtui.exe"))
}

fn launch_clashtui() {
    let path = clashtui_path();
    if path.exists() {
        // Launch via cmd start: this reliably gives the console app a window
        // and Windows routes it into Windows Terminal by default (matching a
        // manual launch).
        let _ = Command::new("cmd")
            .args(["/c", "start", "", &path.to_string_lossy()])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}

/// Run a clashtui service subcommand (restart/stop) in a visible console so
/// clashtui's own UAC elevation works and the user sees the result.
fn run_core_service(args: &[&str]) {
    let path = clashtui_path();
    if path.exists() {
        let _ = Command::new(&path)
            .arg("service")
            .args(args)
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn();
    }
}

/// Is clashtui currently running?
fn clashtui_running() -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq clashtui.exe", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .to_lowercase()
            .contains("clashtui.exe"),
        Err(_) => false,
    }
}

/// Stop any running clashtui process.
fn stop_clashtui() {
    let _ = Command::new("taskkill")
        .args(["/IM", "clashtui.exe", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

/// Toggle clashtui: launch it if not running, otherwise close it.
fn toggle_clashtui() {
    if clashtui_running() {
        stop_clashtui();
    } else {
        launch_clashtui();
    }
}

fn open_web() {
    let _ = Command::new("cmd")
        .args(["/c", "start", "", "http://127.0.0.1:9090/ui/"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Ask the core whether TUN is enabled.
fn tun_enabled() -> bool {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok();
    match client {
        Some(c) => match c.get(format!("{CORE_API}/configs")).send() {
            Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>() {
                Ok(v) => v
                    .get("tun")
                    .and_then(|t| t.get("enable"))
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false),
                Err(_) => false,
            },
            _ => false,
        },
        None => false,
    }
}

/// Measure real latency through the mihomo mixed-port proxy. This reflects the
/// actual node chosen by the 智能选择 (Smart) group, since its now-target is a
/// virtual "Smart - Select" that the API does not expose directly.
fn measure_latency() -> Option<u64> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .proxy(reqwest::Proxy::all("http://127.0.0.1:7890").ok()?)
        .build()
        .ok()?;
    let start = std::time::Instant::now();
    let resp = client
        .get("https://cp.cloudflare.com/generate_204")
        .send()
        .ok()?;
    if resp.status().is_success() {
        Some(start.elapsed().as_millis() as u64)
    } else {
        None
    }
}

/// Name of the currently selected item under the top-level 总体模式 group.
fn current_mode_name() -> String {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok();
    if let Some(c) = client {
        if let Ok(resp) = c.get(format!("{CORE_API}/proxies")).send() {
            if resp.status().is_success() {
                if let Ok(v) = resp.json::<serde_json::Value>() {
                    let now = v
                        .get("proxies")
                        .and_then(|p| p.get("总体模式"))
                        .and_then(|g| g.get("now"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    if !now.is_empty() {
                        return now.to_string();
                    }
                }
            }
        }
    }
    "总体模式".to_string()
}

/// Tooltip showing the current mode's name plus its real latency through the
/// proxy (the actual node traffic uses).
fn current_mode_tooltip() -> String {
    let name = current_mode_name();
    match measure_latency() {
        Some(ms) => format!("{name} {ms}ms"),
        None => format!("{name} FALSE"),
    }
}
/// Check whether the Windows system proxy points at the core (enabled).
fn system_proxy_enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
    {
        Ok(key) => key.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0,
        Err(_) => false,
    }
}

// wininet.dll's InternetSetOption tells WinInet to reload proxy settings.
#[link(name = "wininet")]
extern "system" {
    fn InternetSetOptionW(
        h_internet: *mut core::ffi::c_void,
        option: u32,
        buffer: *mut core::ffi::c_void,
        buffer_length: u32,
    ) -> i32;
}

const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
const INTERNET_OPTION_REFRESH: u32 = 37;

// kernel32 named mutex: single-instance detection. The handle is created in
// the main process and kept alive for the whole process lifetime.
#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(
        lp_mutex_attributes: *mut core::ffi::c_void,
        b_initial_owner: i32,
        lp_name: *const u16,
    ) -> *mut core::ffi::c_void;
    fn OpenMutexW(
        dw_desired_access: u32,
        b_inherit_handle: i32,
        lp_name: *const u16,
    ) -> *mut core::ffi::c_void;
}
const SYNCHRONIZE: u32 = 0x00100000;

/// Return true if another tray instance already holds the named mutex.
/// Strategy: try to open the existing mutex first (returns non-null if it
/// exists). Only if it does NOT exist do we create it, which keeps the
/// detection independent of GetLastError timing after CreateMutexW.
fn single_instance_check() -> bool {
    // The "\0" terminator is required: CreateMutexW/OpenMutexW take a
    // null-terminated UTF-16 string. Without it the OS reads past the Vec
    // into random memory until it hits a 0, so each process would compute a
    // different (garbage) name and the mutex would never be found.
    let name: Vec<u16> = "ClashtuiTraySingleton\0".encode_utf16().collect();
    let opened = unsafe { OpenMutexW(SYNCHRONIZE, 0, name.as_ptr()) };
    if !opened.is_null() {
        // An existing instance is already running.
        return true;
    }
    // No instance yet: create the mutex and keep the handle alive for the
    // whole process lifetime (deliberately leaked) so other instances can
    // detect us.
    let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr()) };
    if handle.is_null() {
        // Cannot create the mutex; be conservative and let this instance run.
        return false;
    }
    false
}

/// Enable or disable the Windows system proxy pointing at the core.
fn set_system_proxy(enabled: bool) {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    const PROXY_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(
        PROXY_KEY,
        winreg::enums::KEY_WRITE | winreg::enums::KEY_READ,
    ) {
        if enabled {
            let _ = key.set_value("ProxyEnable", &1u32);
            let _ = key.set_value("ProxyServer", &"127.0.0.1:7890");
        } else {
            let _ = key.set_value("ProxyEnable", &0u32);
        }
        // Notify WinInet so applications pick up the change immediately.
        unsafe {
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_SETTINGS_CHANGED,
                std::ptr::null_mut(),
                0,
            );
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_REFRESH,
                std::ptr::null_mut(),
                0,
            );
        }
    }
}

/// Toggle TUN mode in the running core via the config API.
fn set_tun(enabled: bool) {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok();
    if let Some(c) = client {
        let payload = serde_json::json!({ "tun": { "enable": enabled } });
        let _ = c
            .patch(format!("{CORE_API}/configs"))
            .json(&payload)
            .send();
    }
}

fn detect_mode() -> IconMode {
    if tun_enabled() {
        IconMode::Tun
    } else if system_proxy_enabled() {
        IconMode::SystemProxy
    } else {
        IconMode::Normal
    }
}

/// Apply a mode: TUN uses the core's TUN stack, system proxy uses the Windows
/// registry, normal turns both off.
fn apply_mode(mode: IconMode) {
    match mode {
        IconMode::Tun => {
            set_system_proxy(false);
            set_tun(true);
        }
        IconMode::SystemProxy => {
            set_tun(false);
            set_system_proxy(true);
        }
        IconMode::Normal => {
            set_tun(false);
            set_system_proxy(false);
        }
    }
}

/// Load one of the bundled PNG icons (scaled to tray size), falling back to
/// a generated solid-color square.
fn load_icon(mode: IconMode) -> Icon {
    // Icons are embedded at compile time so the tray exe is self-contained.
    let data = match mode {
        IconMode::Tun => include_bytes!("../assets/tray_tun.png").as_slice(),
        IconMode::SystemProxy => include_bytes!("../assets/tray_sysproxy.png").as_slice(),
        IconMode::Normal => include_bytes!("../assets/tray_white.png").as_slice(),
    };
    if let Ok(img) = image::load_from_memory(data) {
        // Use a high-resolution source so Windows can scale it crisply at
        // any DPI. 256px is a good balance for the system tray.
        let small = img.thumbnail(256, 256).into_rgba8();
        let (w, h) = small.dimensions();
        let rgba = small.into_raw();
        if let Ok(icon) = Icon::from_rgba(rgba, w, h) {
            return icon;
        }
    }
    // Fallback: 32x32 solid square
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    let color = match mode {
        IconMode::Tun => [255u8, 220u8, 0u8, 255u8],
        IconMode::SystemProxy => [30u8, 144u8, 255u8, 255u8],
        IconMode::Normal => [240u8, 240u8, 240u8, 255u8],
    };
    for _ in 0..32 * 32 {
        rgba.extend_from_slice(&color);
    }
    Icon::from_rgba(rgba, 32, 32).unwrap_or_else(|_| Icon::from_rgba(vec![0; 0], 1, 1).unwrap())
}

struct Application {
    tray_icon: Option<TrayIcon>,
    launch_item: Arc<MenuItem>,
    web_item: Arc<MenuItem>,
    restart_core_item: Arc<MenuItem>,
    stop_core_item: Arc<MenuItem>,
    mode_tun: Arc<CheckMenuItem>,
    mode_sysproxy: Arc<CheckMenuItem>,
    exit_core_item: Arc<MenuItem>,
    exit_item: Arc<MenuItem>,
    last_tooltip_refresh: std::time::Instant,
}

impl Application {
    fn new() -> Self {
        let menu = Menu::new();
        let launch_item = Arc::new(MenuItem::new("仪表盘", true, None));
        let web_item = Arc::new(MenuItem::new("打开网页端", true, None));
        let restart_core_item = Arc::new(MenuItem::new("重启内核", true, None));
        let stop_core_item = Arc::new(MenuItem::new("停止内核", true, None));
        let mode = detect_mode();
        let mode_tun = Arc::new(CheckMenuItem::new("TUN 模式", true, mode == IconMode::Tun, None));
        let mode_sysproxy = Arc::new(CheckMenuItem::new(
            "系统代理",
            true,
            mode == IconMode::SystemProxy,
            None,
        ));
        let exit_core_item = Arc::new(MenuItem::new("关闭代理", true, None));
        let exit_item = Arc::new(MenuItem::new("退出", true, None));
        menu.append_items(&[
            launch_item.as_ref(),
            web_item.as_ref(),
            &PredefinedMenuItem::separator(),
            restart_core_item.as_ref(),
            stop_core_item.as_ref(),
            &PredefinedMenuItem::separator(),
            mode_tun.as_ref(),
            mode_sysproxy.as_ref(),
            &PredefinedMenuItem::separator(),
            exit_core_item.as_ref(),
            exit_item.as_ref(),
        ])
        .expect("failed to build menu");

        Self {
            tray_icon: None,
            launch_item,
            web_item,
            restart_core_item,
            stop_core_item,
            mode_tun,
            mode_sysproxy,
            exit_core_item,
            exit_item,
            last_tooltip_refresh: std::time::Instant::now(),
        }
    }

    fn refresh_mode_checks(&self, mode: IconMode) {
        self.mode_tun.set_checked(mode == IconMode::Tun);
        self.mode_sysproxy.set_checked(mode == IconMode::SystemProxy);
    }

    fn rebuild_tray(&mut self) {
        let mode = detect_mode();
        self.refresh_mode_checks(mode);
        let icon = load_icon(mode);
        let tip = current_mode_tooltip();
        self.tray_icon = Some(
            TrayIconBuilder::new()
                .with_menu_on_left_click(false)
                .with_menu(Box::new(self.menu()))
                .with_tooltip(&tip)
                .with_icon(icon)
                .build()
                .expect("failed to create tray icon"),
        );
    }

    /// Update the icon/tooltip in place after a mode switch, without
    /// destroying and recreating the tray icon. Rebuilding on every toggle
    /// removes the icon from the Windows notification area and back, which
    /// loses any pinned position the user set.
    fn update_icon(&self) {
        let mode = detect_mode();
        self.refresh_mode_checks(mode);
        let icon = load_icon(mode);
        let tip = current_mode_tooltip();
        if let Some(tray) = &self.tray_icon {
            let _ = tray.set_icon(Some(icon));
            let _ = tray.set_tooltip(Some(tip.as_str()));
        }
    }

    fn menu(&self) -> Menu {
        let menu = Menu::new();
        menu.append_items(&[
            self.launch_item.as_ref(),
            self.web_item.as_ref(),
            &PredefinedMenuItem::separator(),
            self.restart_core_item.as_ref(),
            self.stop_core_item.as_ref(),
            &PredefinedMenuItem::separator(),
            self.mode_tun.as_ref(),
            self.mode_sysproxy.as_ref(),
            &PredefinedMenuItem::separator(),
            self.exit_core_item.as_ref(),
            self.exit_item.as_ref(),
        ])
        .expect("failed to rebuild menu");
        menu
    }
}

impl ApplicationHandler<UserEvent> for Application {
    fn resumed(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: WindowEvent,
    ) {
    }

    fn new_events(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        cause: winit::event::StartCause,
    ) {
        if winit::event::StartCause::Init == cause {
            self.rebuild_tray();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Refresh the tooltip every 30s with a fresh real latency measurement.
        if self.last_tooltip_refresh.elapsed() >= std::time::Duration::from_secs(30) {
            if let Some(tray) = &self.tray_icon {
                let _ = tray.set_tooltip(Some(current_mode_tooltip()));
            }
            self.last_tooltip_refresh = std::time::Instant::now();
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    }

    fn user_event(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::TrayIcon(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            })
            | UserEvent::TrayIcon(TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }) => toggle_clashtui(),
            // Refresh the tooltip the moment the mouse hovers the icon.
            UserEvent::TrayIcon(TrayIconEvent::Enter { .. }) => {
                if let Some(tray) = &self.tray_icon {
                    let _ = tray.set_tooltip(Some(current_mode_tooltip()));
                }
                self.last_tooltip_refresh = std::time::Instant::now();
            }
            UserEvent::Menu(ev) => {
                let id = ev.id;
                if id == self.launch_item.id() {
                    launch_clashtui();
                } else if id == self.web_item.id() {
                    open_web();
                } else if id == self.restart_core_item.id() {
                    run_core_service(&["restart"]);
                } else if id == self.stop_core_item.id() {
                    run_core_service(&["stop"]);
                } else if id == self.mode_tun.id() {
                    // Toggle: if TUN is already active, clear it back to normal.
                    let current = detect_mode();
                    let target = if current == IconMode::Tun {
                        IconMode::Normal
                    } else {
                        IconMode::Tun
                    };
                    apply_mode(target);
                    self.update_icon();
                } else if id == self.mode_sysproxy.id() {
                    // Toggle: if system proxy is already active, clear it back.
                    let current = detect_mode();
                    let target = if current == IconMode::SystemProxy {
                        IconMode::Normal
                    } else {
                        IconMode::SystemProxy
                    };
                    apply_mode(target);
                    self.update_icon();
                } else if id == self.exit_core_item.id() {
                    run_core_service(&["stop"]);
                    _event_loop.exit();
                } else if id == self.exit_item.id() {
                    _event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    // Single instance: if another tray process already holds the mutex,
    // don't stack a second tray icon. Instead toggle clashtui (same as a
    // left-click on the running tray) and exit this duplicate: launch it if
    // it is not running, or close it if it already is.
    if single_instance_check() {
        toggle_clashtui();
        return;
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::TrayIcon(event));
    }));
    let proxy = event_loop.create_proxy();
    muda::MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    let mut app = Application::new();
    if let Err(err) = event_loop.run_app(&mut app) {
        eprintln!("tray error: {err:?}");
    }
}
