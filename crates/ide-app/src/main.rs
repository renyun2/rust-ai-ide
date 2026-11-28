#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent_panel;
mod canvas;
mod external_open;
mod ide_state;
mod image_preview;
mod markdown;
mod persistence;
mod svg_icon;
#[cfg(not(target_os = "windows"))]
mod text_cosmic;
mod text_input;

use crate::canvas::{
    detect_system_dpi_scale, render_ri_icon_rgba, save_ri_icon_ico, save_ri_icon_png,
    SurfaceCanvas,
};
use crate::external_open::{
    ensure_editor_shims, ensure_windows_url_protocol, infer_workspace_for_file, parse_cli_args,
    path_is_inside_workspace, start_ipc_server, try_forward_via_ipc, EditorIntegrationPaths,
    ExternalOpenTarget,
};
use crate::ide_state::{
    duplicate_open_count, session_windows_after_close, workspace_paths_equal,
    write_windows_clipboard_text, FocusTarget, IdeApplicationState, IdeCursorKind,
    TextInputNavAction, WindowControlCmd,
};
use anyhow::Result;
use ide_core::{IdeAction, IdeaTheme, KeymapScheme, MainMenuCategory, ThemeKind, ToolWindowId};
use softbuffer::{Context, Surface};
use std::collections::{HashMap, HashSet};
use std::env;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{ElementState, Event, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::keyboard::{Key, ModifiersState};
use tao::window::{CursorIcon, Icon, ResizeDirection, Window, WindowBuilder, WindowId, RGBA};

#[cfg(target_os = "windows")]
fn cursor_pos_in_window(
    window: &tao::window::Window,
    canvas: &SurfaceCanvas,
    fallback: (i32, i32),
) -> (i32, i32) {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::Graphics::Gdi::ScreenToClient;

    #[link(name = "user32")]
    extern "system" {
        fn GetCursorPos(lpPoint: *mut POINT) -> i32;
    }

    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        if GetCursorPos(&mut pt) != 0 {
            let hwnd = HWND(window.hwnd() as *mut core::ffi::c_void);
            if ScreenToClient(hwnd, &mut pt).as_bool() {
                return canvas.to_logical_pos(pt.x as f64, pt.y as f64);
            }
        }
    }
    fallback
}

#[cfg(not(target_os = "windows"))]
fn cursor_pos_in_window(
    _window: &tao::window::Window,
    _canvas: &SurfaceCanvas,
    fallback: (i32, i32),
) -> (i32, i32) {
    fallback
}

#[cfg(target_os = "windows")]
fn trim_process_working_set() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
        fn SetProcessWorkingSetSize(
            h_process: *mut core::ffi::c_void,
            dw_minimum_working_set_size: usize,
            dw_maximum_working_set_size: usize,
        ) -> i32;
    }
    unsafe {
        let proc_h = GetCurrentProcess();
        let _ = SetProcessWorkingSetSize(proc_h, usize::MAX, usize::MAX);
    }
}

#[cfg(not(target_os = "windows"))]
fn trim_process_working_set() {}

