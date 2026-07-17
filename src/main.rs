#![windows_subsystem = "windows"]

use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    UpdateWindow, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
    DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromStream, GdipCreateFromHDC, GdipDeleteGraphics, GdipDisposeImage,
    GdipDrawImageRectRectI, GdipGetImageHeight, GdipGetImageWidth, GdiplusStartup,
    GdiplusStartupInput, UnitPixel,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{
    RegSetValueExW, RegCreateKeyExW, RegCloseKey, HKEY_CURRENT_USER, HKEY, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, INITCOMMONCONTROLSEX, ICC_BAR_CLASSES, TBM_SETRANGE, TBM_SETPOS,
};

const TBM_GETPOS: u32 = 0x0400; // WM_USER
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_CONTROL, MOD_SHIFT};
use windows::Win32::UI::Shell::{SHCreateMemStream, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;

fn hmenu(id: usize) -> HMENU {
    HMENU(id as *mut std::ffi::c_void)
}

unsafe fn force_foreground(hwnd: HWND) {
    let fg = GetForegroundWindow();
    let fg_thread = GetWindowThreadProcessId(fg, None);
    let my_thread = GetCurrentThreadId();
    if fg_thread != my_thread {
        let _ = AttachThreadInput(my_thread, fg_thread, true);
        let _ = SetForegroundWindow(hwnd);
        let _ = AttachThreadInput(my_thread, fg_thread, false);
    } else {
        let _ = SetForegroundWindow(hwnd);
    }
}

fn hbrush_system(color_index: i32) -> windows::Win32::Graphics::Gdi::HBRUSH {
    windows::Win32::Graphics::Gdi::HBRUSH((color_index + 1) as isize as *mut std::ffi::c_void)
}

const HOTKEY_ID: i32 = 1;
const ID_BTN_RESTART: usize = 101;
const ID_BTN_CHROME: usize = 102;
const ID_BTN_SPOTIFY: usize = 103;
const ID_TRACKBAR_VOL: usize = 104;
const ID_STATIC_BATTERY: usize = 105;

const CHROME_PATH: &str = "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe";
const SPOTIFY_PATH: &str = "C:\\Users\\gamer\\AppData\\Local\\Microsoft\\WindowsApps\\Spotify.exe";

const BUBBLE_SIZE: i32 = 46;
const BUBBLE_TIMER_ID: usize = 2;
const BUBBLE_BOB_AMPLITUDE: f64 = 5.0;
const BUBBLE_BOB_PERIOD_MS: f64 = 2600.0;

static mut MAIN_HWND: Option<HWND> = None;
static mut BUBBLE_HWND: Option<HWND> = None;
static mut BUBBLE_BITMAP: Option<HBITMAP> = None;
static mut BUBBLE_BASE_X: i32 = 0;
static mut BUBBLE_BASE_Y: i32 = 0;
static mut BUBBLE_START: Option<Instant> = None;

const BUBBLE_ICON_BYTES: &[u8] = include_bytes!("../assets/handyman.jpg");

unsafe fn init_gdiplus() {
    let input = GdiplusStartupInput {
        GdiplusVersion: 1,
        ..Default::default()
    };
    let mut token: usize = 0;
    let mut output = std::mem::zeroed();
    let _ = GdiplusStartup(&mut token, &input, &mut output);
}

/// Loads the icon (embedded in the binary), crops it to a square, and bakes it into a
/// premultiplied-alpha 32bpp DIB with a smooth (anti-aliased) circular mask, ready for UpdateLayeredWindow.
unsafe fn load_bubble_bitmap(size: i32) -> Option<HBITMAP> {
    let stream = SHCreateMemStream(Some(BUBBLE_ICON_BYTES))?;
    let mut image: *mut windows::Win32::Graphics::GdiPlus::GpImage = std::ptr::null_mut();
    if GdipCreateBitmapFromStream(&stream, &mut image as *mut _ as *mut _) != windows::Win32::Graphics::GdiPlus::Ok {
        return None;
    }

    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let _ = GdipGetImageWidth(image, &mut width);
    let _ = GdipGetImageHeight(image, &mut height);
    if width == 0 || height == 0 {
        let _ = GdipDisposeImage(image);
        return None;
    }

    let side = width.min(height);
    let src_x = ((width - side) / 2) as i32;
    let src_y = ((height - side) / 2) as i32;

    // Render at 2x and downsample later for extra edge smoothness.
    let render_size = size * 2;

    let mut bmi: BITMAPINFO = std::mem::zeroed();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = render_size;
    bmi.bmiHeader.biHeight = -render_size; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB.0 as u32;

    let screen_dc = GetDC(None);
    let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let Ok(render_bm) = CreateDIBSection(screen_dc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) else {
        ReleaseDC(None, screen_dc);
        let _ = GdipDisposeImage(image);
        return None;
    };
    let mem_dc = CreateCompatibleDC(screen_dc);
    let old = SelectObject(mem_dc, render_bm);

    let mut graphics: *mut windows::Win32::Graphics::GdiPlus::GpGraphics = std::ptr::null_mut();
    if GdipCreateFromHDC(mem_dc, &mut graphics) == windows::Win32::Graphics::GdiPlus::Ok {
        let _ = GdipDrawImageRectRectI(
            graphics,
            image as *mut _,
            0,
            0,
            render_size,
            render_size,
            src_x,
            src_y,
            side as i32,
            side as i32,
            UnitPixel,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
        );
        let _ = GdipDeleteGraphics(graphics);
    }
    let _ = GdipDisposeImage(image);

    // Apply an anti-aliased circular alpha mask to the high-res render.
    let render_pixels = std::slice::from_raw_parts_mut(bits_ptr as *mut u8, (render_size * render_size * 4) as usize);
    let center = render_size as f32 / 2.0;
    let radius = center - 1.0;
    let edge = 2.0f32;
    for y in 0..render_size {
        for x in 0..render_size {
            let idx = ((y * render_size + x) * 4) as usize;
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let coverage = ((radius + edge * 0.5 - dist) / edge).clamp(0.0, 1.0);
            let alpha = (coverage * 255.0).round() as u32;
            let b = render_pixels[idx] as u32;
            let g = render_pixels[idx + 1] as u32;
            let r = render_pixels[idx + 2] as u32;
            render_pixels[idx] = ((b * alpha) / 255) as u8;
            render_pixels[idx + 1] = ((g * alpha) / 255) as u8;
            render_pixels[idx + 2] = ((r * alpha) / 255) as u8;
            render_pixels[idx + 3] = alpha as u8;
        }
    }

    // Downsample 2x -> 1x (box filter) into the final bitmap for a smoother edge than a single-pass render.
    let mut final_bmi = bmi;
    final_bmi.bmiHeader.biWidth = size;
    final_bmi.bmiHeader.biHeight = -size;
    let mut final_bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let final_bm = CreateDIBSection(screen_dc, &final_bmi, DIB_RGB_COLORS, &mut final_bits_ptr, None, 0).ok();

    if final_bm.is_some() {
        let final_pixels = std::slice::from_raw_parts_mut(final_bits_ptr as *mut u8, (size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                let mut sums = [0u32; 4];
                for sy in 0..2 {
                    for sx in 0..2 {
                        let sidx = (((y * 2 + sy) * render_size + (x * 2 + sx)) * 4) as usize;
                        for c in 0..4 {
                            sums[c] += render_pixels[sidx + c] as u32;
                        }
                    }
                }
                let didx = ((y * size + x) * 4) as usize;
                for c in 0..4 {
                    final_pixels[didx + c] = (sums[c] / 4) as u8;
                }
            }
        }
    }

    SelectObject(mem_dc, old);
    let _ = DeleteDC(mem_dc);
    let _ = DeleteObject(render_bm);
    ReleaseDC(None, screen_dc);

    final_bm
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--install" {
        install_startup();
        return;
    }

    // Boot-time watchdog: give explorer a chance to init, then check once.
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(18));
        check_and_heal_tray();
    });

    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);

        let hinstance = GetModuleHandleW(None).unwrap();
        let class_name = w!("TrayGuardianMainWnd");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(main_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("TrayGuardian"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            0,
            0,
            None,
            None,
            hinstance,
            None,
        )
        .unwrap();

        // Ctrl+Shift+T opens the quick panel.
        let _ = RegisterHotKey(hwnd, HOTKEY_ID, MOD_CONTROL | MOD_SHIFT, 0x54);

        MAIN_HWND = Some(hwnd);
        init_gdiplus();
        create_bubble(hinstance.into());

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn install_startup() {
    unsafe {
        let exe = std::env::current_exe().unwrap();
        let exe_str = exe.to_string_lossy().to_string();
        let mut hkey = HKEY::default();
        let subkey = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey,
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
        .is_ok()
        {
            let value_name = w!("TrayGuardian");
            let data = wide(&exe_str);
            let data_bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
            let _ = RegSetValueExW(hkey, value_name, 0, REG_SZ, Some(data_bytes));
            let _ = RegCloseKey(hkey);
            println!("Installed startup entry: {}", exe_str);
        } else {
            eprintln!("Failed to write registry Run key");
        }
    }
}

fn check_and_heal_tray() {
    unsafe {
        let tray_wnd = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null());
        let mut broken = tray_wnd.is_err() || tray_wnd.as_ref().map(|h| h.0.is_null()).unwrap_or(true);

        if !broken {
            let tray = tray_wnd.unwrap();
            let notify = FindWindowExW(tray, None, w!("TrayNotifyWnd"), PCWSTR::null());
            broken = notify.is_err() || notify.as_ref().map(|h| h.0.is_null()).unwrap_or(true);
        }

        if broken {
            let _ = Command::new("taskkill").args(["/F", "/IM", "explorer.exe"]).status();
            thread::sleep(Duration::from_millis(500));
            let _ = Command::new("explorer.exe").spawn();
        }
    }
}

