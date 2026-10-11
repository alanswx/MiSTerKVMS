//! Keyboard and mouse capture on Windows, on a thread of its own:
//!
//! - A low-level keyboard hook sees and swallows every key, including
//!   Alt+Tab, the Windows key and Alt+F4 (not Ctrl+Alt+Del or Win+L, which
//!   Windows keeps). Keys are identified by scan code, so the layout doesn't
//!   matter.
//! - A low-level mouse hook swallows the mouse so nothing on the PC reacts;
//!   the pointer stays put, hidden over a 1-pixel window of ours.
//! - Movement comes from Raw Input (the mouse's own counts) in raw mode, or
//!   from the hook (Windows-accelerated) otherwise.
//!
//! Capture ends on the release chord, when our window loses the foreground,
//! or when `stop` is called.

use std::cell::RefCell;
use std::mem::size_of;
use std::ptr::null_mut;
use std::sync::mpsc;
use std::sync::{Arc, Once};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_NUMLOCK, VK_PAUSE};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, MOUSE_VIRTUAL_DESKTOP,
    RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEMOUSE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::input::{KeyResult, MouseButton};
use crate::keymap::scancode_to_hid;
use crate::session::MisterSession;

/// Where to capture.
#[derive(Debug, Clone, Copy)]
pub struct CaptureTarget {
    /// The app's top-level window; capture ends when it isn't in front.
    pub window: isize,
    /// The video area in screen pixels; the hidden pointer sits at its centre.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// Raw Input movement instead of Windows-accelerated movement.
    pub raw_mouse: bool,
}