#[derive(Debug, Clone)]
enum IdeWakeupEvent {
    Wakeup(WindowId),
    ExternalOpen(Vec<ExternalOpenTarget>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowCommand {
    None,
    CloseWindow,
}

#[derive(Debug, Clone, Copy)]
struct ManualResizeState {
    dir: ResizeDirection,
    start_cursor_screen: (i32, i32),
    start_pos: (i32, i32),
    start_size: (u32, u32),
}

struct IdeWindowHost {
    window: Rc<Window>,
    _context: Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    app_state: IdeApplicationState,
    surface_canvas: SurfaceCanvas,
    cursor_pos: (i32, i32),
    last_cursor_icon: CursorIcon,
    modifiers: ModifiersState,
    left_mouse_down: bool,
    manual_resize: Option<ManualResizeState>,
    last_shift_press: Option<Instant>,
    last_titlebar_click: Option<(Instant, (i32, i32))>,
    last_input_click: Option<(Instant, (i32, i32), u8)>,
    last_pty_rev: u64,
    pending_redraw: bool,
    is_focused: bool,
    is_minimized: bool,
    unfocused_since: Option<Instant>,
    idle_trimmed: bool,
    minimized_trimmed: bool,
    last_wheel_time: Option<Instant>,
    last_wheel_dir: f32,
    wheel_accum_y: f32,
    wheel_accum_x: f32,
    last_ime_physical: Option<(i32, i32)>,
    last_bg_rgb: u32,
    macos_ime_handled_keydown: bool,
}

#[inline]
fn should_insert_key_text(
    require_event_text: bool,
    ime_already_committed: bool,
    event_text_present: bool,
) -> bool {
    if !require_event_text {
        return true;
    }
    !ime_already_committed && event_text_present
}

#[inline]
fn sanitize_insertable_text(raw: &str) -> String {
    raw.chars()
        .filter(|&c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect()
}

fn resolve_keyboard_insert_text(
    logical_key: &Key<'_>,
    event_text: Option<&str>,
) -> Option<String> {
    match logical_key {
        Key::Character(ch) => {
            if let Some(t) = event_text {
                let clean = sanitize_insertable_text(t);
                if !clean.is_empty() {
                    return Some(clean);
                }
            }
            if cfg!(any(target_os = "windows", target_os = "macos")) {
                return None;
            }
            let clean = sanitize_insertable_text(ch);
            if clean.is_empty() {
                None
            } else {
                Some(clean)
            }
        }
        Key::Unidentified(_) | Key::Process => {
            let clean = sanitize_insertable_text(event_text?);
            if clean.is_empty() {
                None
            } else {
                Some(clean)
            }
        }
        _ => None,
    }
}

#[cfg(target_os = "windows")]
mod win_text_hook {
    use super::{sanitize_insertable_text, IdeWakeupEvent};
    use core::ffi::c_void;
    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};
    use std::time::{Duration, Instant};
    use tao::event_loop::EventLoopProxy;
    use tao::platform::windows::WindowExtWindows;
    use tao::window::{Window, WindowId};

    const GWLP_WNDPROC: i32 = -4;
    const WM_NCDESTROY: u32 = 0x0082;
    const WM_KEYDOWN: u32 = 0x0100;
    const WM_KEYUP: u32 = 0x0101;
    const WM_CHAR: u32 = 0x0102;
    const WM_DEADCHAR: u32 = 0x0103;
    const WM_SYSKEYDOWN: u32 = 0x0104;
    const WM_SYSKEYUP: u32 = 0x0105;
    const WM_SYSCHAR: u32 = 0x0106;
    const WM_SYSDEADCHAR: u32 = 0x0107;
    const WM_UNICHAR: u32 = 0x0109;
    const UNICODE_NOCHAR: usize = 0xFFFF;
    const WM_IME_STARTCOMPOSITION: u32 = 0x010D;
    const WM_IME_ENDCOMPOSITION: u32 = 0x010E;
    const WM_IME_COMPOSITION: u32 = 0x010F;
    const WM_IME_CHAR: u32 = 0x0286;
    const GCS_RESULTSTR: u32 = 0x0800;
    const WM_KEYFIRST: u32 = 0x0100;
    const WM_KEYLAST: u32 = 0x0109;
    const PM_NOREMOVE: u32 = 0x0000;
    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12;
    const VK_F4: usize = 0x73;

    #[repr(C)]
    struct WinPoint {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct WinMsg {
        hwnd: *mut c_void,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt: WinPoint,
    }

    #[link(name = "user32")]
    extern "system" {
        #[cfg(target_pointer_width = "64")]
        fn SetWindowLongPtrW(hwnd: *mut c_void, n_index: i32, dw_new_long: isize) -> isize;
        #[cfg(target_pointer_width = "32")]
        #[link_name = "SetWindowLongW"]
        fn SetWindowLongPtrW(hwnd: *mut c_void, n_index: i32, dw_new_long: isize) -> isize;
        fn CallWindowProcW(
            lp_prev_wnd_func: isize,
            hwnd: *mut c_void,
            msg: u32,
            wparam: usize,
            lparam: isize,
        ) -> isize;
        fn PeekMessageW(
            lp_msg: *mut WinMsg,
            hwnd: *mut c_void,
            msg_filter_min: u32,
            msg_filter_max: u32,
            remove_msg: u32,
        ) -> i32;
        fn GetKeyState(n_virt_key: i32) -> i16;
    }

    #[link(name = "imm32")]
    extern "system" {
        fn ImmGetContext(hwnd: *mut c_void) -> *mut c_void;
        fn ImmReleaseContext(hwnd: *mut c_void, himc: *mut c_void) -> i32;
        fn ImmGetCompositionStringW(
            himc: *mut c_void,
            dw_index: u32,
            lp_buf: *mut c_void,
            dw_buf_len: u32,
        ) -> i32;
    }

    struct WinHookEntry {
        old_wndproc: isize,
        window_id: WindowId,
        proxy: EventLoopProxy<IdeWakeupEvent>,
        in_ime_comp: bool,
        ime_forwarding_depth: u32,
        tao_owns_chars: bool,
        pending_high_surrogate: Option<u16>,
        pending_text: String,
        committed_in_comp: String,
        last_ime_result: Option<String>,
        suppressed_tao_ime_texts: VecDeque<String>,
        recent_ime_echo: Option<(String, Instant)>,
    }

    thread_local! {
        static HOOKS: RefCell<HashMap<isize, WinHookEntry>> = RefCell::new(HashMap::new());
    }

    unsafe fn peek_next_kbd_msg(hwnd: *mut c_void) -> Option<u32> {
        let mut msg = std::mem::MaybeUninit::<WinMsg>::uninit();
        if PeekMessageW(msg.as_mut_ptr(), hwnd, WM_KEYFIRST, WM_KEYLAST, PM_NOREMOVE) != 0 {
            Some(msg.assume_init().message)
        } else {
            None
        }
    }

    unsafe fn imm_get_result_str(hwnd: *mut c_void) -> Option<String> {
        let himc = ImmGetContext(hwnd);
        if himc.is_null() {
            return None;
        }
        let byte_len = ImmGetCompositionStringW(himc, GCS_RESULTSTR, std::ptr::null_mut(), 0);
        let out = if byte_len > 0 {
            let u16_len = (byte_len as usize) / 2;
            let mut buf = vec![0u16; u16_len];
            let got = ImmGetCompositionStringW(
                himc,
                GCS_RESULTSTR,
                buf.as_mut_ptr() as *mut c_void,
                (u16_len * 2) as u32,
            );
            if got > 0 {
                let actual = (got as usize / 2).min(buf.len());
                Some(String::from_utf16_lossy(&buf[..actual]))
            } else {
                None
            }
        } else {
            None
        };
        let _ = ImmReleaseContext(hwnd, himc);
        out
    }

    fn decode_utf16_unit(entry: &mut WinHookEntry, unit: u16) -> Option<char> {
        if (0xD800..=0xDBFF).contains(&unit) {
            entry.pending_high_surrogate = Some(unit);
            return None;
        }
        if (0xDC00..=0xDFFF).contains(&unit) {
            if let Some(hi) = entry.pending_high_surrogate.take() {
                for decoded in char::decode_utf16([hi, unit]) {
                    if let Ok(ch) = decoded {
                        if !ch.is_control() || ch == '\n' {
                            return Some(ch);
                        }
                    }
                }
            }
            return None;
        }
        entry.pending_high_surrogate = None;
        if let Some(ch) = char::from_u32(unit as u32) {
            if !ch.is_control() || ch == '\n' {
                return Some(ch);
            }
        }
        None
    }

    fn consume_if_recent_ime_echo(entry: &mut WinHookEntry, ch: char) -> bool {
        let Some((ref mut pending_echo, at)) = entry.recent_ime_echo else {
            return false;
        };
        if at.elapsed() > Duration::from_millis(80) || pending_echo.is_empty() {
            entry.recent_ime_echo = None;
            return false;
        }
        if let Some(first) = pending_echo.chars().next() {
            if first == ch {
                pending_echo.drain(..first.len_utf8());
                if pending_echo.is_empty() {
                    entry.recent_ime_echo = None;
                }
                return true;
            }
        }
        false
    }

    fn push_unowned_char(entry: &mut WinHookEntry, ch: char) {
        if !consume_if_recent_ime_echo(entry, ch) {
            entry.pending_text.push(ch);
        }
    }

    pub fn install(window: &Window, proxy: EventLoopProxy<IdeWakeupEvent>) {
        let hwnd = window.hwnd() as *mut c_void;
        let key = hwnd as isize;
        let installed = HOOKS.with(|m| m.borrow().contains_key(&key));
        if installed {
            return;
        }
        unsafe {
            let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, hook_wndproc as *const () as isize);
            if prev != 0 {
                HOOKS.with(|m| {
                    m.borrow_mut().insert(
                        key,
                        WinHookEntry {
                            old_wndproc: prev,
                            window_id: window.id(),
                            proxy,
                            in_ime_comp: false,
                            ime_forwarding_depth: 0,
                            tao_owns_chars: false,
                            pending_high_surrogate: None,
                            pending_text: String::new(),
                            committed_in_comp: String::new(),
                            last_ime_result: None,
                            suppressed_tao_ime_texts: VecDeque::new(),
                            recent_ime_echo: None,
                        },
                    );
                });
            }
        }
    }

    pub fn drain_pending_text(window: &Window) -> Option<String> {
        let key = window.hwnd() as isize;
        HOOKS.with(|m| {
            let mut map = m.borrow_mut();
            let entry = map.get_mut(&key)?;
            if entry.pending_text.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut entry.pending_text))
            }
        })
    }

    pub fn take_suppressed_ime_text(window: &Window, text: &str) -> bool {
        let key = window.hwnd() as isize;
        HOOKS.with(|m| {
            let mut map = m.borrow_mut();
            let Some(entry) = map.get_mut(&key) else {
                return false;
            };
            if let Some(pos) = entry.suppressed_tao_ime_texts.iter().position(|s| s == text) {
                entry.suppressed_tao_ime_texts.drain(..=pos);
                true
            } else {
                false
            }
        })
    }

    unsafe extern "system" fn hook_wndproc(
        hwnd: *mut c_void,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        let key = hwnd as isize;
        let old_wndproc = HOOKS.with(|m| m.borrow().get(&key).map(|e| e.old_wndproc));
        let Some(old_wndproc) = old_wndproc else {
            return 0;
        };

        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if !(msg == WM_SYSKEYDOWN && wparam == VK_F4) {
                    let next_msg = peek_next_kbd_msg(hwnd);
                    let tao_will_own = next_msg.is_some_and(|nm| {
                        !matches!(nm, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP)
                    });
                    HOOKS.with(|m| {
                        if let Some(entry) = m.borrow_mut().get_mut(&key) {
                            entry.tao_owns_chars = tao_will_own;
                        }
                    });
                }
                CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam)
            }
            WM_DEADCHAR | WM_SYSDEADCHAR => {
                HOOKS.with(|m| {
                    if let Some(entry) = m.borrow_mut().get_mut(&key) {
                        entry.tao_owns_chars = false;
                    }
                });
                CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam)
            }
            WM_IME_STARTCOMPOSITION => {
                HOOKS.with(|m| {
                    if let Some(entry) = m.borrow_mut().get_mut(&key) {
                        entry.in_ime_comp = true;
                        entry.committed_in_comp.clear();
                        entry.last_ime_result = None;
                        entry.pending_high_surrogate = None;
                    }
                });
                CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam)
            }
            WM_IME_COMPOSITION => {
                if (lparam as u32 & GCS_RESULTSTR) != 0 {
                    if let Some(raw_res) = imm_get_result_str(hwnd) {
                        let clean = sanitize_insertable_text(&raw_res);
                        if !clean.is_empty() {
                            let wakeup = HOOKS.with(|m| {
                                if let Some(entry) = m.borrow_mut().get_mut(&key) {
                                    entry.pending_text.push_str(&clean);
                                    entry.committed_in_comp.push_str(&clean);
                                    entry.last_ime_result = Some(raw_res);
                                    entry.recent_ime_echo = Some((clean, Instant::now()));
                                    Some((entry.proxy.clone(), entry.window_id))
                                } else {
                                    None
                                }
                            });
                            if let Some((proxy, wid)) = wakeup {
                                let _ = proxy.send_event(IdeWakeupEvent::Wakeup(wid));
                            }
                        }
                    }
                }
                HOOKS.with(|m| {
                    if let Some(entry) = m.borrow_mut().get_mut(&key) {
                        entry.ime_forwarding_depth = entry.ime_forwarding_depth.saturating_add(1);
                    }
                });
                let res = CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam);
                HOOKS.with(|m| {
                    if let Some(entry) = m.borrow_mut().get_mut(&key) {
                        entry.ime_forwarding_depth = entry.ime_forwarding_depth.saturating_sub(1);
                    }
                });
                res
            }
            WM_IME_ENDCOMPOSITION => {
                let current_res = imm_get_result_str(hwnd);
                HOOKS.with(|m| {
                    if let Some(entry) = m.borrow_mut().get_mut(&key) {
                        entry.in_ime_comp = false;
                        let prev = entry.last_ime_result.take();
                        let committed = std::mem::take(&mut entry.committed_in_comp);
                        if let Some(cur) = current_res {
                            if !cur.is_empty()
                                && (prev.as_deref() == Some(cur.as_str())
                                    || (!committed.is_empty()
                                        && sanitize_insertable_text(&cur) == committed))
                            {
                                while entry.suppressed_tao_ime_texts.len() >= 16 {
                                    entry.suppressed_tao_ime_texts.pop_front();
                                }
                                entry.suppressed_tao_ime_texts.push_back(cur);
                            }
                        }
                        entry.ime_forwarding_depth = entry.ime_forwarding_depth.saturating_add(1);
                    }
                });
                let res = CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam);
                HOOKS.with(|m| {
                    if let Some(entry) = m.borrow_mut().get_mut(&key) {
                        entry.ime_forwarding_depth = entry.ime_forwarding_depth.saturating_sub(1);
                    }
                });
                res
            }
            WM_IME_CHAR => {
                let forwarding = HOOKS.with(|m| {
                    m.borrow()
                        .get(&key)
                        .is_some_and(|e| e.ime_forwarding_depth > 0)
                });
                if !forwarding {
                    let wakeup = HOOKS.with(|m| {
                        let mut map = m.borrow_mut();
                        let entry = map.get_mut(&key)?;
                        if let Some(ch) = decode_utf16_unit(entry, wparam as u16) {
                            push_unowned_char(entry, ch);
                        }
                        if entry.pending_text.is_empty() {
                            None
                        } else {
                            Some((entry.proxy.clone(), entry.window_id))
                        }
                    });
                    if let Some((proxy, wid)) = wakeup {
                        let _ = proxy.send_event(IdeWakeupEvent::Wakeup(wid));
                    }
                    return 0;
                }
                HOOKS.with(|m| {
                    if let Some(entry) = m.borrow_mut().get_mut(&key) {
                        entry.ime_forwarding_depth = entry.ime_forwarding_depth.saturating_add(1);
                    }
                });
                let res = CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam);
                HOOKS.with(|m| {
                    if let Some(entry) = m.borrow_mut().get_mut(&key) {
                        entry.ime_forwarding_depth = entry.ime_forwarding_depth.saturating_sub(1);
                    }
                });
                res
            }
            WM_CHAR | WM_SYSCHAR => {
                let (tao_owns, from_ime) = HOOKS.with(|m| {
                    m.borrow()
                        .get(&key)
                        .map(|e| (e.tao_owns_chars, e.ime_forwarding_depth > 0 || e.in_ime_comp))
                        .unwrap_or((false, false))
                });
                if tao_owns {
                    let res = CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam);
                    let more_char = matches!(peek_next_kbd_msg(hwnd), Some(WM_CHAR | WM_SYSCHAR));
                    if !more_char {
                        HOOKS.with(|m| {
                            if let Some(entry) = m.borrow_mut().get_mut(&key) {
                                entry.tao_owns_chars = false;
                            }
                        });
                    }
                    return res;
                }
                if !from_ime {
                    let ctrl_down = (GetKeyState(VK_CONTROL) as u16 & 0x8000) != 0;
                    let alt_down = (GetKeyState(VK_MENU) as u16 & 0x8000) != 0;
                    if !ctrl_down && !alt_down {
                        let wakeup = HOOKS.with(|m| {
                            let mut map = m.borrow_mut();
                            let entry = map.get_mut(&key)?;
                            if let Some(ch) = decode_utf16_unit(entry, wparam as u16) {
                                push_unowned_char(entry, ch);
                            }
                            if entry.pending_text.is_empty() {
                                None
                            } else {
                                Some((entry.proxy.clone(), entry.window_id))
                            }
                        });
                        if let Some((proxy, wid)) = wakeup {
                            let _ = proxy.send_event(IdeWakeupEvent::Wakeup(wid));
                        }
                        return 0;
                    }
                }
                CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam)
            }
            WM_UNICHAR => {
                if wparam == UNICODE_NOCHAR {
                    return 1;
                }
                if let Some(ch) = char::from_u32(wparam as u32) {
                    if !ch.is_control() || ch == '\n' {
                        let wakeup = HOOKS.with(|m| {
                            let mut map = m.borrow_mut();
                            let entry = map.get_mut(&key)?;
                            push_unowned_char(entry, ch);
                            if entry.pending_text.is_empty() {
                                None
                            } else {
                                Some((entry.proxy.clone(), entry.window_id))
                            }
                        });
                        if let Some((proxy, wid)) = wakeup {
                            let _ = proxy.send_event(IdeWakeupEvent::Wakeup(wid));
                        }
                        return 0;
                    }
                }
                CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam)
            }
            WM_NCDESTROY => {
                let res = CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam);
                HOOKS.with(|m| {
                    m.borrow_mut().remove(&key);
                });
                res
            }
            _ => CallWindowProcW(old_wndproc, hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod win_text_hook {
    use super::IdeWakeupEvent;
    use tao::event_loop::EventLoopProxy;
    use tao::window::Window;

    pub fn install(_window: &Window, _proxy: EventLoopProxy<IdeWakeupEvent>) {}

    pub fn drain_pending_text(_window: &Window) -> Option<String> {
        None
    }

    pub fn take_suppressed_ime_text(_window: &Window, _text: &str) -> bool {
        false
    }
}

#[inline]
fn is_non_visual_key(key: &Key<'_>) -> bool {
    matches!(
        key,
        Key::Process
            | Key::Shift
            | Key::Control
            | Key::Alt
            | Key::Super
            | Key::CapsLock
            | Key::NumLock
            | Key::ScrollLock
    )
}

#[inline]
fn rgb_to_tao_rgba(rgb: u32) -> RGBA {
    (
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
        255,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum SystemWheelMode {
    /// `tao` already scales `LineDelta` by OS settings (Windows `SPI_GETWHEELSCROLLLINES` = N, or macOS AppKit mouse speed).
    NativeLineDelta,
    /// Windows `SPI_GETWHEELSCROLLLINES == WHEEL_PAGESCROLL`: `tao` fell back to 3 lines/notch; map each notch to one viewport page.
    WindowsPageScroll,
    /// Linux GTK `event.delta()` emits wheel clicks (`±1.0` per detent); multiply by system/DE lines-per-notch.
    FixedLinesPerNotch(u32),
}

#[cfg(target_os = "windows")]
fn query_windows_wheel_scroll_lines() -> u32 {
    #[link(name = "user32")]
    extern "system" {
        fn SystemParametersInfoW(
            ui_action: u32,
            ui_param: u32,
            pv_param: *mut core::ffi::c_void,
            f_win_ini: u32,
        ) -> i32;
    }
    const SPI_GETWHEELSCROLLLINES: u32 = 0x0068;
    let mut lines: u32 = 3;
    unsafe {
        if SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            (&mut lines as *mut u32).cast::<core::ffi::c_void>(),
            0,
        ) != 0
        {
            lines
        } else {
            3
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_kde_wheel_scroll_lines(content: &str) -> Option<u32> {
    let mut in_kde_section = false;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_kde_section = line.eq_ignore_ascii_case("[KDE]");
            continue;
        }
        if in_kde_section {
            if let Some(val) = line.strip_prefix("WheelScrollLines=") {
                if let Ok(n) = val.trim().parse::<u32>() {
                    return Some(n.min(100));
                }
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn detect_linux_wheel_scroll_lines() -> u32 {
    if let Some(home) = env::var_os("HOME") {
        let kdeglobals = PathBuf::from(home).join(".config").join("kdeglobals");
        if let Ok(content) = std::fs::read_to_string(kdeglobals) {
            if let Some(n) = parse_kde_wheel_scroll_lines(&content) {
                return n;
            }
        }
    }
    3
}

fn current_system_wheel_mode() -> SystemWheelMode {
    #[cfg(target_os = "windows")]
    {
        const WHEEL_PAGESCROLL: u32 = u32::MAX;
        if query_windows_wheel_scroll_lines() == WHEEL_PAGESCROLL {
            SystemWheelMode::WindowsPageScroll
        } else {
            SystemWheelMode::NativeLineDelta
        }
    }
    #[cfg(target_os = "macos")]
    {
        SystemWheelMode::NativeLineDelta
    }
    #[cfg(target_os = "linux")]
    {
        SystemWheelMode::FixedLinesPerNotch(detect_linux_wheel_scroll_lines())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        SystemWheelMode::FixedLinesPerNotch(3)
    }
}

/// Converts a `tao` `MouseScrollDelta` into `(horiz_px, vert_lines)` honoring OS wheel settings.
fn resolve_wheel_scroll_deltas(
    delta: &MouseScrollDelta,
    mode: SystemWheelMode,
    page_lines: usize,
    scale_factor: f32,
    shift_held: bool,
) -> (f32, f32) {
    let scale = scale_factor.max(0.5);
    let px_per_line = (22.0 * scale).max(12.0);
    match *delta {
        MouseScrollDelta::LineDelta(x, y) => {
            let raw_dx = -x;
            let raw_dy = -y;
            let vert_lines = match mode {
                SystemWheelMode::NativeLineDelta => raw_dy,
                SystemWheelMode::WindowsPageScroll => {
                    let page = page_lines.saturating_sub(1).max(1) as f32;
                    (raw_dy / 3.0) * page
                }
                SystemWheelMode::FixedLinesPerNotch(n) => raw_dy * (n as f32),
            };
            if shift_held && raw_dx.abs() < 0.01 {
                (vert_lines * 12.0, 0.0)
            } else {
                let horiz_px = match mode {
                    SystemWheelMode::NativeLineDelta | SystemWheelMode::WindowsPageScroll => {
                        raw_dx * 12.0
                    }
                    SystemWheelMode::FixedLinesPerNotch(n) => raw_dx * 12.0 * (n as f32),
                };
                (horiz_px, vert_lines)
            }
        }
        MouseScrollDelta::PixelDelta(p) => {
            let raw_dx_px = (-p.x as f32) / scale;
            let raw_dy_lines = (-p.y as f32) / px_per_line;
            if shift_held && raw_dx_px.abs() < 0.01 {
                ((-p.y as f32) / scale, 0.0)
            } else {
                (raw_dx_px, raw_dy_lines)
            }
        }
        _ => (0.0, 0.0),
    }
}

fn consume_wheel_accum(accum: &mut f32) -> i32 {
    let mut whole = accum.trunc() as i32;
    let rem = *accum - whole as f32;
    if (rem.abs() - 1.0).abs() < 1e-4 {
        whole += if rem > 0.0 { 1 } else { -1 };
        *accum = 0.0;
    } else {
        *accum = rem;
    }
    whole
}

fn spawn_ide_window(
    elwt: &EventLoopWindowTarget<IdeWakeupEvent>,
    proxy: &EventLoopProxy<IdeWakeupEvent>,
    workspace_root: PathBuf,
    storage_dir: Option<&Path>,
    external_integration: Option<&EditorIntegrationPaths>,
    window_icon: Option<Icon>,
    taskbar_icon: Option<Icon>,
    initial_theme: IdeaTheme,
    init_log_w: f64,
    init_log_h: f64,
    init_maximized: bool,
) -> Result<IdeWindowHost> {
    let builder = WindowBuilder::new()
        .with_title(format!(
            "{} — Rust AI IDE",
            workspace_root
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("RI")
        ))
        .with_visible(false)
        .with_background_color(rgb_to_tao_rgba(initial_theme.window_bg))
        .with_window_icon(window_icon)
        .with_decorations(false)
        .with_resizable(true)
        .with_maximized(init_maximized)
        .with_inner_size(LogicalSize::new(init_log_w, init_log_h));
    #[cfg(target_os = "windows")]
    let builder = {
        use tao::platform::windows::WindowBuilderExtWindows;
        builder.with_taskbar_icon(taskbar_icon)
    };
    #[cfg(not(target_os = "windows"))]
    let _ = taskbar_icon;
    let window = Rc::new(builder.build(elwt)?);
    apply_windows_class_icons(&window);
    apply_windows11_window_style(&window, initial_theme.kind != ThemeKind::IslandsLight);
    win_text_hook::install(&window, proxy.clone());

    let context = Context::new(window.clone())
        .map_err(|e| anyhow::anyhow!("Failed to initialize softbuffer context: {e}"))?;
    let surface = Surface::new(&context, window.clone())
        .map_err(|e| anyhow::anyhow!("Failed to create rendering surface: {e}"))?;

    let wid = window.id();
    let proxy_w = proxy.clone();
    let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = proxy_w.send_event(IdeWakeupEvent::Wakeup(wid));
    });

    let mut app_state = if storage_dir.is_some() {
        IdeApplicationState::new_with_wakeup_deferred_mobile_probe(workspace_root, Some(wakeup))
    } else {
        IdeApplicationState::new_with_wakeup(workspace_root, Some(wakeup))
    };
    if let Some(dir) = storage_dir {
        app_state.restore_from_persistence_dir(dir.to_path_buf());
    }
    if let Some(paths) = external_integration {
        app_state.set_external_integration(paths.clone());
    }
    let last_bg_rgb = app_state.theme.window_bg;
    window.set_background_color(Some(rgb_to_tao_rgba(last_bg_rgb)));
    apply_windows11_window_style(&window, app_state.theme.kind != ThemeKind::IslandsLight);
    let _ = app_state.take_workspace_title_dirty();
    window.set_title(&app_state.taskbar_title());
    let init_os_scale = (window.scale_factor() as f32).clamp(0.75, 4.0);
    app_state.system_dpi_scale = init_os_scale;
    app_state.window_maximized = window.is_maximized();
    let init_phys = window.inner_size();
    let surface_canvas = SurfaceCanvas::new_with_scale(
        init_phys.width.max(1) as usize,
        init_phys.height.max(1) as usize,
        app_state.effective_scale(),
    );
    app_state.adapt_panels_for_logical_width(surface_canvas.width as i32);
    let last_pty_rev = app_state.active_terminal_revision();

    Ok(IdeWindowHost {
        window,
        _context: context,
        surface,
        app_state,
        surface_canvas,
        cursor_pos: (0, 0),
        last_cursor_icon: CursorIcon::Default,
        modifiers: ModifiersState::default(),
        left_mouse_down: false,
        manual_resize: None,
        last_shift_press: None,
        last_titlebar_click: None,
        last_input_click: None,
        last_pty_rev,
        pending_redraw: true,
        is_focused: true,
        is_minimized: false,
        unfocused_since: None,
        idle_trimmed: false,
        minimized_trimmed: false,
        last_wheel_time: None,
        last_wheel_dir: 0.0,
        wheel_accum_y: 0.0,
        wheel_accum_x: 0.0,
        last_ime_physical: None,
        last_bg_rgb,
        macos_ime_handled_keydown: false,
    })
}

fn reveal_ide_window(host: &mut IdeWindowHost, play_startup_anim: bool) {
    if play_startup_anim {
        host.app_state.restart_startup_animation();
    }
    host.window.set_visible(true);
    host.render_frame();
    host.window.request_redraw();
}

fn sync_ime_position(
    window: &tao::window::Window,
    state: &IdeApplicationState,
    canvas: &SurfaceCanvas,
    last: &mut Option<(i32, i32)>,
) {
    let (px, py) = canvas.to_physical_pos(state.ime_spot.0, state.ime_spot.1);
    if let Some((lx, ly)) = *last {
        if (px - lx).abs() <= 2 && (py - ly).abs() <= 2 {
            return;
        }
    }
    *last = Some((px, py));
    window.set_ime_position(PhysicalPosition::new(px, py));
}

fn load_ri_icon_sized(size: u32) -> Option<Icon> {
    let px = size.max(16);
    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::IconExtWindows;
        if let Ok(icon) = Icon::from_resource(1, Some(PhysicalSize::new(px, px))) {
            return Some(icon);
        }
    }
    let rgba = render_ri_icon_rgba(px);
    if rgba.is_empty() {
        return None;
    }
    Icon::from_rgba(rgba, px, px).ok()
}

#[cfg(target_os = "windows")]
fn apply_windows_class_icons(window: &tao::window::Window) {
    use core::ffi::c_void;
    use tao::platform::windows::WindowExtWindows;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(lp_module_name: *const u16) -> *mut c_void;
    }
    #[link(name = "user32")]
    extern "system" {
        fn LoadImageW(
            h_inst: *mut c_void,
            name: *const u16,
            r#type: u32,
            cx: i32,
            cy: i32,
            fu_load: u32,
        ) -> *mut c_void;
        fn GetSystemMetrics(n_index: i32) -> i32;
        #[cfg(target_pointer_width = "64")]
        fn SetClassLongPtrW(hwnd: *mut c_void, n_index: i32, dw_new_long: isize) -> isize;
        #[cfg(target_pointer_width = "32")]
        #[link_name = "SetClassLongW"]
        fn SetClassLongPtrW(hwnd: *mut c_void, n_index: i32, dw_new_long: isize) -> isize;
    }

    const IMAGE_ICON: u32 = 1;
    const LR_DEFAULTSIZE: u32 = 0x0040;
    const LR_SHARED: u32 = 0x8000;
    const GCLP_HICON: i32 = -14;
    const GCLP_HICONSM: i32 = -34;
    const SM_CXSMICON: i32 = 49;
    const SM_CYSMICON: i32 = 50;

    let hwnd = window.hwnd() as *mut c_void;
    let res_id = 1usize as *const u16;
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let h_big = LoadImageW(hinst, res_id, IMAGE_ICON, 0, 0, LR_DEFAULTSIZE | LR_SHARED);
        if !h_big.is_null() {
            SetClassLongPtrW(hwnd, GCLP_HICON, h_big as isize);
        }
        let sm_w = GetSystemMetrics(SM_CXSMICON).max(16);
        let sm_h = GetSystemMetrics(SM_CYSMICON).max(16);
        let h_small = LoadImageW(hinst, res_id, IMAGE_ICON, sm_w, sm_h, LR_SHARED);
        if !h_small.is_null() {
            SetClassLongPtrW(hwnd, GCLP_HICONSM, h_small as isize);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_windows_class_icons(_window: &tao::window::Window) {}

fn sync_window_title(window: &tao::window::Window, state: &mut IdeApplicationState) {
    if state.take_workspace_title_dirty() {
        window.set_title(&state.taskbar_title());
    }
}

fn find_workspace_window(
    windows: &HashMap<WindowId, IdeWindowHost>,
    path: &Path,
    origin_id: WindowId,
) -> Option<WindowId> {
    let matches: Vec<WindowId> = windows
        .iter()
        .filter(|(_, host)| workspace_paths_equal(&host.app_state.workspace_root, path))
        .map(|(id, _)| *id)
        .collect();
    if matches.is_empty() {
        return None;
    }
    if matches.contains(&origin_id) {
        return Some(origin_id);
    }
    if let Some(id) = matches.iter().copied().find(|id| {
        windows
            .get(id)
            .is_some_and(|host| host.is_focused && !host.is_minimized)
    }) {
        return Some(id);
    }
    matches.into_iter().find(|id| *id != origin_id)
}

#[cfg(target_os = "windows")]
fn activate_os_window(window: &tao::window::Window) {
    use tao::platform::windows::WindowExtWindows;
    #[link(name = "user32")]
    extern "system" {
        fn IsIconic(hwnd: *mut core::ffi::c_void) -> i32;
        fn ShowWindow(hwnd: *mut core::ffi::c_void, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: *mut core::ffi::c_void) -> i32;
        fn BringWindowToTop(hwnd: *mut core::ffi::c_void) -> i32;
    }
    const SW_RESTORE: i32 = 9;
    let hwnd = window.hwnd() as *mut core::ffi::c_void;
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd) == 0 {
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
        } else {
            BringWindowToTop(hwnd);
        }
    }
    window.set_focus();
}

#[cfg(not(target_os = "windows"))]
fn activate_os_window(window: &tao::window::Window) {
    window.set_minimized(false);
    window.set_focus();
}

#[cfg(target_os = "windows")]
fn flash_os_window(window: &tao::window::Window) {
    use tao::platform::windows::WindowExtWindows;
    #[repr(C)]
    struct FlashWInfo {
        cb_size: u32,
        hwnd: *mut core::ffi::c_void,
        flags: u32,
        count: u32,
        timeout: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn FlashWindowEx(info: *const FlashWInfo) -> i32;
    }
    const FLASHW_ALL: u32 = 0x0000_0003;
    let info = FlashWInfo {
        cb_size: std::mem::size_of::<FlashWInfo>() as u32,
        hwnd: window.hwnd() as *mut core::ffi::c_void,
        flags: FLASHW_ALL,
        count: 6,
        timeout: 70,
    };
    unsafe {
        FlashWindowEx(&info);
    }
}

#[cfg(not(target_os = "windows"))]
fn flash_os_window(window: &tao::window::Window) {
    use tao::window::UserAttentionType;
    window.request_user_attention(Some(UserAttentionType::Critical));
}

/// Focus the window that already has `path`. Repeated attempts flash that window.
fn focus_already_open_project(
    windows: &mut HashMap<WindowId, IdeWindowHost>,
    recent: &mut Option<(WindowId, u32, Instant)>,
    created_this_batch: &HashSet<WindowId>,
    origin_id: WindowId,
    path: &Path,
) -> bool {
    let Some(existing_id) = find_workspace_window(windows, path, origin_id) else {
        return false;
    };
    let fresh = created_this_batch.contains(&existing_id);
    let flashed = if fresh {
        false
    } else {
        let now = Instant::now();
        let count = duplicate_open_count(recent, &existing_id, now);
        *recent = Some((existing_id, count, now));
        count >= 2
    };

    if let Some(host) = windows.get_mut(&existing_id) {
        host.is_minimized = false;
        host.is_focused = true;
        activate_os_window(&host.window);
        if flashed {
            flash_os_window(&host.window);
            host.app_state.start_window_attention_flash();
        }
        host.pending_redraw = true;
        host.window.request_redraw();
    }
    if fresh {
        return true;
    }

    let same = existing_id == origin_id;
    if let Some(origin) = windows.get_mut(&origin_id) {
        origin.app_state.status_notification = if flashed {
            origin
                .app_state
                .tr(
                    "该项目已打开，窗口已闪烁提示",
                    "Project is already open — flashed that window",
                )
                .to_string()
        } else if same {
            origin
                .app_state
                .tr(
                    "该项目已在当前窗口打开",
                    "This project is already open in this window",
                )
                .to_string()
        } else {
            origin
                .app_state
                .tr(
                    "该项目已打开，已切换到对应窗口",
                    "Project is already open — switched to that window",
                )
                .to_string()
        };
        origin.pending_redraw = true;
    }
    if flashed && !same {
        if let Some(target) = windows.get_mut(&existing_id) {
            target.app_state.status_notification = target
                .app_state
                .tr(
                    "该项目已在此窗口打开",
                    "This project is already open in this window",
                )
                .to_string();
            target.pending_redraw = true;
        }
    }
    true
}

fn schedule_attention_frames(
    windows: &HashMap<WindowId, IdeWindowHost>,
    control_flow: &mut ControlFlow,
    ui_test_mode: bool,
) {
    if ui_test_mode {
        return;
    }
    if windows.values().any(|host| {
        host.app_state.window_attention_until.is_some()
            || host.app_state.external_drag_hover.is_some()
            || host.app_state.active_ide_drag.is_some()
    }) {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16));
    }
}

#[cfg(target_os = "windows")]
fn apply_windows11_window_style(window: &tao::window::Window, dark_mode: bool) {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
        DWMWCP_ROUND,
    };
    let hwnd = HWND(window.hwnd() as *mut core::ffi::c_void);
    let pref = DWMWCP_ROUND.0 as u32;
    let dark: i32 = if dark_mode { 1 } else { 0 };
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_windows11_window_style(_window: &tao::window::Window, _dark_mode: bool) {}

fn detect_resize_direction(x: i32, y: i32, w: i32, h: i32) -> Option<ResizeDirection> {
    const EDGE: i32 = 6;
    if w <= EDGE * 2 || h <= EDGE * 2 {
        return None;
    }
    let left = x < EDGE;
    let right = x >= w - EDGE;
    let top = y < EDGE;
    let bottom = y >= h - EDGE;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, true, _) => Some(ResizeDirection::North),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

fn resize_cursor_icon(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::NorthWest => CursorIcon::NwResize,
        ResizeDirection::NorthEast => CursorIcon::NeResize,
        ResizeDirection::SouthWest => CursorIcon::SwResize,
        ResizeDirection::SouthEast => CursorIcon::SeResize,
        ResizeDirection::West => CursorIcon::WResize,
        ResizeDirection::East => CursorIcon::EResize,
        ResizeDirection::North => CursorIcon::NResize,
        ResizeDirection::South => CursorIcon::SResize,
    }
}