fn restart_tray_now() {
    thread::spawn(|| {
        let _ = Command::new("taskkill").args(["/F", "/IM", "explorer.exe"]).status();
        thread::sleep(Duration::from_millis(500));
        let _ = Command::new("explorer.exe").spawn();
    });
}

fn launch_app(path: &str) {
    unsafe {
        let wpath = wide(path);
        let _ = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wpath.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn get_master_volume() -> Option<f32> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        endpoint.GetMasterVolumeLevelScalar().ok()
    }
}

fn set_master_volume(level: f32) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) {
            if let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                if let Ok(endpoint) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                    let guid = windows::core::GUID::zeroed();
                    let _ = endpoint.SetMasterVolumeLevelScalar(level, &guid);
                }
            }
        }
    }
}

fn get_battery_text() -> String {
    unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut status).is_ok() {
            if status.BatteryLifePercent == 255 {
                return "No battery".to_string();
            }
            let charging = status.ACLineStatus == 1;
            format!(
                "Battery: {}% {}",
                status.BatteryLifePercent,
                if charging { "(charging)" } else { "" }
            )
        } else {
            "Battery: unknown".to_string()
        }
    }
}

static mut PANEL_HWND: Option<HWND> = None;

const ANIM_TIMER_ID: usize = 1;
const ANIM_TIMER_INTERVAL_MS: u32 = 8;
const ANIM_DURATION_MS: u64 = 320;
const ANIM_SLIDE_OFFSET: i32 = 34;