pub struct WinCapture {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl WinCapture {
    /// Start capturing into `session`. `on_end` runs on the capture thread
    /// after everything is released, however capture ended.
    pub fn start(
        target: CaptureTarget,
        session: Arc<MisterSession>,
        on_end: Box<dyn FnOnce() + Send>,
    ) -> Result<Self, String> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("win-capture".into())
            .spawn(move || run(target, session, on_end, ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self { thread_id, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("capture thread failed".into()),
        }
    }

    /// End capture and wait until everything is released.
    pub fn stop(mut self) {
        self.shutdown();
    }

    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    fn shutdown(&mut self) {
        if let Some(t) = self.thread.take() {
            // SAFETY: posting to a thread id we own; harmless if it has exited.
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
            if t.thread().id() != thread::current().id() {
                let _ = t.join();
            }
        }
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// State the hook and window procedures reach (they run on this thread).
struct Ctx {
    session: Arc<MisterSession>,
    target: CaptureTarget,
    center: POINT,
    /// Display scaling where the pointer sits (2.0 at 200%). Accelerated
    /// movement is divided by it, so it's in points like on macOS and the
    /// guest pointer moves as far as the Windows one would.
    pixels_per_point: f64,
    /// A Left Ctrl press held back to see if it's AltGr's fake Ctrl
    /// (same timestamp as the Right Alt that follows): (time, scan).
    pending_ctrl: Option<u32>,
    /// Last absolute raw position (tablets, remote desktop).
    last_absolute: Option<(i32, i32)>,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

const TIMER_FOCUS: usize = 1;
const CLASS_NAME: &[u16] = &utf16z::<19>("MiSTerKVMS.Capture");

const fn utf16z<const N: usize>(s: &str) -> [u16; N] {
    let bytes = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < bytes.len() {
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

fn run(
    target: CaptureTarget,
    session: Arc<MisterSession>,
    on_end: Box<dyn FnOnce() + Send>,
    ready: mpsc::Sender<Result<u32, String>>,
) {
    // SAFETY: Win32 calls on this thread with valid arguments; the hooks and
    // window are removed before the thread exits.
    unsafe {
        let module = GetModuleHandleW(null_mut());
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: module,
                lpszClassName: CLASS_NAME.as_ptr(),
                // No class cursor: the pointer is hidden over this window.
                hCursor: null_mut(),
                ..Default::default()
            };
            RegisterClassExW(&class);
        });

        let mut original = POINT::default();
        GetCursorPos(&mut original);
        let center = POINT { x: target.x + target.width / 2, y: target.y + target.height / 2 };

        // A 1-pixel window under the pointer: it hides the pointer and
        // receives Raw Input. Never activated, so focus stays with the app.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_LAYERED,
            CLASS_NAME.as_ptr(),
            null_mut(),
            WS_POPUP,
            center.x,
            center.y,
            1,
            1,
            target.window as HWND,
            null_mut(),
            module,
            null_mut(),
        );
        if hwnd.is_null() {
            let _ = ready.send(Err("could not create the capture window".into()));
            on_end();
            return;
        }
        SetLayeredWindowAttributes(hwnd, 0, 1, LWA_ALPHA);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        CTX.with(|c| {
            *c.borrow_mut() = Some(Ctx {
                session: session.clone(),
                target,
                center,
                pixels_per_point: pixels_per_point(center),
                pending_ctrl: None,
                last_absolute: None,
            })
        });

        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
        if keyboard.is_null() || mouse.is_null() {
            for hook in [keyboard, mouse] {
                if !hook.is_null() {
                    UnhookWindowsHookEx(hook);
                }
            }
            DestroyWindow(hwnd);
            CTX.with(|c| c.borrow_mut().take());
            let _ = ready.send(Err("could not install the keyboard/mouse hooks".into()));
            on_end();
            return;
        }
        let raw = target.raw_mouse && register_raw_mouse(hwnd, false);

        SetCursorPos(center.x, center.y);
        clip(center);
        SetCursor(null_mut());
        SetTimer(hwnd, TIMER_FOCUS, 100, None);
        crate::log(&format!(
            "capture: start (raw mouse {}, display scale {:.2})",
            if raw { "on" } else { "off" },
            pixels_per_point(center)
        ));
        let _ = ready.send(Ok(GetCurrentThreadId()));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        KillTimer(hwnd, TIMER_FOCUS);
        if raw {
            register_raw_mouse(null_mut(), true);
        }
        UnhookWindowsHookEx(keyboard);
        UnhookWindowsHookEx(mouse);
        ClipCursor(std::ptr::null());
        DestroyWindow(hwnd);
        SetCursorPos(original.x, original.y);
        CTX.with(|c| c.borrow_mut().take());
        session.release_all();
        crate::log("capture: end");
        on_end();
    }
}

/// Display scaling of the monitor at `at` (1.0 = 100%).
fn pixels_per_point(at: POINT) -> f64 {
    let (mut x, mut y) = (96u32, 96u32);
    // SAFETY: plain queries with out-parameters.
    let ok = unsafe { GetDpiForMonitor(MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST), MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    if ok < 0 || x == 0 {
        1.0
    } else {
        x as f64 / 96.0
    }
}

unsafe fn register_raw_mouse(hwnd: HWND, remove: bool) -> bool {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01, // generic desktop
        usUsage: 0x02,     // mouse
        dwFlags: if remove { RIDEV_REMOVE } else { RIDEV_INPUTSINK },
        hwndTarget: hwnd,
    };
    unsafe { RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) != 0 }
}

unsafe fn clip(center: POINT) {
    let rect = RECT { left: center.x, top: center.y, right: center.x + 1, bottom: center.y + 1 };
    unsafe { ClipCursor(&rect) };
}

fn with_ctx<R>(f: impl FnOnce(&mut Ctx) -> R) -> Option<R> {
    CTX.with(|c| c.borrow_mut().as_mut().map(f))
}

fn end_capture() {
    // SAFETY: ends this thread's message loop.
    unsafe { PostQuitMessage(0) };
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lparam points at a KBDLLHOOKSTRUCT.
        let k = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if with_ctx(|ctx| key(ctx, k, down)).is_some() {
            return 1; // swallowed: Windows and other apps don't see it
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn key(ctx: &mut Ctx, k: &KBDLLHOOKSTRUCT, down: bool) {
    let extended = k.flags & LLKHF_EXTENDED != 0;
    // AltGr on non-US layouts: Windows adds a Left Ctrl press before Right
    // Alt, marked with scan code 0x21D, or else with the same timestamp.
    if k.scanCode & 0x200 != 0 {
        return;
    }
    let is_left_ctrl = k.scanCode == 0x1D && !extended;
    if let Some(time) = ctx.pending_ctrl.take() {
        let is_altgr = k.scanCode == 0x38 && extended && down && k.time == time;
        if !is_altgr {
            send_key(ctx, 0xE0, true);
        }
    }
    if is_left_ctrl && down && k.flags & LLKHF_INJECTED == 0 {
        ctx.pending_ctrl = Some(k.time);
        return;
    }
    let usage = match k.vkCode as u16 {
        VK_PAUSE => Some(0x48),
        VK_NUMLOCK => Some(0x53),
        _ => scancode_to_hid(k.scanCode as u16, extended),
    };
    if let Some(usage) = usage {
        send_key(ctx, usage, down);
    }
}

fn send_key(ctx: &mut Ctx, usage: u16, down: bool) {
    if ctx.session.key_event(usage, down) == KeyResult::ExitCapture {
        end_capture();
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lparam points at an MSLLHOOKSTRUCT.
        let m = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        if with_ctx(|ctx| mouse(ctx, wparam as u32, m)).is_some() {
            return 1;
        }
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn mouse(ctx: &mut Ctx, msg: u32, m: &MSLLHOOKSTRUCT) {
    if ctx.pending_ctrl.take().is_some() {
        send_key(ctx, 0xE0, true);
    }
    let button = |b, down| ctx.session.mouse_button(b, down);
    match msg {
        WM_MOUSEMOVE if !ctx.target.raw_mouse => {
            // The pointer doesn't move (we swallow the event), so where it
            // would have gone is the accelerated movement.
            let (dx, dy) = (m.pt.x - ctx.center.x, m.pt.y - ctx.center.y);
            if dx != 0 || dy != 0 {
                let scale = ctx.pixels_per_point;
                ctx.session.mouse_move(dx as f64 / scale, dy as f64 / scale);
            }
        }
        WM_LBUTTONDOWN => button(MouseButton::Left, true),
        WM_LBUTTONUP => button(MouseButton::Left, false),
        WM_RBUTTONDOWN => button(MouseButton::Right, true),
        WM_RBUTTONUP => button(MouseButton::Right, false),
        WM_MBUTTONDOWN => button(MouseButton::Middle, true),
        WM_MBUTTONUP => button(MouseButton::Middle, false),
        _ => {} // wheel, X buttons: mrext has no command for them yet
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            // SAFETY: lparam is the HRAWINPUT for this message.
            unsafe { raw_input(lparam as HRAWINPUT) };
            0
        }
        WM_TIMER if wparam == TIMER_FOCUS => {
            let in_front = with_ctx(|ctx| unsafe {
                let fg = GetForegroundWindow();
                let root = GetAncestor(fg, GA_ROOT);
                // Windows drops the clip on some focus changes; put it back.
                clip(ctx.center);
                root == ctx.target.window as HWND || fg == hwnd
            });
            if in_front == Some(false) {
                end_capture();
            }
            0
        }
        WM_SETCURSOR => {
            unsafe { SetCursor(null_mut()) };
            1
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn raw_input(handle: HRAWINPUT) {
    let mut data = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    let read = unsafe {
        GetRawInputData(handle, RID_INPUT, (&mut data as *mut RAWINPUT).cast(), &mut size, size_of::<RAWINPUTHEADER>() as u32)
    };
    if read == u32::MAX || data.header.dwType != RIM_TYPEMOUSE {
        return;
    }
    // SAFETY: dwType says this is mouse data.
    let m = unsafe { data.data.mouse };
    with_ctx(|ctx| {
        let (dx, dy) = if m.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
            // Tablets and remote desktop report positions (0..65535 across
            // the screen or the virtual desktop); turn them into movement.
            let (w, h) = unsafe {
                if m.usFlags & MOUSE_VIRTUAL_DESKTOP != 0 {
                    (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN))
                } else {
                    (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
                }
            };
            let at = (m.lLastX * w / 65535, m.lLastY * h / 65535);
            let delta = ctx.last_absolute.map_or((0, 0), |(x, y)| (at.0 - x, at.1 - y));
            ctx.last_absolute = Some(at);
            // Screen pixels, like the accelerated path.
            (delta.0 as f64 / ctx.pixels_per_point, delta.1 as f64 / ctx.pixels_per_point)
        } else {
            // The mouse's own counts, unscaled (as on macOS).
            (m.lLastX as f64, m.lLastY as f64)
        };
        if dx != 0.0 || dy != 0.0 {
            ctx.session.mouse_move(dx, dy);
        }
    });
}