fn to_tao_cursor(kind: IdeCursorKind) -> CursorIcon {
    match kind {
        IdeCursorKind::Default => CursorIcon::Default,
        IdeCursorKind::Text => CursorIcon::Text,
        IdeCursorKind::Pointer => CursorIcon::Hand,
        IdeCursorKind::EwResize => CursorIcon::EwResize,
        IdeCursorKind::NsResize => CursorIcon::NsResize,
        IdeCursorKind::Copy => CursorIcon::Copy,
        IdeCursorKind::Grabbing => CursorIcon::Grabbing,
        IdeCursorKind::NoDrop => CursorIcon::NoDrop,
    }
}

#[derive(Debug, Clone)]
struct CrossWindowDragState {
    source_window_id: WindowId,
    path: PathBuf,
}

#[cfg(target_os = "windows")]
fn set_mouse_capture(window: &tao::window::Window) {
    use tao::platform::windows::WindowExtWindows;
    #[link(name = "user32")]
    extern "system" {
        fn SetCapture(hwnd: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }
    unsafe {
        let _ = SetCapture(window.hwnd() as *mut core::ffi::c_void);
    }
}

#[cfg(not(target_os = "windows"))]
fn set_mouse_capture(_window: &tao::window::Window) {}

#[cfg(target_os = "windows")]
fn release_mouse_capture() {
    #[link(name = "user32")]
    extern "system" {
        fn ReleaseCapture() -> i32;
    }
    unsafe {
        let _ = ReleaseCapture();
    }
}

#[cfg(not(target_os = "windows"))]
fn release_mouse_capture() {}

#[cfg(target_os = "windows")]
fn is_left_mouse_button_down() -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(v_key: i32) -> i16;
    }
    const VK_LBUTTON: i32 = 0x01;
    unsafe { (GetAsyncKeyState(VK_LBUTTON) as u16 & 0x8000) != 0 }
}

#[cfg(not(target_os = "windows"))]
fn is_left_mouse_button_down() -> bool {
    true
}