struct AnimState {
    opening: bool,
    start: Instant,
    rest_x: i32,
    rest_y: i32,
    width: i32,
    height: i32,
}

static mut ANIM: Option<AnimState> = None;

fn ease_in_out_cubic(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let f = -2.0 * t + 2.0;
        1.0 - (f * f * f) / 2.0
    }
}

unsafe fn start_show_animation(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) {
    let _ = SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        x,
        y + ANIM_SLIDE_OFFSET,
        width,
        height,
        SWP_NOACTIVATE,
    );
    let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), 0, LWA_ALPHA);
    let _ = ShowWindow(hwnd, SW_SHOWNA);
    ANIM = Some(AnimState {
        opening: true,
        start: Instant::now(),
        rest_x: x,
        rest_y: y,
        width,
        height,
    });
    SetTimer(hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
}

unsafe fn start_hide_animation(hwnd: HWND) {
    let mut rect = RECT::default();
    let _ = GetWindowRect(hwnd, &mut rect);
    ANIM = Some(AnimState {
        opening: false,
        start: Instant::now(),
        rest_x: rect.left,
        rest_y: rect.top,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
    });
    SetTimer(hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
}

unsafe fn advance_animation(hwnd: HWND) {
    let Some(state) = ANIM.as_ref() else { return };
    let t = (state.start.elapsed().as_millis() as f64 / ANIM_DURATION_MS as f64).min(1.0);
    let eased = ease_in_out_cubic(t);

    let (y, alpha) = if state.opening {
        let y = state.rest_y + ANIM_SLIDE_OFFSET - (ANIM_SLIDE_OFFSET as f64 * eased) as i32;
        let alpha = (255.0 * eased) as u8;
        (y, alpha)
    } else {
        let y = state.rest_y + (ANIM_SLIDE_OFFSET as f64 * eased) as i32;
        let alpha = if t < 0.5 {
            255u8
        } else {
            let t2 = (t - 0.5) / 0.5;
            (255.0 * (1.0 - t2)) as u8
        };
        (y, alpha)
    };

    let _ = SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        state.rest_x,
        y,
        state.width,
        state.height,
        SWP_NOACTIVATE,
    );
    let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), alpha, LWA_ALPHA);

    if t >= 1.0 {
        let _ = KillTimer(hwnd, ANIM_TIMER_ID);
        if !state.opening {
            let _ = ShowWindow(hwnd, SW_HIDE);
            // Reset to resting position while hidden so the next open animates from the right spot.
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                state.rest_x,
                state.rest_y,
                state.width,
                state.height,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        ANIM = None;
    }
}

