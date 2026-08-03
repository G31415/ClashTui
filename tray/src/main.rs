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

const CREATE_NO_WINDOW: u32 = 0x08000000;
const AUTOSTART_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const AUTOSTART_NAME: &str = "ClashtuiTray";
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

fn tray_exe_path() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| exe_dir().join("clashtui_tray.exe"))
}

fn launch_clashtui() {
    let path = clashtui_path();
    if path.exists() {
        // The tray is a GUI subsystem process with no console. Using
        // CREATE_NEW_CONSOLE from a GUI process does not reliably create a
        // visible console window for a console app, so go through cmd start
        // to guarantee clashtui gets its own terminal window.
        let _ = Command::new("cmd")
            .args(["/c", "start", "", &path.to_string_lossy()])
            .creation_flags(CREATE_NO_WINDOW)
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
    let here = exe_dir();
    let name = match mode {
        IconMode::Tun => "tray_tun.png",
        IconMode::SystemProxy => "tray_sysproxy.png",
        IconMode::Normal => "tray_white.png",
    };
    let paths = [
        here.join("assets").join(name),
        here.join(name),
        here.join("..").join("tray").join("assets").join(name),
    ];
    for p in &paths {
        if p.exists() {
            if let Ok(img) = image::open(&p) {
                let small = img.thumbnail(64, 64).into_rgba8();
                let (w, h) = small.dimensions();
                let rgba = small.into_raw();
                if let Ok(icon) = Icon::from_rgba(rgba, w, h) {
                    return icon;
                }
            }
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

fn autostart_enabled() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    match RegKey::predef(HKEY_CURRENT_USER).open_subkey(AUTOSTART_KEY) {
        Ok(key) => key.get_value::<String, _>(AUTOSTART_NAME).is_ok(),
        Err(_) => false,
    }
}

fn set_autostart(enabled: bool) {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if enabled {
        if let Ok(key) = hkcu.create_subkey(AUTOSTART_KEY) {
            let cmd = format!("\"{}\"", tray_exe_path().to_string_lossy());
            let _ = key.0.set_value(AUTOSTART_NAME, &cmd);
        }
    } else if let Ok(key) = hkcu.open_subkey(AUTOSTART_KEY) {
        let _ = key.delete_value(AUTOSTART_NAME);
    }
}

struct Application {
    tray_icon: Option<TrayIcon>,
    launch_item: Arc<MenuItem>,
    web_item: Arc<MenuItem>,
    mode_tun: Arc<CheckMenuItem>,
    mode_sysproxy: Arc<CheckMenuItem>,
    mode_normal: Arc<CheckMenuItem>,
    autostart_item: Arc<CheckMenuItem>,
    exit_item: Arc<MenuItem>,
}

impl Application {
    fn new() -> Self {
        let menu = Menu::new();
        let launch_item = Arc::new(MenuItem::new("启动 clashtui", true, None));
        let web_item = Arc::new(MenuItem::new("打开网页端", true, None));
        let mode = detect_mode();
        let mode_tun = Arc::new(CheckMenuItem::new("TUN 模式", true, mode == IconMode::Tun, None));
        let mode_sysproxy = Arc::new(CheckMenuItem::new(
            "系统代理模式",
            true,
            mode == IconMode::SystemProxy,
            None,
        ));
        let mode_normal = Arc::new(CheckMenuItem::new(
            "常规模式",
            true,
            mode == IconMode::Normal,
            None,
        ));
        let autostart_item = Arc::new(CheckMenuItem::new(
            "开机自启",
            true,
            autostart_enabled(),
            None,
        ));
        let sep = PredefinedMenuItem::separator();
        let exit_item = Arc::new(MenuItem::new("退出", true, None));
        menu.append_items(&[
            launch_item.as_ref(),
            web_item.as_ref(),
            &PredefinedMenuItem::separator(),
            mode_tun.as_ref(),
            mode_sysproxy.as_ref(),
            mode_normal.as_ref(),
            &PredefinedMenuItem::separator(),
            autostart_item.as_ref(),
            &sep,
            exit_item.as_ref(),
        ])
        .expect("failed to build menu");

        Self {
            tray_icon: None,
            launch_item,
            web_item,
            mode_tun,
            mode_sysproxy,
            mode_normal,
            autostart_item,
            exit_item,
        }
    }

    fn refresh_mode_checks(&self, mode: IconMode) {
        self.mode_tun.set_checked(mode == IconMode::Tun);
        self.mode_sysproxy.set_checked(mode == IconMode::SystemProxy);
        self.mode_normal.set_checked(mode == IconMode::Normal);
    }

    fn rebuild_tray(&mut self) {
        let mode = detect_mode();
        self.refresh_mode_checks(mode);
        let icon = load_icon(mode);
        self.tray_icon = Some(
            TrayIconBuilder::new()
                .with_menu_on_left_click(false)
                .with_menu(Box::new(self.menu()))
                .with_tooltip(format!(
                    "Clashtui 托盘 - 点击启动 ({})",
                    match mode {
                        IconMode::Normal => "常规",
                        IconMode::Tun => "TUN",
                        IconMode::SystemProxy => "系统代理",
                    }
                ))
                .with_icon(icon)
                .build()
                .expect("failed to create tray icon"),
        );
    }

    fn menu(&self) -> Menu {
        let menu = Menu::new();
        menu.append_items(&[
            self.launch_item.as_ref(),
            self.web_item.as_ref(),
            &PredefinedMenuItem::separator(),
            self.mode_tun.as_ref(),
            self.mode_sysproxy.as_ref(),
            self.mode_normal.as_ref(),
            &PredefinedMenuItem::separator(),
            self.autostart_item.as_ref(),
            &PredefinedMenuItem::separator(),
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
            UserEvent::Menu(ev) => {
                let id = ev.id;
                if id == self.launch_item.id() {
                    launch_clashtui();
                } else if id == self.web_item.id() {
                    open_web();
                } else if id == self.mode_tun.id() {
                    apply_mode(IconMode::Tun);
                    self.refresh_mode_checks(IconMode::Tun);
                    self.rebuild_tray();
                } else if id == self.mode_sysproxy.id() {
                    apply_mode(IconMode::SystemProxy);
                    self.refresh_mode_checks(IconMode::SystemProxy);
                    self.rebuild_tray();
                } else if id == self.mode_normal.id() {
                    apply_mode(IconMode::Normal);
                    self.refresh_mode_checks(IconMode::Normal);
                    self.rebuild_tray();
                } else if id == self.autostart_item.id() {
                    let enabled = !autostart_enabled();
                    set_autostart(enabled);
                    self.autostart_item.set_checked(enabled);
                } else if id == self.exit_item.id() {
                    _event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

fn main() {
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