#[cfg(target_os = "windows")]
fn global_cursor_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    #[link(name = "user32")]
    extern "system" {
        fn GetCursorPos(lp_point: *mut POINT) -> i32;
    }
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        if GetCursorPos(&mut pt) != 0 {
            Some((pt.x, pt.y))
        } else {
            None
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn global_cursor_pos() -> Option<(i32, i32)> {
    None
}

#[cfg(target_os = "windows")]
fn find_ide_window_at_screen_pos(
    windows: &HashMap<WindowId, IdeWindowHost>,
    screen_pt: (i32, i32),
) -> Option<(WindowId, (i32, i32))> {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::Graphics::Gdi::ScreenToClient;

    #[link(name = "user32")]
    extern "system" {
        fn WindowFromPoint(point: POINT) -> *mut core::ffi::c_void;
        fn GetAncestor(hwnd: *mut core::ffi::c_void, ga_flags: u32) -> *mut core::ffi::c_void;
    }
    const GA_ROOT: u32 = 2;

    let pt = POINT {
        x: screen_pt.0,
        y: screen_pt.1,
    };
    let hit_hwnd = unsafe { WindowFromPoint(pt) };
    if hit_hwnd.is_null() {
        return None;
    }
    let root_hwnd = unsafe {
        let anc = GetAncestor(hit_hwnd, GA_ROOT);
        if anc.is_null() {
            hit_hwnd
        } else {
            anc
        }
    };
    for (&wid, host) in windows {
        if host.is_minimized || !host.window.is_visible() {
            continue;
        }
        if host.window.hwnd() as isize == root_hwnd as isize {
            let mut client_pt = pt;
            let ok = unsafe {
                ScreenToClient(
                    HWND(host.window.hwnd() as *mut core::ffi::c_void),
                    &mut client_pt,
                )
                .as_bool()
            };
            if ok {
                let logical = host
                    .surface_canvas
                    .to_logical_pos(client_pt.x as f64, client_pt.y as f64);
                return Some((wid, logical));
            }
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn find_ide_window_at_screen_pos(
    windows: &HashMap<WindowId, IdeWindowHost>,
    screen_pt: (i32, i32),
) -> Option<(WindowId, (i32, i32))> {
    for (&wid, host) in windows {
        if host.is_minimized || !host.window.is_visible() {
            continue;
        }
        let Ok(pos) = host.window.inner_position() else {
            continue;
        };
        let size = host.window.inner_size();
        let rx = screen_pt.0 - pos.x;
        let ry = screen_pt.1 - pos.y;
        if rx >= 0 && ry >= 0 && rx < size.width as i32 && ry < size.height as i32 {
            let logical = host.surface_canvas.to_logical_pos(rx as f64, ry as f64);
            return Some((wid, logical));
        }
    }
    None
}

fn sync_cross_window_drag_hover(
    windows: &mut HashMap<WindowId, IdeWindowHost>,
    drag: &CrossWindowDragState,
) {
    let target = global_cursor_pos()
        .and_then(|pt| find_ide_window_at_screen_pos(windows, pt))
        .or_else(|| {
            windows.get(&drag.source_window_id).and_then(|src| {
                let (cx, cy) = src.cursor_pos;
                let w = src.surface_canvas.width as i32;
                let h = src.surface_canvas.height as i32;
                if cx >= 0 && cy >= 0 && cx < w && cy < h {
                    Some((drag.source_window_id, (cx, cy)))
                } else {
                    None
                }
            })
        });

    for (&wid, host) in windows.iter_mut() {
        if target.as_ref().is_some_and(|(tid, _)| *tid == wid) {
            let (_, local_pos) = target.unwrap();
            if wid == drag.source_window_id {
                host.app_state.external_drag_hover = None;
                let next = Some((drag.path.clone(), local_pos));
                if host.app_state.active_ide_drag != next {
                    host.app_state.active_ide_drag = next;
                    host.pending_redraw = true;
                }
            } else {
                host.app_state.active_ide_drag = None;
                let next = Some((drag.path.clone(), local_pos));
                if host.app_state.external_drag_hover != next {
                    host.app_state.external_drag_hover = next;
                    host.pending_redraw = true;
                }
            }
            host.cursor_pos = local_pos;
        } else {
            if host.app_state.active_ide_drag.take().is_some()
                || host.app_state.external_drag_hover.take().is_some()
            {
                host.pending_redraw = true;
            }
        }
    }

    let desired_icon = match target {
        Some((tid, (lx, ly))) => {
            if let Some(thost) = windows.get(&tid) {
                let w = thost.surface_canvas.width as i32;
                let h = thost.surface_canvas.height as i32;
                to_tao_cursor(thost.app_state.cursor_kind_at(lx, ly, w, h))
            } else {
                CursorIcon::NoDrop
            }
        }
        None => CursorIcon::NoDrop,
    };
    if let Some(src_host) = windows.get_mut(&drag.source_window_id) {
        if src_host.last_cursor_icon != desired_icon {
            src_host.window.set_cursor_icon(desired_icon);
            src_host.last_cursor_icon = desired_icon;
        }
    }
}

fn clear_all_window_drags(windows: &mut HashMap<WindowId, IdeWindowHost>) {
    for host in windows.values_mut() {
        host.left_mouse_down = false;
        host.app_state.pending_ide_drag = None;
        if host.app_state.active_ide_drag.take().is_some()
            || host.app_state.external_drag_hover.take().is_some()
        {
            host.pending_redraw = true;
        }
    }
}

fn finish_cross_window_drag(
    windows: &mut HashMap<WindowId, IdeWindowHost>,
    drag: CrossWindowDragState,
) {
    release_mouse_capture();
    let target = global_cursor_pos().and_then(|pt| find_ide_window_at_screen_pos(windows, pt));
    clear_all_window_drags(windows);
    if let Some(src_host) = windows.get_mut(&drag.source_window_id) {
        src_host.app_state.end_dock_drag(
            src_host.cursor_pos.0,
            src_host.cursor_pos.1,
            src_host.surface_canvas.width as i32,
            src_host.surface_canvas.height as i32,
        );
        src_host.app_state.end_splitter_drag();
        src_host.app_state.end_scrollbar_drag();
        src_host.app_state.end_mouse_text_drag();
        sync_window_cursor(
            &src_host.window,
            &src_host.app_state,
            &src_host.surface_canvas,
            src_host.cursor_pos,
            &mut src_host.last_cursor_icon,
        );
        src_host.pending_redraw = true;
    }

    if let Some((target_wid, (lx, ly))) = target {
        let is_cross = target_wid != drag.source_window_id;
        if let Some(target_host) = windows.get_mut(&target_wid) {
            let _ = target_host
                .app_state
                .handle_ide_drag_drop_at(drag.path, lx, ly, is_cross);
            target_host.cursor_pos = (lx, ly);
            sync_window_cursor(
                &target_host.window,
                &target_host.app_state,
                &target_host.surface_canvas,
                (lx, ly),
                &mut target_host.last_cursor_icon,
            );
            target_host.pending_redraw = true;
            target_host.window.request_redraw();
            if is_cross {
                target_host.is_minimized = false;
                target_host.is_focused = true;
                activate_os_window(&target_host.window);
            }
        }
    }
}

fn cancel_cross_window_drag(windows: &mut HashMap<WindowId, IdeWindowHost>) {
    release_mouse_capture();
    clear_all_window_drags(windows);
    for host in windows.values_mut() {
        sync_window_cursor(
            &host.window,
            &host.app_state,
            &host.surface_canvas,
            host.cursor_pos,
            &mut host.last_cursor_icon,
        );
        host.window.request_redraw();
    }
}

fn sync_window_cursor(
    window: &tao::window::Window,
    state: &IdeApplicationState,
    canvas: &SurfaceCanvas,
    cursor_pos: (i32, i32),
    last_icon: &mut CursorIcon,
) {
    let win_w = canvas.width as i32;
    let win_h = canvas.height as i32;

    let next_icon = if !window.is_maximized()
        && state.active_splitter_drag.is_none()
        && state.active_scrollbar_drag.is_none()
        && state.mouse_drag_target.is_none()
    {
        if let Some(dir) = detect_resize_direction(cursor_pos.0, cursor_pos.1, win_w, win_h) {
            resize_cursor_icon(dir)
        } else {
            to_tao_cursor(state.cursor_kind_at(cursor_pos.0, cursor_pos.1, win_w, win_h))
        }
    } else {
        to_tao_cursor(state.cursor_kind_at(cursor_pos.0, cursor_pos.1, win_w, win_h))
    };

    if next_icon != *last_icon {
        window.set_cursor_icon(next_icon);
        *last_icon = next_icon;
    }
}

fn collect_open_paths(
    windows: &HashMap<WindowId, IdeWindowHost>,
    order: &[PathBuf],
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in order {
        if windows
            .values()
            .any(|host| workspace_paths_equal(&host.app_state.workspace_root, path))
            && !out.iter().any(|existing| workspace_paths_equal(existing, path))
        {
            out.push(path.clone());
        }
    }
    for host in windows.values() {
        let path = &host.app_state.workspace_root;
        if !out.iter().any(|existing| workspace_paths_equal(existing, path)) {
            out.push(path.clone());
        }
    }
    out
}

fn same_session(left: &[PathBuf], right: &[PathBuf]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| workspace_paths_equal(a, b))
}

fn remember_open_windows(storage_dir: Option<&Path>, paths: &[PathBuf]) {
    let Some(dir) = storage_dir else {
        return;
    };
    if paths.is_empty() {
        return;
    }
    persistence::store_open_windows(dir, paths);
}

fn existing_session_dirs(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_dir() && !out.iter().any(|existing| workspace_paths_equal(existing, path)) {
            out.push(path.clone());
        }
    }
    out
}

fn resolve_external_file_path(
    windows: &HashMap<WindowId, IdeWindowHost>,
    path: &Path,
    cwd: Option<&Path>,
) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if let Some(base) = cwd {
        let candidate = base.join(path);
        if candidate.is_file() {
            return candidate;
        }
    }
    for host in windows.values() {
        let candidate = host.app_state.workspace_root.join(path);
        if candidate.is_file() {
            return candidate;
        }
    }
    path.to_path_buf()
}

fn preferred_window_for_file(
    windows: &HashMap<WindowId, IdeWindowHost>,
    path: &Path,
) -> Option<WindowId> {
    let mut best_match: Option<(WindowId, usize, bool)> = None;
    for (&id, host) in windows {
        let ws = &host.app_state.workspace_root;
        if path_is_inside_workspace(path, ws) {
            let depth = ws.as_os_str().len();
            let replace = match best_match {
                None => true,
                Some((_, prev_depth, prev_focused)) => {
                    depth > prev_depth || (depth == prev_depth && host.is_focused && !prev_focused)
                }
            };
            if replace {
                best_match = Some((id, depth, host.is_focused));
            }
        }
    }
    if let Some((id, _, _)) = best_match {
        return Some(id);
    }
    windows
        .iter()
        .find(|(_, host)| host.is_focused && !host.is_minimized)
        .map(|(id, _)| *id)
        .or_else(|| {
            windows
                .iter()
                .find(|(_, host)| host.is_focused)
                .map(|(id, _)| *id)
        })
        .or_else(|| windows.keys().next().copied())
}

fn deliver_external_targets(
    windows: &mut HashMap<WindowId, IdeWindowHost>,
    targets: &[ExternalOpenTarget],
) {
    for target in targets {
        match target {
            ExternalOpenTarget::OpenWorkspace { path, .. } => {
                if let Some((&existing_id, _)) = windows
                    .iter()
                    .find(|(_, host)| workspace_paths_equal(&host.app_state.workspace_root, path))
                {
                    if let Some(host) = windows.get_mut(&existing_id) {
                        host.is_minimized = false;
                        host.is_focused = true;
                        activate_os_window(&host.window);
                        host.pending_redraw = true;
                        host.window.request_redraw();
                    }
                } else if path.is_dir() {
                    let target_id = windows
                        .iter()
                        .find(|(_, h)| h.is_focused)
                        .map(|(id, _)| *id)
                        .or_else(|| windows.keys().next().copied());
                    if let Some(host) = target_id.and_then(|id| windows.get_mut(&id)) {
                        host.app_state.pending_new_windows.push(path.clone());
                    }
                }
            }
            ExternalOpenTarget::OpenFile {
                path,
                line,
                column,
                cwd,
                ..
            } => {
                let resolved = resolve_external_file_path(windows, path, cwd.as_deref());
                let Some(id) = preferred_window_for_file(windows, &resolved) else {
                    continue;
                };
                for (&wid, host) in windows.iter_mut() {
                    host.is_focused = wid == id;
                }
                let Some(host) = windows.get_mut(&id) else {
                    continue;
                };
                host.app_state.close_top_menu();
                host.app_state.context_menu = None;
                host.app_state
                    .open_file_at_line_col(resolved, *line, *column);
                host.is_minimized = false;
                host.is_focused = true;
                host.window.set_visible(true);
                activate_os_window(&host.window);
                host.pending_redraw = true;
                host.window.request_redraw();
            }
        }
    }
}

fn parse_explicit_workspace_arg(args: &[String]) -> Option<PathBuf> {
    parse_cli_args(args, env::current_dir().ok().as_deref()).explicit_workspace
}

fn parse_workspace_arg(args: &[String]) -> PathBuf {
    parse_explicit_workspace_arg(args)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--export-icon") {
        let out_path = args
            .get(pos + 1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("assets/ri_icon.png"));
        let is_ico = out_path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ico"));
        if is_ico {
            save_ri_icon_ico(&out_path)?;
            println!("Exported RI icon ICO: {}", out_path.display());
        } else {
            save_ri_icon_png(&out_path, 256)?;
            println!("Exported RI icon PNG: {}", out_path.display());
        }
        return Ok(());
    }
    let self_check_mode = args.iter().any(|a| a == "--self-check");
    let ui_test_mode = args.iter().any(|a| a == "--ui-test");
    let storage_dir = persistence::default_storage_dir();
    let cwd = env::current_dir().ok();
    let parsed_cli = parse_cli_args(&args, cwd.as_deref());
    if !self_check_mode && !ui_test_mode && !parsed_cli.targets.is_empty() {
        if try_forward_via_ipc(storage_dir.as_deref(), &parsed_cli.targets) {
            return Ok(());
        }
    }

    let saved_global: persistence::PersistedGlobalSettings = storage_dir
        .as_deref()
        .and_then(|d| persistence::load_json(&persistence::global_settings_path(d)))
        .unwrap_or_default();
    let explicit_workspace = parsed_cli.explicit_workspace.clone().or_else(|| {
        parsed_cli.targets.iter().find_map(|t| match t {
            ExternalOpenTarget::OpenFile { path, .. } => {
                infer_workspace_for_file(path, &saved_global.open_windows)
            }
            ExternalOpenTarget::OpenWorkspace { path, .. } => Some(path.clone()),
        })
    });
    let saved_session = if explicit_workspace.is_none() && !ui_test_mode {
        existing_session_dirs(&saved_global.open_windows)
    } else {
        Vec::new()
    };
    let workspace_root = explicit_workspace
        .or_else(|| saved_session.first().cloned())
        .or_else(|| saved_global.last_workspace.clone().filter(|p| p.is_dir()))
        .unwrap_or_else(|| parse_workspace_arg(&args));

    if self_check_mode {
        let sys_scale = detect_system_dpi_scale();
        let mut state = IdeApplicationState::new(workspace_root);
        state.system_dpi_scale = sys_scale;
        let phys_w = (1560.0 * sys_scale).round() as usize;
        let phys_h = (940.0 * sys_scale).round() as usize;
        let mut canvas = SurfaceCanvas::new_with_scale(phys_w, phys_h, state.effective_scale());
        state.render(&mut canvas);

        let screenshot_dir = Path::new(
            r"C:\Users\renyu\.gemini\antigravity-acp\brain\bdfa34ef-e982-4639-9e67-ad62c315c891",
        );
        if screenshot_dir.exists() {
            let _ = canvas.save_png(&screenshot_dir.join("idea_ui_default.png"));
        }

        // 2. 导出 Git Commit 面板 + 菜单展开 + Git Branches 弹窗状态图
        state.left_tool = Some(ToolWindowId::Commit);
        state.hamburger_menu_expanded = true;
        state.git_branch_popup_open = true;
        state.render(&mut canvas);
        if screenshot_dir.exists() {
            let _ = canvas.save_png(&screenshot_dir.join("idea_ui_git_and_menus.png"));
        }
        state.git_branch_popup_open = false;
        state.hamburger_menu_expanded = false;
        state.left_tool = Some(ToolWindowId::Project);

        // 3. 导出 Settings & IDEA Keymap 弹窗状态图
        state.focus = FocusTarget::SettingsModal;
        state.render(&mut canvas);
        if screenshot_dir.exists() {
            let _ = canvas.save_png(&screenshot_dir.join("idea_ui_settings_keymap.png"));
        }

        // 4. 导出 AI 模型配置弹窗状态图
        state.open_ai_config_modal();
        state.render(&mut canvas);
        if screenshot_dir.exists() {
            let _ = canvas.save_png(&screenshot_dir.join("idea_ui_ai_config.png"));
        }
        state.focus = FocusTarget::Editor;

        // 5. 导出 Cursor Ctrl+K 行内修改 Diff 状态图
        state.trigger_ctrl_k_inline_edit();
        state.render(&mut canvas);
        if screenshot_dir.exists() {
            let _ = canvas.save_png(&screenshot_dir.join("idea_ui_ctrl_k_diff.png"));
        }

        println!(
            "=== Rust AI IDE (Latest IDEA Islands UI + Full Interactive Engine) Self-Check ==="
        );
        println!("Workspace Root : {}", state.workspace_root.display());
        println!(
            "System DPI     : {:.0}% (scale {:.2}x)",
            sys_scale * 100.0,
            sys_scale
        );
        println!("Open Buffers   : {}", state.open_buffers.len());
        println!(
            "Visible Tree   : {}",
            state.project_tree.visible_entries.len()
        );
        println!(
            "Git Branch     : {} ({} changed files)",
            state.git_engine.current_branch,
            state.git_engine.changed_files.len()
        );
        println!(
            "Rendered Frame : {}x{} logical / {}x{} physical ({} interactive hit zones)",
            canvas.width,
            canvas.height,
            canvas.phys_width,
            canvas.phys_height,
            state.hit_regions.len()
        );
        return Ok(());
    }

    let event_loop = EventLoopBuilder::<IdeWakeupEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let external_integration = if !ui_test_mode {
        let paths = storage_dir.as_deref().and_then(ensure_editor_shims);
        if let Some(ref p) = paths {
            let _ = ensure_windows_url_protocol(&p.exe_path);
        }
        paths
    } else {
        None
    };
    let mut ipc_guard = if !ui_test_mode {
        let ipc_proxy = proxy.clone();
        start_ipc_server(storage_dir.as_deref(), move |targets| {
            let _ = ipc_proxy.send_event(IdeWakeupEvent::ExternalOpen(targets));
        })
    } else {
        None
    };

    let (init_log_w, init_log_h) =
        if saved_global.window_width >= 800 && saved_global.window_height >= 540 {
            (
                saved_global.window_width as f64,
                saved_global.window_height as f64,
            )
        } else if let Some(mon) = event_loop.primary_monitor() {
            let mscale = mon.scale_factor().max(0.5);
            let msize = mon.size();
            let mon_log_w = msize.width as f64 / mscale;
            let mon_log_h = msize.height as f64 / mscale;
            (
                1560.0_f64.min((mon_log_w * 0.92).max(1120.0)),
                940.0_f64.min((mon_log_h * 0.90).max(700.0)),
            )
        } else {
            (1560.0, 940.0)
        };

    let initial_theme = IdeaTheme::from_kind(saved_global.theme_kind);
    let shared_window_icon = load_ri_icon_sized(48);
    let shared_taskbar_icon = load_ri_icon_sized(128);
    let initial_host = spawn_ide_window(
        &event_loop,
        &proxy,
        workspace_root,
        storage_dir.as_deref(),
        external_integration.as_ref(),
        shared_window_icon.clone(),
        shared_taskbar_icon.clone(),
        initial_theme,
        init_log_w,
        init_log_h,
        saved_global.window_maximized,
    )?;
    let primary_window_id = initial_host.window.id();
    let mut windows: HashMap<WindowId, IdeWindowHost> = HashMap::new();
    windows.insert(primary_window_id, initial_host);
    let mut session_order = vec![windows
        .get(&primary_window_id)
        .map(|host| host.app_state.workspace_root.clone())
        .unwrap_or_default()];
    if !ui_test_mode {
        let primary_root = session_order[0].clone();
        for root in saved_session {
            if workspace_paths_equal(&root, &primary_root) {
                continue;
            }
            match spawn_ide_window(
                &event_loop,
                &proxy,
                root,
                storage_dir.as_deref(),
                external_integration.as_ref(),
                shared_window_icon.clone(),
                shared_taskbar_icon.clone(),
                initial_theme,
                init_log_w,
                init_log_h,
                false,
            ) {
                Ok(mut host) => {
                    host.is_focused = false;
                    host.unfocused_since = Some(Instant::now());
                    session_order.push(host.app_state.workspace_root.clone());
                    let id = host.window.id();
                    windows.insert(id, host);
                }
                Err(err) => {
                    if let Some(origin) = windows.get_mut(&primary_window_id) {
                        origin.app_state.status_notification = format!(
                            "{}: {err}",
                            origin
                                .app_state
                                .tr("无法恢复项目窗口", "Failed to restore project window")
                        );
                    }
                }
            }
        }
        trim_process_working_set();
    }
    let secondary_ids: Vec<WindowId> = windows
        .keys()
        .copied()
        .filter(|&id| id != primary_window_id)
        .collect();
    for id in secondary_ids {
        if let Some(host) = windows.get_mut(&id) {
            reveal_ide_window(host, !ui_test_mode);
        }
    }
    if let Some(host) = windows.get_mut(&primary_window_id) {
        reveal_ide_window(host, !ui_test_mode);
        if !ui_test_mode {
            host.window.set_focus();
        }
    }
    if !parsed_cli.targets.is_empty() {
        deliver_external_targets(&mut windows, &parsed_cli.targets);
    }
    let mut ui_test_frames: u32 = 0;
    let mut recent_reopen: Option<(WindowId, u32, Instant)> = None;
    let mut close_burst: Option<(Instant, Vec<PathBuf>)> = None;
    let mut active_cross_drag: Option<CrossWindowDragState> = None;

    event_loop.run(move |event, elwt, control_flow| {
        let mut any_anim = false;
        let mut any_active_terminal = false;
        for host in windows.values() {
            if !host.is_minimized && host.app_state.has_active_animations() {
                any_anim = true;
            }
            if !host.is_minimized
                && ((host.app_state.bottom_tool == Some(ToolWindowId::Terminal)
                    && (host.is_focused
                        || host
                            .app_state
                            .terminal
                            .active_pty()
                            .alive
                            .load(std::sync::atomic::Ordering::Relaxed)))
                    || host.app_state.terminal.any_run_active())
            {
                any_active_terminal = true;
            }
        }
        if ui_test_mode {
            *control_flow = ControlFlow::Poll;
        } else if any_anim {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16));
        } else if any_active_terminal {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(80));
        } else {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(260));
        }

        let mut spawn_pending = |windows: &mut HashMap<WindowId, IdeWindowHost>| {
            let mut new_window_requests: Vec<(WindowId, PathBuf)> = Vec::new();
            let mut switch_requests: Vec<(WindowId, PathBuf)> = Vec::new();
            for (&origin_id, host) in windows.iter_mut() {
                for p in host.app_state.pending_new_windows.drain(..) {
                    new_window_requests.push((origin_id, p));
                }
                for p in host.app_state.pending_switch_workspaces.drain(..) {
                    switch_requests.push((origin_id, p));
                }
            }
            let mut created_this_batch: HashSet<WindowId> = HashSet::new();
            for (origin_id, target_path) in new_window_requests {
                if focus_already_open_project(
                    windows,
                    &mut recent_reopen,
                    &created_this_batch,
                    origin_id,
                    &target_path,
                ) {
                    continue;
                }
                match spawn_ide_window(
                    elwt,
                    &proxy,
                    target_path.clone(),
                    storage_dir.as_deref(),
                    external_integration.as_ref(),
                    shared_window_icon.clone(),
                    shared_taskbar_icon.clone(),
                    initial_theme,
                    init_log_w,
                    init_log_h,
                    false,
                ) {
                    Ok(mut new_host) => {
                        let new_id = new_host.window.id();
                        reveal_ide_window(&mut new_host, !ui_test_mode);
                        windows.insert(new_id, new_host);
                        created_this_batch.insert(new_id);
                        if let Some(origin) = windows.get_mut(&origin_id) {
                            origin.app_state.status_notification = format!(
                                "{}: {}",
                                origin
                                    .app_state
                                    .tr("已在新窗口开启项目", "Opened project in new window"),
                                target_path.display()
                            );
                            origin.pending_redraw = true;
                        }
                    }
                    Err(e) => {
                        if let Some(origin) = windows.get_mut(&origin_id) {
                            origin.app_state.status_notification = format!(
                                "{}: {e}",
                                origin.app_state.tr("无法创建新窗口", "Failed to create new window")
                            );
                            origin.pending_redraw = true;
                        }
                    }
                }
            }
            for (origin_id, target_path) in switch_requests {
                if focus_already_open_project(
                    windows,
                    &mut recent_reopen,
                    &created_this_batch,
                    origin_id,
                    &target_path,
                ) {
                    continue;
                }
                if let Some(origin) = windows.get_mut(&origin_id) {
                    origin.app_state.switch_workspace(target_path);
                    sync_window_title(&origin.window, &mut origin.app_state);
                    origin.pending_redraw = true;
                    origin.window.request_redraw();
                }
            }
        };

        match event {
            Event::LoopDestroyed => {
                let _ = ipc_guard.take();
            }
            Event::UserEvent(IdeWakeupEvent::ExternalOpen(targets)) => {
                deliver_external_targets(&mut windows, &targets);
                spawn_pending(&mut windows);
                schedule_attention_frames(&windows, control_flow, ui_test_mode);
            }
            Event::UserEvent(IdeWakeupEvent::Wakeup(wid)) => {
                if let Some(host) = windows.get_mut(&wid) {
                    if host.drain_pending_win_text() {
                        host.pending_redraw = true;
                    }
                    let _ = host.app_state.poll_background_updates();
                    sync_window_title(&host.window, &mut host.app_state);
                    if !host.is_minimized {
                        host.pending_redraw = true;
                    }
                }
                spawn_pending(&mut windows);
                schedule_attention_frames(&windows, control_flow, ui_test_mode);
            }
            Event::MainEventsCleared => {
                if let Some(ref drag) = active_cross_drag {
                    if !is_left_mouse_button_down() {
                        if let Some(drag) = active_cross_drag.take() {
                            finish_cross_window_drag(&mut windows, drag);
                        }
                    } else {
                        sync_cross_window_drag_hover(&mut windows, drag);
                    }
                } else {
                    for host in windows.values_mut() {
                        if let Some((path, old_pos)) = host.app_state.external_drag_hover.clone() {
                            let cur = cursor_pos_in_window(
                                &host.window,
                                &host.surface_canvas,
                                host.cursor_pos,
                            );
                            if cur != old_pos {
                                host.cursor_pos = cur;
                                host.app_state.external_drag_hover = Some((path, cur));
                                host.pending_redraw = true;
                            }
                        }
                    }
                }
                spawn_pending(&mut windows);
                schedule_attention_frames(&windows, control_flow, ui_test_mode);
                if ui_test_mode {
                    if let Some(host) = windows.get_mut(&primary_window_id) {
                        ui_test_frames += 1;
                        if ui_test_frames == 2 {
                            host.window.request_redraw();
                        } else if ui_test_frames == 5 {
                            host.app_state.hamburger_menu_expanded = true;
                            host.app_state.active_menu_category = Some(MainMenuCategory::View);
                            host.app_state.trigger_ctrl_k_inline_edit();
                            host.window.request_redraw();
                        } else if ui_test_frames >= 10 {
                            let screenshot_dir = Path::new(
                                r"C:\Users\renyu\.gemini\antigravity-acp\brain\bdfa34ef-e982-4639-9e67-ad62c315c891",
                            );
                            if screenshot_dir.exists() {
                                let _ = host
                                    .surface_canvas
                                    .save_png(&screenshot_dir.join("idea_window_live_test.png"));
                            }
                            println!(
                                "Automated Live Window UI Test passed ({} frames rendered).",
                                ui_test_frames
                            );
                            *control_flow = ControlFlow::Exit;
                        }
                    }
                } else {
                    let burst = close_burst.as_ref().map(|(started, snapshot)| {
                        (
                            started.elapsed() >= Duration::from_millis(800),
                            snapshot.clone(),
                        )
                    });
                    if let Some((ready, snapshot)) = burst {
                        if ready {
                            let still_open = collect_open_paths(&windows, &session_order);
                            let keep = session_windows_after_close(&snapshot, &still_open);
                            session_order = keep.clone();
                            remember_open_windows(storage_dir.as_deref(), &keep);
                            close_burst = None;
                        } else {
                            *control_flow =
                                ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(40));
                        }
                    } else {
                        let latest = collect_open_paths(&windows, &session_order);
                        if !same_session(&latest, &session_order) {
                            session_order = latest.clone();
                            remember_open_windows(storage_dir.as_deref(), &latest);
                        }
                    }
                    let mut all_minimized = !windows.is_empty();
                    let mut newly_trimmed_any = false;
                    for host in windows.values_mut() {
                        if host.drain_pending_win_text() {
                            host.pending_redraw = true;
                        }
                        if host.is_minimized {
                            let _ = host.app_state.poll_background_updates();
                            if !host.minimized_trimmed {
                                host.shrink_for_minimized_all();
                                newly_trimmed_any = true;
                            }
                            continue;
                        }
                        all_minimized = false;
                        host.minimized_trimmed = false;
                        let mut need_redraw = host.pending_redraw;
                        if host.app_state.poll_background_updates() {
                            sync_window_title(&host.window, &mut host.app_state);
                            need_redraw = true;
                        }
                        if host.app_state.step_animations() {
                            need_redraw = true;
                        }
                        let current_rev = host.app_state.active_terminal_revision();
                        if current_rev != host.last_pty_rev {
                            host.last_pty_rev = current_rev;
                            need_redraw = true;
                        }
                        if host.is_focused
                            && host.app_state.last_input_activity.elapsed()
                                >= Duration::from_millis(530)
                        {
                            host.app_state.caret_visible = !host.app_state.caret_visible;
                            host.app_state.last_input_activity = Instant::now();
                            need_redraw = true;
                        }
                        if need_redraw {
                            host.pending_redraw = false;
                            host.idle_trimmed = false;
                            host.window.request_redraw();
                        } else if !host.is_focused && !host.idle_trimmed {
                            if host
                                .unfocused_since
                                .is_some_and(|t| t.elapsed() >= Duration::from_secs(8))
                            {
                                host.surface_canvas.release_idle_framebuffer();
                                host.app_state.trim_caches_when_background_idle();
                                host.idle_trimmed = true;
                                newly_trimmed_any = true;
                            }
                        }
                    }
                    if newly_trimmed_any {
                        if all_minimized {
                            if let Some(first) = windows.values().next() {
                                first.surface_canvas.trim_text_caches();
                            }
                        }
                        trim_process_working_set();
                    }
                }
            }
            Event::RedrawRequested(wid) => {
                if let Some(host) = windows.get_mut(&wid) {
                    host.render_frame();
                }
            }
            Event::WindowEvent {
                window_id, event, ..
            } => {
                let mut handled_by_cross_drag = false;
                if let Some(ref drag) = active_cross_drag {
                    match &event {
                        WindowEvent::MouseInput {
                            state: ElementState::Released,
                            button: MouseButton::Left,
                            ..
                        } => {
                            if let Some(finished) = active_cross_drag.take() {
                                finish_cross_window_drag(&mut windows, finished);
                            }
                            handled_by_cross_drag = true;
                        }
                        WindowEvent::CursorMoved { position, .. }
                            if window_id == drag.source_window_id =>
                        {
                            if let Some(src_host) = windows.get_mut(&window_id) {
                                src_host.cursor_pos = src_host
                                    .surface_canvas
                                    .to_logical_pos(position.x, position.y);
                            }
                            if !is_left_mouse_button_down() {
                                if let Some(finished) = active_cross_drag.take() {
                                    finish_cross_window_drag(&mut windows, finished);
                                }
                            } else {
                                sync_cross_window_drag_hover(&mut windows, drag);
                            }
                            handled_by_cross_drag = true;
                        }
                        WindowEvent::KeyboardInput {
                            event:
                                KeyEvent {
                                    logical_key: Key::Escape,
                                    state: ElementState::Pressed,
                                    ..
                                },
                            ..
                        } => {
                            active_cross_drag = None;
                            cancel_cross_window_drag(&mut windows);
                            handled_by_cross_drag = true;
                        }
                        WindowEvent::Focused(false) if window_id == drag.source_window_id => {
                            if !is_left_mouse_button_down() {
                                active_cross_drag = None;
                                cancel_cross_window_drag(&mut windows);
                            }
                        }
                        _ => {}
                    }
                }

                let cmd = if handled_by_cross_drag {
                    WindowCommand::None
                } else if let Some(host) = windows.get_mut(&window_id) {
                    let cmd = host.handle_window_event(event);
                    if active_cross_drag.is_none() && host.left_mouse_down {
                        if let Some((ref drag_path, _)) = host.app_state.active_ide_drag {
                            let drag = CrossWindowDragState {
                                source_window_id: window_id,
                                path: drag_path.clone(),
                            };
                            set_mouse_capture(&host.window);
                            active_cross_drag = Some(drag);
                        }
                    }
                    cmd
                } else {
                    WindowCommand::None
                };
                if let Some(ref drag) = active_cross_drag {
                    if !handled_by_cross_drag {
                        sync_cross_window_drag_hover(&mut windows, drag);
                    }
                }
                if cmd == WindowCommand::CloseWindow {
                    if active_cross_drag
                        .as_ref()
                        .is_some_and(|d| d.source_window_id == window_id)
                    {
                        active_cross_drag = None;
                        cancel_cross_window_drag(&mut windows);
                    }
                    if close_burst.is_none() {
                        let snapshot = collect_open_paths(&windows, &session_order);
                        close_burst = Some((Instant::now(), snapshot));
                    }
                    let snapshot = close_burst
                        .as_ref()
                        .map(|(_, paths)| paths.clone())
                        .unwrap_or_default();
                    if let Some(mut host) = windows.remove(&window_id) {
                        if host.app_state.focus == FocusTarget::SettingsModal {
                            host.app_state.commit_mobile_sdk_locations_from_inputs();
                        }
                        host.app_state.save_all_persisted_state();
                        drop(host);
                        trim_process_working_set();
                    }
                    if windows.is_empty() {
                        let keep = session_windows_after_close(&snapshot, &[]);
                        if !ui_test_mode {
                            remember_open_windows(storage_dir.as_deref(), &keep);
                        }
                        *control_flow = ControlFlow::Exit;
                    }
                } else {
                    spawn_pending(&mut windows);
                    schedule_attention_frames(&windows, control_flow, ui_test_mode);
                }
            }
            _ => {}
        }
    });
}