unsafe extern "system" fn main_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            if wparam.0 as i32 == HOTKEY_ID {
                toggle_panel(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn toggle_panel(owner: HWND) {
    unsafe {
        if let Some(hwnd) = PANEL_HWND {
            let visible = IsWindowVisible(hwnd).as_bool();
            if visible {
                start_hide_animation(hwnd);
            } else {
                refresh_panel(hwnd);
                let mut rect = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rect);
                start_show_animation(hwnd, rect.left, rect.top, rect.right - rect.left, rect.bottom - rect.top);
                force_foreground(hwnd);
            }
            return;
        }

        let hinstance = GetModuleHandleW(None).unwrap();
        let class_name = w!("TrayGuardianPanelWnd");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(panel_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            hbrBackground: hbrush_system(windows::Win32::Graphics::Gdi::COLOR_WINDOW.0),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let width = 260;
        let height = 260;
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            class_name,
            w!("TrayGuardian Panel"),
            WS_POPUP | WS_BORDER,
            0,
            0,
            width,
            height,
            owner,
            None,
            hinstance,
            None,
        )
        .unwrap();

        // Position near bottom-right, above the taskbar.
        let mut rect = RECT::default();
        let _ = GetWindowRect(GetDesktopWindow(), &mut rect);
        let x = rect.right - width - 20;
        let y = rect.bottom - height - 60;

        create_panel_controls(hwnd, hinstance.into());
        let _ = UpdateWindow(hwnd);
        PANEL_HWND = Some(hwnd);
        refresh_panel(hwnd);
        start_show_animation(hwnd, x, y, width, height);
        force_foreground(hwnd);
    }
}

unsafe fn create_panel_controls(hwnd: HWND, hinstance: windows::Win32::Foundation::HINSTANCE) {
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("BUTTON"),
        w!("Restart Tray"),
        WS_CHILD | WS_VISIBLE,
        10,
        10,
        230,
        30,
        hwnd,
        hmenu(ID_BTN_RESTART),
        hinstance,
        None,
    );
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("BUTTON"),
        w!("Open Chrome"),
        WS_CHILD | WS_VISIBLE,
        10,
        50,
        110,
        30,
        hwnd,
        hmenu(ID_BTN_CHROME),
        hinstance,
        None,
    );
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("BUTTON"),
        w!("Open Spotify"),
        WS_CHILD | WS_VISIBLE,
        130,
        50,
        110,
        30,
        hwnd,
        hmenu(ID_BTN_SPOTIFY),
        hinstance,
        None,
    );
    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        w!("Volume"),
        WS_CHILD | WS_VISIBLE,
        10,
        95,
        230,
        18,
        hwnd,
        hmenu(0),
        hinstance,
        None,
    );
    let trackbar = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("msctls_trackbar32"),
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE,
        10,
        115,
        230,
        30,
        hwnd,
        hmenu(ID_TRACKBAR_VOL),
        hinstance,
        None,
    )
    .unwrap();
    SendMessageW(trackbar, TBM_SETRANGE, WPARAM(1), LPARAM(100 << 16));

    let _ = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        w!("Battery: ..."),
        WS_CHILD | WS_VISIBLE,
        10,
        155,
        230,
        20,
        hwnd,
        hmenu(ID_STATIC_BATTERY),
        hinstance,
        None,
    );
}

unsafe fn refresh_panel(hwnd: HWND) {
    if let Ok(trackbar) = GetDlgItem(hwnd, ID_TRACKBAR_VOL as i32) {
        if let Some(vol) = get_master_volume() {
            SendMessageW(trackbar, TBM_SETPOS, WPARAM(1), LPARAM((vol * 100.0) as isize));
        }
    }
    if let Ok(battery_label) = GetDlgItem(hwnd, ID_STATIC_BATTERY as i32) {
        let text = wide(&get_battery_text());
        let _ = SetWindowTextW(battery_label, PCWSTR(text.as_ptr()));
    }
}

unsafe extern "system" fn panel_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as usize;
            match id {
                ID_BTN_RESTART => restart_tray_now(),
                ID_BTN_CHROME => launch_app(CHROME_PATH),
                ID_BTN_SPOTIFY => launch_app(SPOTIFY_PATH),
                _ => {}
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            let trackbar = HWND(lparam.0 as *mut _);
            let pos = SendMessageW(trackbar, TBM_GETPOS, WPARAM(0), LPARAM(0)).0;
            set_master_volume(pos as f32 / 100.0);
            LRESULT(0)
        }
        WM_ACTIVATE => {
            let active = (wparam.0 & 0xFFFF) as u32;
            if active == 0 && IsWindowVisible(hwnd).as_bool() {
                start_hide_animation(hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == ANIM_TIMER_ID {
                advance_animation(hwnd);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            start_hide_animation(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn create_bubble(hinstance: windows::Win32::Foundation::HINSTANCE) {
    let class_name = w!("TrayGuardianBubbleWnd");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(bubble_wndproc),
        hInstance: hinstance,
        lpszClassName: class_name,
        hbrBackground: hbrush_system(windows::Win32::Graphics::Gdi::COLOR_WINDOW.0),
        ..Default::default()
    };
    RegisterClassW(&wc);

    let mut rect = RECT::default();
    let _ = GetWindowRect(GetDesktopWindow(), &mut rect);
    let x = rect.right - BUBBLE_SIZE - 24;
    let y = rect.bottom - BUBBLE_SIZE - 24;

    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
        class_name,
        w!("TrayGuardian Bubble"),
        WS_POPUP,
        x,
        y,
        BUBBLE_SIZE,
        BUBBLE_SIZE,
        None,
        None,
        hinstance,
        None,
    );
    let Ok(hwnd) = hwnd else { return };

    BUBBLE_BITMAP = load_bubble_bitmap(BUBBLE_SIZE);

    if let Some(bmp) = BUBBLE_BITMAP {
        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(screen_dc);
        let old = SelectObject(mem_dc, bmp);
        let dst_pos = POINT { x, y };
        let src_pos = POINT { x: 0, y: 0 };
        let size = SIZE { cx: BUBBLE_SIZE, cy: BUBBLE_SIZE };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            None,
            Some(&dst_pos),
            Some(&size),
            mem_dc,
            Some(&src_pos),
            windows::Win32::Foundation::COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        SelectObject(mem_dc, old);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
    }

    let _ = ShowWindow(hwnd, SW_SHOWNA);
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, BUBBLE_SIZE, BUBBLE_SIZE, SWP_NOACTIVATE);

    BUBBLE_HWND = Some(hwnd);
    BUBBLE_BASE_X = x;
    BUBBLE_BASE_Y = y;
    BUBBLE_START = Some(Instant::now());
    SetTimer(hwnd, BUBBLE_TIMER_ID, 10, None);
}

unsafe fn advance_bubble_bob(hwnd: HWND) {
    let Some(start) = BUBBLE_START else { return };
    let elapsed_ms = start.elapsed().as_millis() as f64;
    let phase = (elapsed_ms % BUBBLE_BOB_PERIOD_MS) / BUBBLE_BOB_PERIOD_MS;
    let offset = (BUBBLE_BOB_AMPLITUDE * (phase * std::f64::consts::TAU).sin()) as i32;
    let _ = SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        BUBBLE_BASE_X,
        BUBBLE_BASE_Y + offset,
        BUBBLE_SIZE,
        BUBBLE_SIZE,
        SWP_NOACTIVATE,
    );
}

unsafe extern "system" fn bubble_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONUP => {
            if let Some(main_hwnd) = MAIN_HWND {
                toggle_panel(main_hwnd);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == BUBBLE_TIMER_ID {
                advance_bubble_bob(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(bmp) = BUBBLE_BITMAP.take() {
                let _ = DeleteObject(bmp);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