impl IdeWindowHost {
    fn shrink_for_minimized_all(&mut self) {
        self.surface_canvas.shrink_for_minimized();
        let _ = self.surface.resize(NonZeroU32::MIN, NonZeroU32::MIN);
        self.app_state.trim_caches_when_minimized();
        self.minimized_trimmed = true;
        self.idle_trimmed = true;
    }

    fn render_frame(&mut self) {
        self.pending_redraw = false;
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 || self.window.is_minimized() {
            self.is_minimized = true;
            if !self.minimized_trimmed {
                self.shrink_for_minimized_all();
            }
            return;
        }
        self.is_minimized = false;
        self.minimized_trimmed = false;
        self.idle_trimmed = false;
        let width = size.width.max(1);
        let height = size.height.max(1);
        self.app_state.system_dpi_scale = (self.window.scale_factor() as f32).clamp(0.75, 4.0);
        self.app_state.window_maximized = self.window.is_maximized();
        if self.app_state.theme.window_bg != self.last_bg_rgb {
            self.last_bg_rgb = self.app_state.theme.window_bg;
            self.window
                .set_background_color(Some(rgb_to_tao_rgba(self.last_bg_rgb)));
            apply_windows11_window_style(
                &self.window,
                self.app_state.theme.kind != ThemeKind::IslandsLight,
            );
        }

        self.surface_canvas.resize_physical(
            width as usize,
            height as usize,
            self.app_state.effective_scale(),
        );
        self.app_state.render(&mut self.surface_canvas);
        sync_ime_position(
            &self.window,
            &self.app_state,
            &self.surface_canvas,
            &mut self.last_ime_physical,
        );
        sync_window_cursor(
            &self.window,
            &self.app_state,
            &self.surface_canvas,
            self.cursor_pos,
            &mut self.last_cursor_icon,
        );
        self.last_pty_rev = self.app_state.active_terminal_revision();

        if let (Some(nz_w), Some(nz_h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            if self.surface.resize(nz_w, nz_h).is_ok() {
                if let Ok(mut buffer) = self.surface.buffer_mut() {
                    self.surface_canvas.copy_to_softbuffer(&mut buffer);
                    let _ = buffer.present();
                }
            }
        }
    }

    fn drain_pending_win_text(&mut self) -> bool {
        if let Some(txt) = win_text_hook::drain_pending_text(&self.window) {
            if !txt.is_empty() {
                self.app_state.insert_text_at_focus(&txt);
                self.app_state.touch_caret_activity();
                self.surface_canvas
                    .set_scale_factor(self.app_state.effective_scale());
                self.pending_redraw = true;
                return true;
            }
        }
        false
    }

    fn handle_window_event(&mut self, event: WindowEvent<'_>) -> WindowCommand {
        self.drain_pending_win_text();
        match event {
            WindowEvent::CloseRequested => {
                return WindowCommand::CloseWindow;
            }
            WindowEvent::Focused(focused) => {
                self.is_focused = focused;
                if !focused {
                    self.left_mouse_down = false;
                    self.manual_resize = None;
                    self.modifiers = ModifiersState::default();
                    if self.app_state.set_ctrl_held(false) {
                        self.pending_redraw = true;
                    }
                    self.unfocused_since = Some(Instant::now());
                } else {
                    self.unfocused_since = None;
                    self.idle_trimmed = false;
                    self.pending_redraw = true;
                }
            }
            WindowEvent::Resized(new_size) => {
                let minimized = new_size.width == 0
                    || new_size.height == 0
                    || self.window.is_minimized();
                self.is_minimized = minimized;
                if minimized {
                    if !self.minimized_trimmed {
                        self.shrink_for_minimized_all();
                    }
                } else {
                    self.minimized_trimmed = false;
                    self.app_state.window_maximized = self.window.is_maximized();
                    self.pending_redraw = true;
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.app_state.system_dpi_scale = (scale_factor as f32).clamp(0.75, 4.0);
                self.surface_canvas
                    .set_scale_factor(self.app_state.effective_scale());
                self.pending_redraw = true;
            }
            WindowEvent::ReceivedImeText(text) => {
                if !text.is_empty() && !win_text_hook::take_suppressed_ime_text(&self.window, &text) {
                    self.last_shift_press = None;
                    self.app_state.insert_text_at_focus(&text);
                    self.app_state.touch_caret_activity();
                    self.pending_redraw = true;
                    #[cfg(target_os = "macos")]
                    {
                        self.macos_ime_handled_keydown = true;
                    }
                }
            }
            WindowEvent::HoveredFile(path) => {
                let pos = cursor_pos_in_window(&self.window, &self.surface_canvas, self.cursor_pos);
                self.cursor_pos = pos;
                self.app_state.external_drag_hover = Some((path, pos));
                self.pending_redraw = true;
            }
            WindowEvent::HoveredFileCancelled => {
                if self.app_state.external_drag_hover.take().is_some() {
                    self.pending_redraw = true;
                }
            }
            WindowEvent::DroppedFile(path) => {
                let pos = cursor_pos_in_window(&self.window, &self.surface_canvas, self.cursor_pos);
                self.cursor_pos = pos;
                self.app_state.handle_os_dropped_file(path, pos.0, pos.1);
                self.surface_canvas
                    .set_scale_factor(self.app_state.effective_scale());
                self.pending_redraw = true;
            }
            WindowEvent::ModifiersChanged(new_mods) => {
                if process_double_shift_modifier_event(
                    &mut self.app_state,
                    self.modifiers,
                    new_mods,
                    &mut self.last_shift_press,
                    Instant::now(),
                ) {
                    self.pending_redraw = true;
                }
                self.modifiers = new_mods;
                let ctrl_held = (new_mods.control_key()
                    || (cfg!(target_os = "macos") && new_mods.super_key()))
                    && !new_mods.alt_key();
                if self.app_state.set_ctrl_held(ctrl_held) {
                    self.app_state.refresh_ctrl_hover_symbol(
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                        &self.surface_canvas,
                    );
                    sync_window_cursor(
                        &self.window,
                        &self.app_state,
                        &self.surface_canvas,
                        self.cursor_pos,
                        &mut self.last_cursor_icon,
                    );
                    if self
                        .app_state
                        .editor_code_rect
                        .contains(self.cursor_pos.0, self.cursor_pos.1)
                    {
                        self.pending_redraw = true;
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(rs) = self.manual_resize {
                    if let Ok(inner_pos) = self.window.inner_position() {
                        let cur_screen_x = inner_pos.x + position.x.round() as i32;
                        let cur_screen_y = inner_pos.y + position.y.round() as i32;
                        let dx = cur_screen_x - rs.start_cursor_screen.0;
                        let dy = cur_screen_y - rs.start_cursor_screen.1;
                        let min_w = (640.0 * self.surface_canvas.scale_factor).round() as i32;
                        let min_h = (420.0 * self.surface_canvas.scale_factor).round() as i32;
                        let mut new_x = rs.start_pos.0;
                        let mut new_y = rs.start_pos.1;
                        let mut new_w = rs.start_size.0 as i32;
                        let mut new_h = rs.start_size.1 as i32;
                        match rs.dir {
                            ResizeDirection::East
                            | ResizeDirection::NorthEast
                            | ResizeDirection::SouthEast => {
                                new_w = (rs.start_size.0 as i32 + dx).max(min_w);
                            }
                            ResizeDirection::West
                            | ResizeDirection::NorthWest
                            | ResizeDirection::SouthWest => {
                                let clamped_w = (rs.start_size.0 as i32 - dx).max(min_w);
                                new_x = rs.start_pos.0 + (rs.start_size.0 as i32 - clamped_w);
                                new_w = clamped_w;
                            }
                            _ => {}
                        }
                        match rs.dir {
                            ResizeDirection::South
                            | ResizeDirection::SouthEast
                            | ResizeDirection::SouthWest => {
                                new_h = (rs.start_size.1 as i32 + dy).max(min_h);
                            }
                            ResizeDirection::North
                            | ResizeDirection::NorthEast
                            | ResizeDirection::NorthWest => {
                                let clamped_h = (rs.start_size.1 as i32 - dy).max(min_h);
                                new_y = rs.start_pos.1 + (rs.start_size.1 as i32 - clamped_h);
                                new_h = clamped_h;
                            }
                            _ => {}
                        }
                        if new_x != rs.start_pos.0 || new_y != rs.start_pos.1 {
                            self.window
                                .set_outer_position(PhysicalPosition::new(new_x, new_y));
                        }
                        self.window
                            .set_inner_size(PhysicalSize::new(new_w as u32, new_h as u32));
                        return WindowCommand::None;
                    }
                }
                self.cursor_pos = self.surface_canvas.to_logical_pos(position.x, position.y);
                let win_w = self.surface_canvas.width as i32;
                let win_h = self.surface_canvas.height as i32;

                let splitter_dragging = self.app_state.active_splitter_drag.is_some();
                let dragged_split = self.app_state.update_splitter_drag(
                    self.cursor_pos.0,
                    self.cursor_pos.1,
                    win_w,
                    win_h,
                );
                let dragged_dock = self.left_mouse_down
                    && self.app_state.update_dock_drag(
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                        win_w,
                        win_h,
                    );
                let dragged_scroll = self
                    .app_state
                    .update_scrollbar_drag(self.cursor_pos.0, self.cursor_pos.1);
                let dragged_ide_file = self.left_mouse_down
                    && !dragged_split
                    && !splitter_dragging
                    && !dragged_scroll
                    && !dragged_dock
                    && self
                        .app_state
                        .update_ide_drag(self.cursor_pos.0, self.cursor_pos.1);
                let dragged_text = self.left_mouse_down
                    && !dragged_split
                    && !splitter_dragging
                    && !dragged_scroll
                    && !dragged_ide_file
                    && !dragged_dock
                    && self.app_state.handle_input_mouse_drag(
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                        &self.surface_canvas,
                    );
                if dragged_split || dragged_scroll || dragged_ide_file || dragged_text || dragged_dock {
                    self.pending_redraw = true;
                } else if !splitter_dragging
                    && self
                        .app_state
                        .handle_mouse_move(self.cursor_pos.0, self.cursor_pos.1)
                {
                    self.pending_redraw = true;
                }
                if self.app_state.ctrl_held {
                    self.app_state.refresh_ctrl_hover_symbol(
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                        &self.surface_canvas,
                    );
                }
                sync_window_cursor(
                    &self.window,
                    &self.app_state,
                    &self.surface_canvas,
                    self.cursor_pos,
                    &mut self.last_cursor_icon,
                );
            }
            WindowEvent::CursorLeft { .. } => {
                if self.app_state.armed_window_control.take().is_some()
                    || self.app_state.close_top_menu()
                {
                    self.pending_redraw = true;
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    self.last_shift_press = None;
                    self.app_state.dismiss_startup_animation();
                    self.left_mouse_down = true;
                    let win_w = self.surface_canvas.width as i32;
                    let win_h = self.surface_canvas.height as i32;

                    if !self.window.is_maximized() {
                        if let Some(dir) = detect_resize_direction(
                            self.cursor_pos.0,
                            self.cursor_pos.1,
                            win_w,
                            win_h,
                        ) {
                            self.left_mouse_down = false;
                            if self.window.drag_resize_window(dir).is_err() {
                                if let Ok(outer_pos) = self.window.outer_position() {
                                    let inner_size = self.window.inner_size();
                                    let inner_pos =
                                        self.window.inner_position().unwrap_or(outer_pos);
                                    let (phys_cx, phys_cy) = self
                                        .surface_canvas
                                        .to_physical_pos(self.cursor_pos.0, self.cursor_pos.1);
                                    self.manual_resize = Some(ManualResizeState {
                                        dir,
                                        start_cursor_screen: (
                                            inner_pos.x + phys_cx,
                                            inner_pos.y + phys_cy,
                                        ),
                                        start_pos: (outer_pos.x, outer_pos.y),
                                        start_size: (inner_size.width, inner_size.height),
                                    });
                                }
                            }
                            return WindowCommand::None;
                        }
                    }

                    if self.app_state.try_begin_splitter_drag(
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                        win_w,
                        win_h,
                    ) || self
                        .app_state
                        .try_begin_scrollbar_drag(self.cursor_pos.0, self.cursor_pos.1)
                    {
                        self.pending_redraw = true;
                        return WindowCommand::None;
                    }

                    if self
                        .app_state
                        .is_point_in_toolbar_blank(self.cursor_pos.0, self.cursor_pos.1)
                    {
                        if self.app_state.close_top_menu() {
                            self.pending_redraw = true;
                        }
                        let now = Instant::now();
                        let is_double = self
                            .last_titlebar_click
                            .map(|(prev_t, prev_p)| {
                                now.duration_since(prev_t) <= Duration::from_millis(350)
                                    && (self.cursor_pos.0 - prev_p.0).abs() <= 4
                                    && (self.cursor_pos.1 - prev_p.1).abs() <= 4
                            })
                            .unwrap_or(false);
                        if is_double {
                            let next = !self.window.is_maximized();
                            self.window.set_maximized(next);
                            self.app_state.window_maximized = next;
                            self.last_titlebar_click = None;
                        } else {
                            self.last_titlebar_click = Some((now, self.cursor_pos));
                            self.left_mouse_down = false;
                            let _ = self.window.drag_window();
                        }
                        self.pending_redraw = true;
                        return WindowCommand::None;
                    }

                    let now = Instant::now();
                    let streak = self
                        .last_input_click
                        .and_then(|(prev_t, prev_p, prev_s)| {
                            if now.duration_since(prev_t) <= Duration::from_millis(380)
                                && (self.cursor_pos.0 - prev_p.0).abs() <= 4
                                && (self.cursor_pos.1 - prev_p.1).abs() <= 4
                            {
                                Some((prev_s + 1).min(3))
                            } else {
                                None
                            }
                        })
                        .unwrap_or(1);

                    if let Some(cmd) = self
                        .app_state
                        .window_control_at(self.cursor_pos.0, self.cursor_pos.1)
                    {
                        self.app_state.arm_window_control(cmd);
                        self.pending_redraw = true;
                        return WindowCommand::None;
                    }

                    self.app_state
                        .begin_ide_drag_candidate_at(self.cursor_pos.0, self.cursor_pos.1);
                    let ctrl_click = (self.modifiers.control_key()
                        || (cfg!(target_os = "macos") && self.modifiers.super_key()))
                        && !self.modifiers.alt_key();
                    self.app_state.handle_mouse_click_with_input_mods(
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                        self.modifiers.shift_key(),
                        ctrl_click,
                        &self.surface_canvas,
                    );
                    self.surface_canvas
                        .set_scale_factor(self.app_state.effective_scale());
                    sync_window_title(&self.window, &mut self.app_state);
                    if streak == 2 {
                        let _ = self.app_state.handle_input_double_click(
                            self.cursor_pos.0,
                            self.cursor_pos.1,
                            &self.surface_canvas,
                        );
                        self.last_input_click = Some((now, self.cursor_pos, 2));
                    } else if streak >= 3 {
                        let _ = self.app_state.handle_input_triple_click(
                            self.cursor_pos.0,
                            self.cursor_pos.1,
                            &self.surface_canvas,
                        );
                        self.last_input_click = None;
                    } else {
                        self.last_input_click = Some((now, self.cursor_pos, 1));
                    }
                    if let Some(cmd) = self.app_state.take_window_control_cmd() {
                        match cmd {
                            WindowControlCmd::Minimize => {
                                self.window.set_minimized(true);
                            }
                            WindowControlCmd::ToggleMaximize => {
                                let next = !self.window.is_maximized();
                                self.window.set_maximized(next);
                                self.app_state.window_maximized = next;
                            }
                            WindowControlCmd::Close => {
                                return WindowCommand::CloseWindow;
                            }
                        }
                    }
                    self.pending_redraw = true;
                }
                ElementState::Released => {
                    self.left_mouse_down = false;
                    if self.manual_resize.take().is_some() {
                        return WindowCommand::None;
                    }
                    if let Some(cmd) = self
                        .app_state
                        .release_armed_window_control(self.cursor_pos.0, self.cursor_pos.1)
                    {
                        match cmd {
                            WindowControlCmd::Minimize => {
                                self.window.set_minimized(true);
                            }
                            WindowControlCmd::ToggleMaximize => {
                                let next = !self.window.is_maximized();
                                self.window.set_maximized(next);
                                self.app_state.window_maximized = next;
                            }
                            WindowControlCmd::Close => {
                                return WindowCommand::CloseWindow;
                            }
                        }
                    }
                    let _ = self
                        .app_state
                        .finish_ide_drag_at(self.cursor_pos.0, self.cursor_pos.1);
                    self.app_state.end_dock_drag(
                        self.cursor_pos.0,
                        self.cursor_pos.1,
                        self.surface_canvas.width as i32,
                        self.surface_canvas.height as i32,
                    );
                    self.app_state.end_splitter_drag();
                    self.app_state.save_global_settings();
                    self.app_state.end_scrollbar_drag();
                    self.app_state.end_mouse_text_drag();
                    sync_window_cursor(
                        &self.window,
                        &self.app_state,
                        &self.surface_canvas,
                        self.cursor_pos,
                        &mut self.last_cursor_icon,
                    );
                    self.pending_redraw = true;
                }
                _ => {}
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                self.app_state.open_context_menu_at(
                    self.cursor_pos.0,
                    self.cursor_pos.1,
                    &self.surface_canvas,
                );
                self.pending_redraw = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let raw_dy_for_zoom = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y,
                    MouseScrollDelta::PixelDelta(p) => {
                        let px_per_line =
                            (22.0 * self.surface_canvas.scale_factor).max(12.0) as f64;
                        (-p.y / px_per_line) as f32
                    }
                    _ => 0.0,
                };

                let zoom_mod = self.modifiers.control_key()
                    || (cfg!(target_os = "macos") && self.modifiers.super_key());
                if zoom_mod && !self.modifiers.alt_key() {
                    if raw_dy_for_zoom < -0.01 {
                        self.app_state.execute_ide_action(IdeAction::ZoomInUi);
                        self.surface_canvas
                            .set_scale_factor(self.app_state.effective_scale());
                        self.pending_redraw = true;
                    } else if raw_dy_for_zoom > 0.01 {
                        self.app_state.execute_ide_action(IdeAction::ZoomOutUi);
                        self.surface_canvas
                            .set_scale_factor(self.app_state.effective_scale());
                        self.pending_redraw = true;
                    }
                } else {
                    let wheel_mode = current_system_wheel_mode();
                    let page_lines = self
                        .app_state
                        .wheel_page_lines_at(self.cursor_pos.0, self.cursor_pos.1);
                    let (dx_px, dy_lines) = resolve_wheel_scroll_deltas(
                        &delta,
                        wheel_mode,
                        page_lines,
                        self.surface_canvas.scale_factor,
                        self.modifiers.shift_key(),
                    );
                    if dx_px.abs() > 0.001 || dy_lines.abs() > 0.001 {
                        let now = Instant::now();
                        let dir = if dy_lines.abs() > 0.001 {
                            dy_lines.signum()
                        } else {
                            dx_px.signum()
                        };
                        let idle_reset = self
                            .last_wheel_time
                            .is_some_and(|prev| now.duration_since(prev).as_millis() > 500);
                        if idle_reset || (dir * self.last_wheel_dir) < 0.0 {
                            self.wheel_accum_y = 0.0;
                            self.wheel_accum_x = 0.0;
                        }
                        self.last_wheel_time = Some(now);
                        if dir != 0.0 {
                            self.last_wheel_dir = dir;
                        }

                        self.wheel_accum_y += dy_lines;
                        self.wheel_accum_x += dx_px;

                        let lines_delta = consume_wheel_accum(&mut self.wheel_accum_y);
                        let horiz_px_delta = consume_wheel_accum(&mut self.wheel_accum_x);

                        if lines_delta != 0 || horiz_px_delta != 0 {
                            self.app_state.handle_mouse_wheel_2d(
                                self.cursor_pos.0,
                                self.cursor_pos.1,
                                lines_delta,
                                horiz_px_delta,
                            );
                            self.pending_redraw = true;
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        text,
                        ..
                    },
                ..
            } => {
                let ime_committed = if cfg!(target_os = "macos") {
                    std::mem::take(&mut self.macos_ime_handled_keydown)
                } else {
                    false
                };
                if is_non_visual_key(&logical_key) && text.is_none() {
                    return WindowCommand::None;
                }
                self.last_shift_press = None;
                let allow_text = should_insert_key_text(
                    cfg!(any(target_os = "macos", target_os = "windows")),
                    ime_committed,
                    text.is_some(),
                );
                let cmd = self.handle_keyboard_pressed(
                    logical_key,
                    text.as_deref(),
                    allow_text,
                    ime_committed,
                );
                if cmd == WindowCommand::CloseWindow {
                    return WindowCommand::CloseWindow;
                }
                self.app_state.touch_caret_activity();
                self.surface_canvas
                    .set_scale_factor(self.app_state.effective_scale());
                self.pending_redraw = true;
            }
            _ => {}
        }
        WindowCommand::None
    }

    fn handle_keyboard_pressed(
        &mut self,
        logical_key: Key<'_>,
        key_text: Option<&str>,
        allow_text: bool,
        ime_committed: bool,
    ) -> WindowCommand {
        dispatch_keyboard_pressed(
            &mut self.app_state,
            &mut self.surface_canvas,
            self.modifiers,
            logical_key,
            key_text,
            allow_text,
            ime_committed,
        )
    }
}

fn process_double_shift_modifier_event(
    app_state: &mut IdeApplicationState,
    prev_mods: ModifiersState,
    new_mods: ModifiersState,
    last_shift_press: &mut Option<Instant>,
    now: Instant,
) -> bool {
    if app_state.suppresses_double_shift() {
        *last_shift_press = None;
        return false;
    }
    if new_mods.shift_key()
        && !prev_mods.shift_key()
        && !new_mods.control_key()
        && !new_mods.alt_key()
        && !new_mods.super_key()
    {
        if let Some(prev) = *last_shift_press {
            if now.duration_since(prev) <= Duration::from_millis(400) {
                app_state.execute_ide_action(IdeAction::SearchEverywhere);
                *last_shift_press = None;
                return true;
            }
        }
        *last_shift_press = Some(now);
    }
    false
}

fn dispatch_keyboard_pressed(
    app_state: &mut IdeApplicationState,
    surface_canvas: &mut SurfaceCanvas,
    modifiers: ModifiersState,
    logical_key: Key<'_>,
    key_text: Option<&str>,
    allow_text: bool,
    ime_committed: bool,
) -> WindowCommand {
        let raw_ctrl = modifiers.control_key();
        let cmd = cfg!(target_os = "macos") && modifiers.super_key();
        let ctrl = raw_ctrl || cmd;
        let alt = modifiers.alt_key();
        let shift = modifiers.shift_key();
        let keymap = app_state.keymap_scheme;
        let block_global_shortcuts = app_state.is_text_input_focused()
            || !matches!(
                app_state.focus,
                FocusTarget::Editor | FocusTarget::AgentChatStream
            );

        match &logical_key {
                        #[cfg(target_os = "macos")]
                        Key::Character(ch) if cmd && !alt && !shift && ch.eq_ignore_ascii_case("q") => {
                            return WindowCommand::CloseWindow;
                        }
                        #[cfg(target_os = "macos")]
                        Key::Character(ch)
                            if cmd
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("w") =>
                        {
                            app_state.execute_ide_action(IdeAction::CloseActiveTab);
                        }
                        #[cfg(target_os = "macos")]
                        Key::Character(ch)
                            if cmd
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && *ch == "," =>
                        {
                            app_state.execute_ide_action(IdeAction::OpenSettingsModal);
                        }
                        // 1. Ctrl+Alt 组合快捷键
                        Key::Character(ch)
                            if ctrl
                                && alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("s") =>
                        {
                            app_state.execute_ide_action(IdeAction::OpenSettingsModal);
                        }
                        Key::Character(ch)
                            if ctrl
                                && alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("u") =>
                        {
                            app_state.execute_ide_action(IdeAction::SwitchUiLanguage);
                        }
                        Key::Character(ch)
                            if ctrl
                                && alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea
                                && ch.eq_ignore_ascii_case("l") =>
                        {
                            app_state.execute_ide_action(IdeAction::ReformatCode);
                        }
                        Key::Character(ch)
                            if ctrl
                                && alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("z") =>
                        {
                            app_state.execute_ide_action(IdeAction::RevertLastCheckpoint);
                        }
                        Key::Character(ch)
                            if ctrl
                                && alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea
                                && ch.eq_ignore_ascii_case("n") =>
                        {
                            app_state.execute_ide_action(IdeAction::NewFolderInProject);
                        }
                        Key::Character(ch)
                            if ctrl
                                && alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("b") =>
                        {
                            app_state.execute_ide_action(IdeAction::GitNewBranch);
                        }
                        // 2. Ctrl+Shift 组合快捷键 (IDEA / Eclipse)
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea
                                && ch.eq_ignore_ascii_case("a") =>
                        {
                            app_state.execute_ide_action(IdeAction::FindAction);
                        }
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && ch.eq_ignore_ascii_case("l") =>
                        {
                            app_state.execute_ide_action(IdeAction::FindAction);
                        }
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea
                                && ch.eq_ignore_ascii_case("n") =>
                        {
                            app_state.execute_ide_action(IdeAction::GoToFile);
                        }
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && ch.eq_ignore_ascii_case("r") =>
                        {
                            app_state.execute_ide_action(IdeAction::GoToFile);
                        }
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("f") =>
                        {
                            if keymap == KeymapScheme::Eclipse {
                                app_state.execute_ide_action(IdeAction::ReformatCode);
                            } else {
                                app_state.execute_ide_action(IdeAction::FindInFiles);
                            }
                        }
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && ch.eq_ignore_ascii_case("c") =>
                        {
                            app_state.execute_ide_action(IdeAction::ToggleLineComment);
                        }
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && ch.eq_ignore_ascii_case("b") =>
                        {
                            app_state.execute_ide_action(IdeAction::ToggleBreakpoint);
                        }
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("k") =>
                        {
                            app_state.execute_ide_action(IdeAction::GitPush);
                        }
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("g") =>
                        {
                            app_state.execute_ide_action(IdeAction::GitBranches);
                        }
                        Key::Character(ch)
                            if ctrl
                                && shift
                                && !alt
                                && (!block_global_shortcuts || app_state.active_text_input().is_some())
                                && ch.eq_ignore_ascii_case("z") =>
                        {
                            app_state.execute_ide_action(IdeAction::RedoEdit);
                        }
                        // 3. Alt+Shift 组合快捷键 (Eclipse Refactor / New / Run)
                        Key::Character(ch)
                            if alt
                                && shift
                                && !ctrl
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && ch.eq_ignore_ascii_case("r") =>
                        {
                            app_state.execute_ide_action(IdeAction::RenameSymbol);
                        }
                        Key::Character(ch)
                            if alt
                                && shift
                                && !ctrl
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && ch.eq_ignore_ascii_case("n") =>
                        {
                            app_state.execute_ide_action(IdeAction::NewFolderInProject);
                        }
                        Key::Character(ch)
                            if alt
                                && shift
                                && !ctrl
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && ch.eq_ignore_ascii_case("x") =>
                        {
                            app_state.execute_ide_action(IdeAction::RunCurrentFile);
                        }
                        // 4. Ctrl 单修饰组合快捷键
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("o") =>
                        {
                            app_state.execute_ide_action(IdeAction::OpenFolder);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && (!block_global_shortcuts
                                    || matches!(
                                        app_state.focus,
                                        FocusTarget::EditorFindInput | FocusTarget::EditorReplaceInput
                                    ))
                                && ch.eq_ignore_ascii_case("f") =>
                        {
                            app_state.open_editor_find_bar(false);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && (!block_global_shortcuts
                                    || matches!(
                                        app_state.focus,
                                        FocusTarget::EditorFindInput | FocusTarget::EditorReplaceInput
                                    ))
                                && ch.eq_ignore_ascii_case("r") =>
                        {
                            app_state.open_editor_find_bar(true);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("h") =>
                        {
                            if keymap == KeymapScheme::Eclipse {
                                app_state.execute_ide_action(IdeAction::FindInFiles);
                            } else {
                                app_state.open_editor_find_bar(true);
                            }
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea
                                && ch.eq_ignore_ascii_case("g") =>
                        {
                            app_state.open_go_to_line_modal();
                        }
                        Key::Character(ch) if ctrl && !alt && !shift && ch.eq_ignore_ascii_case("a") => {
                            let _ = app_state.select_all_at_focus();
                        }
                        Key::Character(ch) if ctrl && !alt && !shift && ch.eq_ignore_ascii_case("c") => {
                            if let Some(sel) = app_state.copy_focused_selection() {
                                write_windows_clipboard_text(&sel);
                                if app_state.focus == FocusTarget::TerminalInput {
                                    let _ = app_state.clear_active_input_selection();
                                }
                            } else if !cmd && app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_interrupt();
                            }
                        }
                        Key::Character(ch) if ctrl && !alt && !shift && ch.eq_ignore_ascii_case("x") => {
                            if let Some(cut) = app_state.cut_focused_selection() {
                                write_windows_clipboard_text(&cut);
                            }
                        }
                        Key::Character(ch) if ctrl && !alt && !shift && ch.eq_ignore_ascii_case("v") => {
                            app_state.paste_into_agent_or_focus();
                        }
                        Key::Character(ch) if ctrl && !alt && !shift && ch.eq_ignore_ascii_case("l") => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                let pty = app_state.terminal.active_pty_mut();
                                pty.clear();
                                pty.send_raw_bytes(b"\x0c");
                            } else if !block_global_shortcuts && keymap == KeymapScheme::Eclipse {
                                app_state.open_go_to_line_modal();
                            }
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && ch.eq_ignore_ascii_case("w") =>
                        {
                            app_state.execute_ide_action(IdeAction::CloseActiveTab);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && *ch == "1" =>
                        {
                            app_state.execute_ide_action(IdeAction::ShowIntentionActions);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse
                                && *ch == "3" =>
                        {
                            app_state.execute_ide_action(IdeAction::SearchEverywhere);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("p") =>
                        {
                            app_state.execute_ide_action(IdeAction::SearchEverywhere);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("s") =>
                        {
                            app_state.execute_ide_action(IdeAction::SaveActiveFile);
                        }
                        Key::Character(ch) if ctrl && !alt && !shift && ch.eq_ignore_ascii_case("d") => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x04");
                            } else if !block_global_shortcuts {
                                if keymap == KeymapScheme::Eclipse {
                                    app_state.execute_ide_action(IdeAction::DeleteLine);
                                } else {
                                    app_state.execute_ide_action(IdeAction::DuplicateLine);
                                }
                            }
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && (!block_global_shortcuts || app_state.active_text_input().is_some())
                                && ch.eq_ignore_ascii_case("y") =>
                        {
                            if keymap == KeymapScheme::Eclipse {
                                app_state.execute_ide_action(IdeAction::RedoEdit);
                            } else if app_state.active_text_input().is_some() {
                                app_state.execute_ide_action(IdeAction::RedoEdit);
                            } else if app_state.agent_hub.inline_edit.is_some() {
                                app_state.accept_inline_diff();
                            } else {
                                app_state.execute_ide_action(IdeAction::DeleteLine);
                            }
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea
                                && ch.eq_ignore_ascii_case("b") =>
                        {
                            app_state.execute_ide_action(IdeAction::GoToDeclaration);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("t") =>
                        {
                            app_state.execute_ide_action(IdeAction::GitPull);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && ch.eq_ignore_ascii_case("n") =>
                        {
                            app_state.execute_ide_action(IdeAction::NewFileInProject);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && *ch == "/" =>
                        {
                            app_state.execute_ide_action(IdeAction::ToggleLineComment);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && *ch == "`" =>
                        {
                            app_state.execute_ide_action(IdeAction::SwitchTheme);
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !block_global_shortcuts
                                && (*ch == "=" || *ch == "+") =>
                        {
                            app_state.execute_ide_action(IdeAction::ZoomInUi);
                            surface_canvas.set_scale_factor(app_state.effective_scale());
                        }
                        Key::Character(ch)
                            if ctrl && !alt && !block_global_shortcuts && *ch == "-" =>
                        {
                            app_state.execute_ide_action(IdeAction::ZoomOutUi);
                            surface_canvas.set_scale_factor(app_state.effective_scale());
                        }
                        Key::Character(ch)
                            if ctrl && !alt && !block_global_shortcuts && *ch == "0" =>
                        {
                            app_state.execute_ide_action(IdeAction::ResetUiZoom);
                            surface_canvas.set_scale_factor(app_state.effective_scale());
                        }
                        Key::Character(ch)
                            if ctrl
                                && !alt
                                && !shift
                                && (!block_global_shortcuts || app_state.active_text_input().is_some())
                                && ch.eq_ignore_ascii_case("z") =>
                        {
                            app_state.execute_ide_action(IdeAction::UndoEdit);
                        }
                        // Alt+0..9 IDEA Tool Window 快捷键
                        Key::Character(ch)
                            if alt && !ctrl && !shift && !block_global_shortcuts && *ch == "1" =>
                        {
                            app_state.execute_ide_action(IdeAction::ToggleProjectWindow);
                        }
                        Key::Character(ch)
                            if alt && !ctrl && !shift && !block_global_shortcuts && *ch == "0" =>
                        {
                            app_state.execute_ide_action(IdeAction::ToggleCommitWindow);
                        }
                        Key::Character(ch)
                            if alt && !ctrl && !shift && !block_global_shortcuts && *ch == "7" =>
                        {
                            app_state.execute_ide_action(IdeAction::ToggleStructureWindow);
                        }
                        Key::Character(ch)
                            if alt && !ctrl && !shift && !block_global_shortcuts && *ch == "8" =>
                        {
                            app_state.execute_ide_action(IdeAction::ToggleAgentHubWindow);
                        }
                        Key::Character(ch)
                            if alt && !ctrl && !shift && !block_global_shortcuts && *ch == "9" =>
                        {
                            app_state.execute_ide_action(IdeAction::GitShowLog);
                        }
                        // 功能键 F3..F12 (适配 IntelliJ IDEA / Eclipse 双方案)
                        Key::F3
                            if !ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse =>
                        {
                            app_state.execute_ide_action(IdeAction::GoToDeclaration);
                        }
                        Key::F4 if ctrl && !alt && !shift && !block_global_shortcuts => {
                            app_state.execute_ide_action(IdeAction::CloseActiveTab);
                        }
                        Key::F5
                            if !ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse =>
                        {
                            app_state.debug_step_into();
                        }
                        Key::F6
                            if shift
                                && !ctrl
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea =>
                        {
                            app_state.execute_ide_action(IdeAction::RenameSymbol);
                        }
                        Key::F6
                            if !ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse =>
                        {
                            app_state.debug_step_over();
                        }
                        Key::F7 if !ctrl && !alt && !shift && !block_global_shortcuts => {
                            if keymap == KeymapScheme::Eclipse {
                                app_state.debug_step_out();
                            } else {
                                app_state.debug_step_into();
                            }
                        }
                        Key::F8
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea =>
                        {
                            app_state.execute_ide_action(IdeAction::ToggleBreakpoint);
                        }
                        Key::F8
                            if shift
                                && !ctrl
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea =>
                        {
                            app_state.debug_step_out();
                        }
                        Key::F8 if !ctrl && !alt && !shift && !block_global_shortcuts => {
                            if keymap == KeymapScheme::Eclipse {
                                app_state.debug_continue();
                            } else {
                                app_state.debug_step_over();
                            }
                        }
                        Key::F9
                            if shift
                                && !ctrl
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea =>
                        {
                            app_state.execute_ide_action(IdeAction::DebugActiveConfig);
                        }
                        Key::F9
                            if !ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea =>
                        {
                            app_state.debug_continue();
                        }
                        Key::F10 if ctrl && shift && !alt && !block_global_shortcuts => {
                            app_state.execute_ide_action(IdeAction::RunCurrentFile);
                        }
                        Key::F10
                            if shift
                                && !ctrl
                                && !alt
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::IntelliJIdea =>
                        {
                            app_state.execute_ide_action(IdeAction::RunActiveConfig);
                        }
                        Key::F11
                            if ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse =>
                        {
                            app_state.execute_ide_action(IdeAction::RunActiveConfig);
                        }
                        Key::F11
                            if !ctrl
                                && !alt
                                && !shift
                                && !block_global_shortcuts
                                && keymap == KeymapScheme::Eclipse =>
                        {
                            app_state.execute_ide_action(IdeAction::DebugActiveConfig);
                        }
                        Key::F12 if alt && !ctrl && !shift && !block_global_shortcuts => {
                            app_state.execute_ide_action(IdeAction::ToggleTerminalWindow);
                        }
                        Key::Insert if alt && !block_global_shortcuts => {
                            app_state.execute_ide_action(IdeAction::NewFileInProject);
                        }
                        Key::Delete => {
                            if alt && !block_global_shortcuts {
                                app_state.execute_ide_action(IdeAction::DeleteSelectedPath);
                            } else if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[3~");
                            } else if ctrl {
                                if !app_state
                                    .handle_focused_key_nav(TextInputNavAction::DeleteWordForward)
                                    && app_state.focus == FocusTarget::Editor
                                {
                                    app_state.active_buffer_mut().delete_word_forward();
                                    app_state.ensure_editor_cursor_visible();
                                }
                            } else if !app_state
                                .handle_focused_key_nav(TextInputNavAction::DeleteForward)
                                && app_state.focus == FocusTarget::Editor
                            {
                                app_state.active_buffer_mut().delete_forward_at_cursor();
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::Tab => {
                            if let FocusTarget::AiConfigModal(fi) = app_state.focus {
                                app_state.focus = FocusTarget::AiConfigModal((fi + 1) % 3);
                            } else if app_state.focus == FocusTarget::CloneGitRepoModal {
                                app_state.clone_modal_field_focus =
                                    1 - app_state.clone_modal_field_focus.min(1);
                                app_state.touch_caret_activity();
                            } else if app_state.focus == FocusTarget::EditorFindInput {
                                app_state.editor_replace_open = true;
                                app_state.focus = FocusTarget::EditorReplaceInput;
                            } else if app_state.focus == FocusTarget::EditorReplaceInput {
                                app_state.focus = FocusTarget::EditorFindInput;
                            } else if app_state.agent_hub.inline_edit.is_some()
                                && matches!(
                                    app_state.focus,
                                    FocusTarget::Editor | FocusTarget::CtrlKPrompt
                                )
                            {
                                app_state.accept_inline_diff();
                            } else if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\t");
                            } else if app_state.focus == FocusTarget::Editor {
                                if shift {
                                    app_state.active_buffer_mut().unindent_selection_or_line();
                                } else {
                                    app_state.active_buffer_mut().indent_selection_or_line();
                                }
                                app_state.refresh_active_buffer_diagnostics();
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::Escape => {
                            let had_menu = app_state.context_menu.take().is_some();
                            let had_popup = app_state.active_menu_category.is_some()
                                || app_state.git_branch_popup_open
                                || app_state.agent_provider_popup_open
                                || app_state.agent_model_popup_open
                                || app_state.agent_mode_popup_open
                                || app_state.agent_sessions_popup_open
                                || app_state.run_config_popup_open
                                || app_state.mobile_popup_open
                                || app_state.project_popup_open
                                || app_state.more_menu_popup_open;
                            app_state.active_menu_category = None;
                            app_state.git_branch_popup_open = false;
                            app_state.agent_provider_popup_open = false;
                            app_state.agent_model_popup_open = false;
                            app_state.agent_mode_popup_open = false;
                            app_state.agent_sessions_popup_open = false;
                            app_state.run_config_popup_open = false;
                            app_state.mobile_popup_open = false;
                            app_state.project_popup_open = false;
                            app_state.more_menu_popup_open = false;
                            if had_menu || had_popup {
                                // 首次 Esc 优先关闭上下文菜单或弹出下拉框
                            } else if app_state.focus == FocusTarget::ImagePreviewModal {
                                app_state.close_image_preview();
                            } else if matches!(
                                app_state.focus,
                                FocusTarget::EditorFindInput | FocusTarget::EditorReplaceInput
                            ) || app_state.editor_find_open
                            {
                                app_state.close_editor_find_bar();
                            } else if app_state.clear_active_input_selection() {
                                // 首次 Esc 仅取消当前文本选区，保持焦点流畅
                            } else if matches!(app_state.focus, FocusTarget::AiConfigModal(_)) {
                                app_state.focus = FocusTarget::AgentChatInput;
                            } else if app_state.agent_hub.inline_edit.is_some() {
                                app_state.reject_inline_diff();
                            } else if app_state.focus == FocusTarget::OpenProjectChoiceModal {
                                app_state.pending_open_workspace = None;
                                app_state.focus = FocusTarget::Editor;
                            } else if app_state.focus == FocusTarget::EditMessageRewindModal {
                                app_state.cancel_edit_message_rewind_modal();
                            } else if app_state.editing_sent_msg_idx.is_some()
                                && app_state.focus == FocusTarget::AgentChatInput
                            {
                                app_state.cancel_edit_sent_user_message();
                            } else if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b");
                            } else if app_state.focus == FocusTarget::SettingsModal {
                                app_state.close_settings_modal();
                            } else {
                                app_state.focus = FocusTarget::Editor;
                            }
                        }
                        Key::Backspace => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                let pty = app_state.terminal.active_pty_mut();
                                pty.clear_selection();
                                pty.send_raw_bytes(b"\x7f");
                            } else if ctrl && !alt {
                                if !app_state
                                    .handle_focused_key_nav(TextInputNavAction::DeleteWordBackward)
                                {
                                    if app_state.focus == FocusTarget::Editor {
                                        app_state.active_buffer_mut().delete_word_backward();
                                        app_state.ensure_editor_cursor_visible();
                                    } else {
                                        app_state.backspace_at_focus();
                                    }
                                }
                            } else {
                                app_state.backspace_at_focus();
                            }
                        }
                        Key::Space if !ctrl && !alt && allow_text => {
                            app_state.insert_text_at_focus(" ");
                        }
                        Key::Enter if !ime_committed => match app_state.focus {
                            FocusTarget::SearchEverywhereModal => {
                                match app_state.search_results.first().cloned() {
                                    Some(ide_search::SearchEverywhereItem::File {
                                        full_path,
                                        ..
                                    }) => {
                                        app_state.open_file(full_path);
                                        app_state.focus = FocusTarget::Editor;
                                    }
                                    Some(ide_search::SearchEverywhereItem::Symbol {
                                        full_path,
                                        line,
                                        ..
                                    }) => {
                                        app_state.open_file(full_path);
                                        let target_line = line.saturating_sub(1);
                                        app_state
                                            .active_buffer_mut()
                                            .set_cursor(target_line, 0, false);
                                        app_state.scroll_line = target_line.saturating_sub(4);
                                        app_state.ensure_editor_cursor_visible();
                                        app_state.focus = FocusTarget::Editor;
                                    }
                                    Some(ide_search::SearchEverywhereItem::Action {
                                        action,
                                        ..
                                    }) => {
                                        app_state.focus = FocusTarget::Editor;
                                        app_state.execute_ide_action(action);
                                    }
                                    _ => {}
                                }
                            }
                            FocusTarget::FindInFilesModal => {
                                if let Some(first) = app_state.content_search_hits.first().cloned()
                                {
                                    app_state.open_file(first.path);
                                    app_state
                                        .active_buffer_mut()
                                        .set_cursor(first.line_number.saturating_sub(1), 0, false);
                                    app_state.ensure_editor_cursor_visible();
                                    app_state.focus = FocusTarget::Editor;
                                }
                            }
                            FocusTarget::NewFileModal => {
                                let name = app_state.modal_text_input.as_str().to_string();
                                if let Ok(created) = app_state.project_tree.create_new_file(&name) {
                                    app_state.open_file(created);
                                    app_state.git_engine.request_refresh();
                                    app_state.refresh_workspace_file_index_async();
                                }
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::NewRunConfigModal => {
                                let cmd_line = app_state.modal_text_input.as_str().to_string();
                                app_state.add_and_run_command_line(&cmd_line);
                            }
                            FocusTarget::NewFolderModal => {
                                let name = app_state.modal_text_input.as_str().to_string();
                                if app_state.project_tree.create_new_directory(&name).is_ok() {
                                    app_state.refresh_workspace_file_index_async();
                                }
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::NewGitBranchModal => {
                                let name = app_state.modal_text_input.as_str().to_string();
                                if let Some(start_ref) = app_state.git_new_branch_from_ref.take() {
                                    app_state
                                        .git_engine
                                        .request_create_branch_at(&name, &start_ref);
                                } else {
                                    app_state
                                        .git_engine
                                        .request_create_and_checkout_branch(&name);
                                }
                                app_state.status_notification =
                                    format!("正在创建并切换分支 '{}'...", name.trim());
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::NewGitTagModal => {
                                let name = app_state.modal_text_input.as_str().trim().to_string();
                                if !name.is_empty() {
                                    app_state.git_engine.request_create_tag(&name, None);
                                    app_state.status_notification =
                                        format!("正在创建 Git 标签 '{name}'...");
                                }
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::NewGitStashModal => {
                                let msg = app_state.modal_text_input.as_str().trim().to_string();
                                app_state.git_engine.request_stash_save(&msg, true);
                                app_state.status_notification =
                                    "正在将未提交变更暂存至 Stash...".to_string();
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::GitAddRemoteModal => {
                                let raw = app_state.modal_text_input.as_str().trim().to_string();
                                let mut parts = raw.split_whitespace();
                                if let (Some(rname), Some(rurl)) = (parts.next(), parts.next()) {
                                    app_state.git_engine.request_add_remote(rname, rurl);
                                    app_state.status_notification =
                                        format!("正在添加远程仓库 '{rname}'...");
                                }
                                app_state.focus = FocusTarget::GitRemotesModal;
                            }
                            FocusTarget::RenamePathModal => {
                                let new_name = app_state.modal_text_input.as_str().trim().to_string();
                                if let Some(old_path) = app_state.rename_target_path.take() {
                                    match app_state.project_tree.rename_path(&old_path, &new_name) {
                                        Ok(new_path) => {
                                            for buf in &mut app_state.open_buffers {
                                                if buf.path == old_path {
                                                    buf.path = new_path.clone();
                                                    buf.language =
                                                        ide_core::LanguageId::from_path(&buf.path);
                                                } else if let Ok(rel) = buf.path.strip_prefix(&old_path) {
                                                    buf.path = new_path.join(rel);
                                                    buf.language =
                                                        ide_core::LanguageId::from_path(&buf.path);
                                                }
                                            }
                                            app_state.git_engine.request_refresh();
                                            app_state.refresh_workspace_file_index_async();
                                            app_state.status_notification =
                                                format!("已重命名为 {}", new_path.display());
                                        }
                                        Err(e) => {
                                            app_state.status_notification =
                                                format!("重命名失败: {e}");
                                        }
                                    }
                                }
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::RenameSymbolModal => {
                                app_state.submit_rename_symbol();
                            }
                            FocusTarget::GoToLineModal => {
                                app_state.submit_go_to_line();
                            }
                            FocusTarget::AddDebugWatchModal => {
                                app_state.submit_add_debug_watch();
                            }
                            FocusTarget::EditorFindInput => {
                                if shift {
                                    app_state.editor_find_prev();
                                } else {
                                    app_state.editor_find_next();
                                }
                            }
                            FocusTarget::EditorReplaceInput => {
                                if shift {
                                    app_state.editor_replace_all();
                                } else {
                                    app_state.editor_replace_one();
                                }
                            }
                            FocusTarget::CtrlKPrompt => {
                                app_state.trigger_ctrl_k_inline_edit();
                            }
                            FocusTarget::AgentChatInput => {
                                if shift {
                                    app_state.insert_text_at_focus("\n");
                                } else {
                                    let prompt = app_state.agent_chat_input.take_and_push_history();
                                    let has_extras = !app_state
                                        .agent_hub
                                        .active_session()
                                        .attached_items
                                        .is_empty()
                                        || !app_state
                                            .agent_hub
                                            .active_session()
                                            .pasted_snippets
                                            .is_empty();
                                    if !prompt.trim().is_empty() || has_extras {
                                        app_state.submit_agent_prompt(prompt);
                                    }
                                }
                            }
                            FocusTarget::AgentChatStream => {}
                            FocusTarget::ImagePreviewModal => {
                                app_state.close_image_preview();
                            }
                            FocusTarget::TerminalInput => {
                                app_state.record_terminal_prompt_command_before_enter();
                                let pty = app_state.terminal.active_pty_mut();
                                pty.clear_selection();
                                pty.scroll_offset = 0;
                                pty.send_raw_bytes(b"\r");
                            }
                            FocusTarget::GitCommitInput => {
                                if shift {
                                    app_state.insert_text_at_focus("\n");
                                } else {
                                    app_state.submit_git_commit(false);
                                }
                            }
                            FocusTarget::GitLogSearchInput => {
                                app_state.git_log_scroll = 0;
                            }
                            FocusTarget::AiConfigModal(_) => {
                                app_state.save_ai_config_modal();
                            }
                            FocusTarget::CloneGitRepoModal => {
                                app_state.submit_git_clone(shift);
                            }
                            FocusTarget::NodeMissingModal => {
                                app_state.recheck_node_and_retry_install();
                            }
                            FocusTarget::SettingsModal => {
                                if app_state.settings_sdk_focus.is_some() {
                                    app_state.commit_mobile_sdk_locations_from_inputs();
                                    app_state.settings_sdk_focus = None;
                                } else {
                                    app_state.close_settings_modal();
                                }
                            }
                            FocusTarget::OpenProjectChoiceModal => {
                                if let Some(p) = app_state.pending_open_workspace.take() {
                                    if shift {
                                        app_state.request_switch_workspace(p);
                                    } else {
                                        app_state.open_workspace_in_new_window(p);
                                    }
                                }
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::RecentProjectsModal => {
                                app_state.confirm_selected_recent_project();
                            }
                            FocusTarget::EditMessageRewindModal => {
                                app_state.confirm_edit_message_rewind(!shift);
                            }
                            FocusTarget::GitMergeModal
                            | FocusTarget::GitRebaseModal
                            | FocusTarget::GitUnstashModal
                            | FocusTarget::GitResetModal
                            | FocusTarget::GitRollbackModal
                            | FocusTarget::GitRemotesModal => {}
                            FocusTarget::Editor => {
                                if alt {
                                    app_state.execute_ide_action(IdeAction::ShowIntentionActions);
                                } else {
                                    app_state.active_buffer_mut().insert_newline_with_auto_indent();
                                    app_state.ensure_editor_cursor_visible();
                                }
                            }
                        },
                        Key::ArrowLeft => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                let seq: &[u8] = if ctrl { b"\x1b[1;5D" } else { b"\x1b[D" };
                                app_state.terminal.active_pty_mut().send_raw_bytes(seq);
                            } else {
                                let nav = if ctrl {
                                    TextInputNavAction::WordLeft { extend: shift }
                                } else {
                                    TextInputNavAction::Left { extend: shift }
                                };
                                if !app_state.handle_focused_key_nav(nav)
                                    && app_state.focus == FocusTarget::Editor
                                {
                                    let buf = app_state.active_buffer_mut();
                                    if ctrl {
                                        buf.move_word_left(shift);
                                    } else {
                                        buf.move_left(shift);
                                    }
                                    app_state.ensure_editor_cursor_visible();
                                }
                            }
                        }
                        Key::ArrowRight => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                let seq: &[u8] = if ctrl { b"\x1b[1;5C" } else { b"\x1b[C" };
                                app_state.terminal.active_pty_mut().send_raw_bytes(seq);
                            } else {
                                let nav = if ctrl {
                                    TextInputNavAction::WordRight { extend: shift }
                                } else {
                                    TextInputNavAction::Right { extend: shift }
                                };
                                if !app_state.handle_focused_key_nav(nav)
                                    && app_state.focus == FocusTarget::Editor
                                {
                                    let buf = app_state.active_buffer_mut();
                                    if ctrl {
                                        buf.move_word_right(shift);
                                    } else {
                                        buf.move_right(shift);
                                    }
                                    app_state.ensure_editor_cursor_visible();
                                }
                            }
                        }
                        Key::Home => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[H");
                            } else if !app_state
                                .handle_focused_key_nav(TextInputNavAction::Home { extend: shift })
                                && app_state.focus == FocusTarget::Editor
                            {
                                let buf = app_state.active_buffer_mut();
                                if ctrl {
                                    buf.move_doc_start(shift);
                                } else {
                                    buf.move_home(shift);
                                }
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::End => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[F");
                            } else if !app_state
                                .handle_focused_key_nav(TextInputNavAction::End { extend: shift })
                                && app_state.focus == FocusTarget::Editor
                            {
                                let buf = app_state.active_buffer_mut();
                                if ctrl {
                                    buf.move_doc_end(shift);
                                } else {
                                    buf.move_end(shift);
                                }
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::PageUp => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[5~");
                            } else if app_state.focus == FocusTarget::RecentProjectsModal {
                                let vis = app_state.recent_projects_max_visible.max(1) as isize;
                                app_state.move_recent_projects_selection(-vis);
                            } else if app_state.focus == FocusTarget::Editor {
                                let vis = app_state.editor_visible_lines.max(4) as isize;
                                app_state.active_buffer_mut().move_page(-vis, shift);
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::PageDown => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[6~");
                            } else if app_state.focus == FocusTarget::RecentProjectsModal {
                                let vis = app_state.recent_projects_max_visible.max(1) as isize;
                                app_state.move_recent_projects_selection(vis);
                            } else if app_state.focus == FocusTarget::Editor {
                                let vis = app_state.editor_visible_lines.max(4) as isize;
                                app_state.active_buffer_mut().move_page(vis, shift);
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::ArrowUp => {
                            if ctrl && alt && !shift && keymap == KeymapScheme::Eclipse && app_state.focus == FocusTarget::Editor {
                                app_state.execute_ide_action(IdeAction::DuplicateLine);
                            } else if app_state.focus == FocusTarget::AgentChatInput {
                                app_state.move_agent_input_vertical(-1, shift, &surface_canvas);
                            } else if app_state.focus == FocusTarget::GitCommitInput {
                                app_state.move_git_commit_input_vertical(-1, shift, &surface_canvas);
                            } else if app_state.focus == FocusTarget::RecentProjectsModal {
                                app_state.move_recent_projects_selection(-1);
                            } else if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[A");
                            } else if app_state.focus == FocusTarget::Editor {
                                app_state.active_buffer_mut().move_up(shift);
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::ArrowDown => {
                            if ctrl && alt && !shift && keymap == KeymapScheme::Eclipse && app_state.focus == FocusTarget::Editor {
                                app_state.execute_ide_action(IdeAction::DuplicateLine);
                            } else if app_state.focus == FocusTarget::AgentChatInput {
                                app_state.move_agent_input_vertical(1, shift, &surface_canvas);
                            } else if app_state.focus == FocusTarget::GitCommitInput {
                                app_state.move_git_commit_input_vertical(1, shift, &surface_canvas);
                            } else if app_state.focus == FocusTarget::RecentProjectsModal {
                                app_state.move_recent_projects_selection(1);
                            } else if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[B");
                            } else if app_state.focus == FocusTarget::Editor {
                                app_state.active_buffer_mut().move_down(shift);
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                            Key::Character(_) | Key::Unidentified(_) | Key::Process
                                if !ctrl && !alt && allow_text =>
                            {
                                if let Some(txt) = resolve_keyboard_insert_text(&logical_key, key_text) {
                                    app_state.insert_text_at_focus(&txt);
                                }
                            }
                            _ => {}
                        }
        WindowCommand::None
}

#[cfg(test)]
mod key_ime_tests {
    use super::{is_non_visual_key, resolve_keyboard_insert_text, should_insert_key_text};
    use tao::keyboard::{Key, NativeKeyCode};

    #[test]
    fn macos_deduplicates_received_ime_text_and_ignores_ime_preedit_keys() {
        // On macOS, non-repeat keydown emits ReceivedImeText first, then KeyboardInput.
        assert!(!should_insert_key_text(true, true, true));
        // On macOS IME preedit, KeyboardInput arrives with text == None.
        assert!(!should_insert_key_text(true, false, false));
        // On macOS key repeat, interpretKeyEvents is skipped so KeyboardInput inserts text once.
        assert!(should_insert_key_text(true, false, true));
        // On Windows/Linux, KeyboardInput text insertion stays enabled.
        assert!(should_insert_key_text(false, false, true));
        // Windows IME composition virtual key should not force redundant redraws.
        assert!(is_non_visual_key(&Key::Process));
        assert!(!is_non_visual_key(&Key::Character("a")));
    }

    #[test]
    fn resolves_unicode_packet_and_unidentified_key_text_for_remote_input() {
        // Remote desktop / mobile phone SendInput(KEYEVENTF_UNICODE) produces VK_PACKET -> Key::Unidentified with text.
        assert_eq!(
            resolve_keyboard_insert_text(&Key::Unidentified(NativeKeyCode::Windows(0)), Some("你好世界")),
            Some("你好世界".to_string())
        );
        assert_eq!(
            resolve_keyboard_insert_text(&Key::Unidentified(NativeKeyCode::Windows(0)), Some("hello\x00")),
            Some("hello".to_string())
        );
        assert_eq!(
            resolve_keyboard_insert_text(&Key::Unidentified(NativeKeyCode::Windows(0)), None),
            None
        );
        assert_eq!(
            resolve_keyboard_insert_text(&Key::Character("a"), Some("a")),
            Some("a".to_string())
        );
        assert_eq!(
            resolve_keyboard_insert_text(&Key::Process, Some("a")),
            Some("a".to_string())
        );
        assert_eq!(resolve_keyboard_insert_text(&Key::Process, None), None);
        if cfg!(any(target_os = "windows", target_os = "macos")) {
            assert_eq!(resolve_keyboard_insert_text(&Key::Character("a"), None), None);
        }
    }

    #[test]
    fn mouse_wheel_follows_system_scroll_lines_and_page_scroll() {
        use super::{
            consume_wheel_accum, parse_kde_wheel_scroll_lines, resolve_wheel_scroll_deltas,
            SystemWheelMode,
        };
        use tao::dpi::PhysicalPosition;
        use tao::event::MouseScrollDelta;

        // Windows / macOS native LineDelta: 1 notch at system setting = 1, 3, 6 lines.
        for setting_lines in [1.0f32, 3.0, 6.0] {
            let (dx, dy) = resolve_wheel_scroll_deltas(
                &MouseScrollDelta::LineDelta(0.0, -setting_lines),
                SystemWheelMode::NativeLineDelta,
                25,
                1.0,
                false,
            );
            assert_eq!(dx, 0.0);
            assert_eq!(dy, setting_lines);
            let mut accum = dy;
            assert_eq!(consume_wheel_accum(&mut accum), setting_lines as i32);
            assert_eq!(accum, 0.0);
        }

        // Windows WHEEL_PAGESCROLL: tao falls back to 3 lines/notch; map back to page - 1.
        let (dx_page, dy_page) = resolve_wheel_scroll_deltas(
            &MouseScrollDelta::LineDelta(0.0, -3.0),
            SystemWheelMode::WindowsPageScroll,
            30,
            1.0,
            false,
        );
        assert_eq!(dx_page, 0.0);
        assert_eq!(dy_page, 29.0);

        // High-resolution fractional wheel (4 ticks of 0.25 with 1-line setting) totals 1 line.
        let mut accum = 0.0f32;
        let mut total_lines = 0;
        for _ in 0..4 {
            let (_, dy) = resolve_wheel_scroll_deltas(
                &MouseScrollDelta::LineDelta(0.0, -0.25),
                SystemWheelMode::NativeLineDelta,
                20,
                1.0,
                false,
            );
            accum += dy;
            total_lines += consume_wheel_accum(&mut accum);
        }
        assert_eq!(total_lines, 1);
        assert_eq!(accum, 0.0);

        // Float precision near 1.0 still consumes cleanly without dropping a line.
        let mut near_one = 0.99995f32;
        assert_eq!(consume_wheel_accum(&mut near_one), 1);
        assert_eq!(near_one, 0.0);

        // macOS trackpad PixelDelta converts physical pixels to exact lines without boost.
        let (dx_px, dy_lines) = resolve_wheel_scroll_deltas(
            &MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -44.0)),
            SystemWheelMode::NativeLineDelta,
            20,
            2.0,
            false,
        );
        assert_eq!(dx_px, 0.0);
        assert_eq!(dy_lines, 1.0);

        // Shift + vertical wheel maps to horizontal scroll proportional to system lines.
        let (dx_shift, dy_shift) = resolve_wheel_scroll_deltas(
            &MouseScrollDelta::LineDelta(0.0, -5.0),
            SystemWheelMode::NativeLineDelta,
            20,
            1.0,
            true,
        );
        assert_eq!(dx_shift, 60.0);
        assert_eq!(dy_shift, 0.0);

        // Linux KDE WheelScrollLines parser.
        let kde = "[General]\nName=Test\n[KDE]\nWheelScrollLines=5\n";
        assert_eq!(parse_kde_wheel_scroll_lines(kde), Some(5));
        let (_, dy_linux) = resolve_wheel_scroll_deltas(
            &MouseScrollDelta::LineDelta(0.0, -1.0),
            SystemWheelMode::FixedLinesPerNotch(5),
            20,
            1.0,
            false,
        );
        assert_eq!(dy_linux, 5.0);
    }
}

#[cfg(test)]
mod external_open_tests {
    use crate::external_open::{
        parse_cli_args, path_is_inside_workspace, split_path_and_position, ExternalOpenTarget,
    };
    use std::path::Path;

    #[test]
    fn external_open_parses_file_line_column() {
        let file = std::env::temp_dir().join("rust-ai-ide-open-probe.txt");
        std::fs::write(&file, "probe\n").unwrap();
        let args = vec![
            "ide-app.exe".to_string(),
            file.display().to_string(),
            "12".to_string(),
            "4".to_string(),
        ];
        let parsed = parse_cli_args(&args, None);
        assert_eq!(
            parsed.targets,
            vec![ExternalOpenTarget::OpenFile {
                path: file.clone(),
                line: Some(12),
                column: Some(4),
                cwd: None,
                new_window: false,
            }]
        );

        let combined = format!("{}:8:2", file.display());
        let (path, line, col) = split_path_and_position(&combined);
        assert_eq!(path, file.display().to_string());
        assert_eq!(line, Some(8));
        assert_eq!(col, Some(2));
        assert!(path_is_inside_workspace(
            Path::new(r"D:\work\full\light-ai\frontend\src\App.vue"),
            Path::new(r"D:\work\full\light-ai"),
        ));
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn eclipse_keymap_dispatches_classic_shortcuts() {
        use super::{dispatch_keyboard_pressed, FocusTarget, IdeApplicationState, SurfaceCanvas};
        use ide_core::KeymapScheme;
        use tao::keyboard::{Key, ModifiersState};

        let root = std::env::temp_dir().join(format!(
            "rust_ai_ide_eclipse_dispatch_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let app_file = root.join("demo.ts");
        std::fs::write(&app_file, "const alpha = 1;\nconst beta = 2;\n").unwrap();

        let mut state = IdeApplicationState::new(root.clone());
        state.wakeup_enabled = false;
        state.open_file(app_file);
        state.focus = FocusTarget::Editor;
        state.keymap_scheme = KeymapScheme::Eclipse;
        let mut canvas = SurfaceCanvas::new_with_scale(1280, 720, 1.0);

        // Ctrl+Alt+Down -> DuplicateLine in Eclipse
        state.active_buffer_mut().set_cursor(0, 0, false);
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL | ModifiersState::ALT,
            Key::ArrowDown,
            None,
            false,
            false,
        );
        assert_eq!(state.active_buffer().line_text(1), "const alpha = 1;");

        // Ctrl+D -> DeleteLine in Eclipse
        state.active_buffer_mut().set_cursor(1, 0, false);
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL,
            Key::Character("d"),
            None,
            false,
            false,
        );
        assert_eq!(state.active_buffer().line_text(1), "const beta = 2;");

        // Ctrl+Y -> RedoEdit in Eclipse (after Undo)
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL,
            Key::Character("z"),
            None,
            false,
            false,
        );
        assert_eq!(state.active_buffer().line_text(1), "const alpha = 1;");
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL,
            Key::Character("y"),
            None,
            false,
            false,
        );
        assert_eq!(state.active_buffer().line_text(1), "const beta = 2;");

        // Ctrl+H -> FindInFilesModal in Eclipse
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL,
            Key::Character("h"),
            None,
            false,
            false,
        );
        assert_eq!(state.focus, FocusTarget::FindInFilesModal);
        state.focus = FocusTarget::Editor;

        // Ctrl+L -> GoToLineModal in Eclipse
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL,
            Key::Character("l"),
            None,
            false,
            false,
        );
        assert_eq!(state.focus, FocusTarget::GoToLineModal);
        state.focus = FocusTarget::Editor;

        // Ctrl+3 -> SearchEverywhereModal in Eclipse
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL,
            Key::Character("3"),
            None,
            false,
            false,
        );
        assert_eq!(state.focus, FocusTarget::SearchEverywhereModal);
        state.focus = FocusTarget::Editor;

        // Alt+Shift+R -> RenameSymbolModal in Eclipse when caret is on identifier
        state.active_buffer_mut().set_cursor(0, 7, false);
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::ALT | ModifiersState::SHIFT,
            Key::Character("r"),
            None,
            false,
            false,
        );
        assert_eq!(state.focus, FocusTarget::RenameSymbolModal);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn text_inputs_suppress_double_shift_and_global_shortcuts() {
        use super::{
            dispatch_keyboard_pressed, process_double_shift_modifier_event, FocusTarget,
            IdeApplicationState, SurfaceCanvas,
        };
        use std::time::{Duration, Instant};
        use tao::keyboard::{Key, ModifiersState};

        let root = std::env::temp_dir().join(format!(
            "rust_ai_ide_input_shortcut_guard_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        std::fs::write(&file, "fn main() {\n    let x = 1;\n}\n").unwrap();

        let mut state = IdeApplicationState::new(root.clone());
        state.wakeup_enabled = false;
        state.open_file(file);
        let mut canvas = SurfaceCanvas::new_with_scale(1280, 720, 1.0);
        let t0 = Instant::now();

        // 1. In GitCommitInput and AgentChatInput, Double-Shift must NOT trigger SearchEverywhere.
        for input_focus in [
            FocusTarget::GitCommitInput,
            FocusTarget::AgentChatInput,
            FocusTarget::TerminalInput,
            FocusTarget::EditorFindInput,
        ] {
            state.focus = input_focus;
            let mut last_shift = None;
            assert!(!process_double_shift_modifier_event(
                &mut state,
                ModifiersState::empty(),
                ModifiersState::SHIFT,
                &mut last_shift,
                t0,
            ));
            assert!(!process_double_shift_modifier_event(
                &mut state,
                ModifiersState::empty(),
                ModifiersState::SHIFT,
                &mut last_shift,
                t0 + Duration::from_millis(120),
            ));
            assert_eq!(state.focus, input_focus);
        }

        // 2. While GitCommitInput or AgentChatInput is focused, global/editor shortcuts do not fire,
        //    while input editing/undo/redo still works.
        state.focus = FocusTarget::GitCommitInput;
        state.git_commit_input.set_text("feat: initial");
        let before_editor_text = state.active_buffer().text();

        // Ctrl+D, Ctrl+/, Ctrl+Shift+A, Ctrl+P, Alt+1 must be ignored.
        for (mods, key) in [
            (ModifiersState::CONTROL, Key::Character("d")),
            (ModifiersState::CONTROL, Key::Character("/")),
            (
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                Key::Character("a"),
            ),
            (ModifiersState::CONTROL, Key::Character("p")),
            (ModifiersState::ALT, Key::Character("1")),
            (ModifiersState::ALT, Key::Delete),
        ] {
            dispatch_keyboard_pressed(&mut state, &mut canvas, mods, key, None, false, false);
            assert_eq!(state.focus, FocusTarget::GitCommitInput);
            assert_eq!(state.active_buffer().text(), before_editor_text);
        }

        // Ctrl+A + Backspace + Ctrl+Z + Ctrl+Y operate on GitCommitInput itself.
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL,
            Key::Character("a"),
            None,
            false,
            false,
        );
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::empty(),
            Key::Backspace,
            None,
            false,
            false,
        );
        assert_eq!(state.git_commit_input.as_str(), "");
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL,
            Key::Character("z"),
            None,
            false,
            false,
        );
        assert_eq!(state.git_commit_input.as_str(), "feat: initial");

        // Same for AgentChatInput.
        state.focus = FocusTarget::AgentChatInput;
        dispatch_keyboard_pressed(
            &mut state,
            &mut canvas,
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            Key::Character("f"),
            None,
            false,
            false,
        );
        assert_eq!(state.focus, FocusTarget::AgentChatInput);

        // 3. In Editor, two standalone Shift presses within 400ms DO trigger SearchEverywhere,
        //    whereas Shift presses separated by a character key reset last_shift_press.
        state.focus = FocusTarget::Editor;
        let mut last_shift = None;
        assert!(!process_double_shift_modifier_event(
            &mut state,
            ModifiersState::empty(),
            ModifiersState::SHIFT,
            &mut last_shift,
            t0,
        ));
        assert!(last_shift.is_some());
        assert!(process_double_shift_modifier_event(
            &mut state,
            ModifiersState::empty(),
            ModifiersState::SHIFT,
            &mut last_shift,
            t0 + Duration::from_millis(150),
        ));
        assert_eq!(state.focus, FocusTarget::SearchEverywhereModal);

        let _ = std::fs::remove_dir_all(&root);
    }
}
