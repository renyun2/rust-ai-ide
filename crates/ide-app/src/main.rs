#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent_panel;
mod canvas;
mod ide_state;
mod image_preview;
mod persistence;
mod text_input;

use crate::canvas::{
    detect_system_dpi_scale, render_ri_icon_rgba, save_ri_icon_png, SurfaceCanvas,
};
use crate::ide_state::{
    write_windows_clipboard_text, FocusTarget, IdeApplicationState, IdeCursorKind,
    TextInputNavAction, WindowControlCmd,
};
use anyhow::Result;
use ide_core::{IdeAction, MainMenuCategory, ToolWindowId};
use softbuffer::{Context, Surface};
use std::env;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event::{ElementState, Event, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::keyboard::{Key, ModifiersState};
use tao::window::{CursorIcon, Icon, ResizeDirection, WindowBuilder};

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

#[derive(Debug, Clone, Copy)]
enum IdeWakeupEvent {
    Wakeup,
}

fn sync_ime_position(
    window: &tao::window::Window,
    state: &IdeApplicationState,
    canvas: &SurfaceCanvas,
) {
    let (px, py) = canvas.to_physical_pos(state.ime_spot.0, state.ime_spot.1);
    window.set_ime_position(PhysicalPosition::new(px, py));
}

fn load_ri_window_icon() -> Option<Icon> {
    let rgba = render_ri_icon_rgba(128);
    if rgba.is_empty() {
        return None;
    }
    Icon::from_rgba(rgba, 128, 128).ok()
}

fn sync_window_title(window: &tao::window::Window, state: &mut IdeApplicationState) {
    if state.take_workspace_title_dirty() {
        window.set_title(&format!(
            "RI — {} [{}]",
            state.workspace_name(),
            state.workspace_root.display()
        ));
    }
}

#[cfg(target_os = "windows")]
fn apply_windows11_rounded_corners(window: &tao::window::Window) {
    use tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    let hwnd = HWND(window.hwnd() as *mut core::ffi::c_void);
    let pref = DWMWCP_ROUND.0 as u32;
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn apply_windows11_rounded_corners(_window: &tao::window::Window) {}

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

fn parse_explicit_workspace_arg(args: &[String]) -> Option<PathBuf> {
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--workspace" {
            if let Some(next) = args.get(i + 1) {
                let p = PathBuf::from(next);
                if p.is_dir() {
                    return Some(p);
                }
            }
        }
        i += 1;
    }
    for arg in args.iter().skip(1) {
        if !arg.starts_with('-') {
            let p = PathBuf::from(arg);
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    None
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
        save_ri_icon_png(&out_path, 256)?;
        println!("Exported RI icon PNG: {}", out_path.display());
        return Ok(());
    }
    let storage_dir = persistence::default_storage_dir();
    let saved_global: persistence::PersistedGlobalSettings = storage_dir
        .as_deref()
        .and_then(|d| persistence::load_json(&persistence::global_settings_path(d)))
        .unwrap_or_default();
    let workspace_root = parse_explicit_workspace_arg(&args)
        .or_else(|| saved_global.last_workspace.clone().filter(|p| p.is_dir()))
        .unwrap_or_else(|| parse_workspace_arg(&args));
    let ui_test_mode = args.iter().any(|a| a == "--ui-test");

    if args.iter().any(|a| a == "--self-check") {
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
    let wakeup: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = proxy.send_event(IdeWakeupEvent::Wakeup);
    });

    let (init_log_w, init_log_h) = if saved_global.window_width >= 800
        && saved_global.window_height >= 540
    {
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

    let window = Rc::new(
        WindowBuilder::new()
            .with_title(format!("RI — Rust AI IDE [{}]", workspace_root.display()))
            .with_window_icon(load_ri_window_icon())
            .with_decorations(false)
            .with_resizable(true)
            .with_maximized(saved_global.window_maximized)
            .with_inner_size(LogicalSize::new(init_log_w, init_log_h))
            .build(&event_loop)?,
    );
    apply_windows11_rounded_corners(&window);

    let context = Context::new(window.clone())
        .map_err(|e| anyhow::anyhow!("Failed to initialize softbuffer context: {e}"))?;
    let mut surface = Surface::new(&context, window.clone())
        .map_err(|e| anyhow::anyhow!("Failed to create rendering surface: {e}"))?;

    let mut app_state = IdeApplicationState::new_with_wakeup(workspace_root, Some(wakeup));
    if let Some(dir) = storage_dir {
        app_state.restore_from_persistence_dir(dir);
        sync_window_title(&window, &mut app_state);
    }
    let init_os_scale = (window.scale_factor() as f32).clamp(0.75, 4.0);
    app_state.system_dpi_scale = init_os_scale;
    app_state.window_maximized = window.is_maximized();
    let init_phys = window.inner_size();
    let mut surface_canvas = SurfaceCanvas::new_with_scale(
        init_phys.width.max(1) as usize,
        init_phys.height.max(1) as usize,
        app_state.effective_scale(),
    );
    app_state.adapt_panels_for_logical_width(surface_canvas.width as i32);
    let mut cursor_pos: (i32, i32) = (0, 0);
    let mut last_cursor_icon = CursorIcon::Default;
    let mut modifiers = ModifiersState::default();
    let mut left_mouse_down = false;
    let mut last_shift_press: Option<Instant> = None;
    let mut last_titlebar_click: Option<(Instant, (i32, i32))> = None;
    let mut last_input_click: Option<(Instant, (i32, i32), u8)> = None;
    let mut ui_test_frames: u32 = 0;
    let mut last_pty_rev = app_state.active_terminal_revision();
    let mut pending_redraw = true;
    let mut last_wheel_time: Option<Instant> = None;
    let mut last_wheel_dir: f32 = 0.0;
    let mut wheel_velocity_boost: f32 = 1.0;
    let mut wheel_accum_y: f32 = 0.0;
    let mut wheel_accum_x: f32 = 0.0;

    event_loop.run(move |event, _, control_flow| {
        if ui_test_mode {
            *control_flow = ControlFlow::Poll;
        } else if app_state.has_active_animations() {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16));
        } else if app_state.bottom_tool == Some(ToolWindowId::Terminal) {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(80));
        } else {
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(260));
        }

        match event {
            Event::UserEvent(IdeWakeupEvent::Wakeup) => {
                let _ = app_state.poll_background_updates();
                sync_window_title(&window, &mut app_state);
                pending_redraw = true;
            }
            Event::MainEventsCleared => {
                if ui_test_mode {
                    ui_test_frames += 1;
                    if ui_test_frames == 2 {
                        window.request_redraw();
                    } else if ui_test_frames == 5 {
                        app_state.hamburger_menu_expanded = true;
                        app_state.active_menu_category = Some(MainMenuCategory::View);
                        app_state.trigger_ctrl_k_inline_edit();
                        window.request_redraw();
                    } else if ui_test_frames >= 10 {
                        let screenshot_dir = Path::new(
                            r"C:\Users\renyu\.gemini\antigravity-acp\brain\bdfa34ef-e982-4639-9e67-ad62c315c891",
                        );
                        if screenshot_dir.exists() {
                            let _ = surface_canvas
                                .save_png(&screenshot_dir.join("idea_window_live_test.png"));
                        }
                        println!(
                            "Automated Live Window UI Test passed ({} frames rendered).",
                            ui_test_frames
                        );
                        *control_flow = ControlFlow::Exit;
                    }
                } else {
                    let mut need_redraw = pending_redraw;
                    if app_state.poll_background_updates() {
                        sync_window_title(&window, &mut app_state);
                        need_redraw = true;
                    }
                    if app_state.step_animations() {
                        need_redraw = true;
                    }
                    if app_state.bottom_tool == Some(ToolWindowId::Terminal) {
                        let current_rev = app_state.active_terminal_revision();
                        if current_rev != last_pty_rev {
                            last_pty_rev = current_rev;
                            need_redraw = true;
                        }
                    }
                    if app_state.last_input_activity.elapsed() >= Duration::from_millis(530) {
                        app_state.caret_visible = !app_state.caret_visible;
                        app_state.last_input_activity = Instant::now();
                        need_redraw = true;
                    }
                    if need_redraw {
                        pending_redraw = false;
                        window.request_redraw();
                    }
                }
            }
            Event::RedrawRequested(_) => {
                pending_redraw = false;
                let size = window.inner_size();
                let width = size.width.max(1);
                let height = size.height.max(1);
                app_state.system_dpi_scale = (window.scale_factor() as f32).clamp(0.75, 4.0);
                app_state.window_maximized = window.is_maximized();

                surface_canvas.resize_physical(
                    width as usize,
                    height as usize,
                    app_state.effective_scale(),
                );
                app_state.render(&mut surface_canvas);
                sync_ime_position(&window, &app_state, &surface_canvas);
                sync_window_cursor(
                    &window,
                    &app_state,
                    &surface_canvas,
                    cursor_pos,
                    &mut last_cursor_icon,
                );
                last_pty_rev = app_state.active_terminal_revision();

                if let (Some(nz_w), Some(nz_h)) =
                    (NonZeroU32::new(width), NonZeroU32::new(height))
                {
                    if surface.resize(nz_w, nz_h).is_ok() {
                        if let Ok(mut buffer) = surface.buffer_mut() {
                            surface_canvas.copy_to_softbuffer(&mut buffer);
                            let _ = buffer.present();
                        }
                    }
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    app_state.save_all_persisted_state();
                    *control_flow = ControlFlow::Exit;
                }
                WindowEvent::Resized(_) => {
                    app_state.window_maximized = window.is_maximized();
                    pending_redraw = true;
                }
                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    app_state.system_dpi_scale = (scale_factor as f32).clamp(0.75, 4.0);
                    surface_canvas.set_scale_factor(app_state.effective_scale());
                    pending_redraw = true;
                }
                WindowEvent::ReceivedImeText(text) => {
                    app_state.insert_text_at_focus(&text);
                    sync_ime_position(&window, &app_state, &surface_canvas);
                    pending_redraw = true;
                }
                WindowEvent::HoveredFile(path) => {
                    let pos = cursor_pos_in_window(&window, &surface_canvas, cursor_pos);
                    cursor_pos = pos;
                    app_state.external_drag_hover = Some((path, pos));
                    pending_redraw = true;
                }
                WindowEvent::HoveredFileCancelled => {
                    if app_state.external_drag_hover.take().is_some() {
                        pending_redraw = true;
                    }
                }
                WindowEvent::DroppedFile(path) => {
                    let pos = cursor_pos_in_window(&window, &surface_canvas, cursor_pos);
                    cursor_pos = pos;
                    app_state.handle_os_dropped_file(path, pos.0, pos.1);
                    surface_canvas.set_scale_factor(app_state.effective_scale());
                    sync_ime_position(&window, &app_state, &surface_canvas);
                    pending_redraw = true;
                }
                WindowEvent::ModifiersChanged(new_mods) => {
                    // 检测 IDEA 标志性 "Double Shift" (双击 Shift 呼出 Search Everywhere)
                    if new_mods.shift_key()
                        && !modifiers.shift_key()
                        && !new_mods.control_key()
                        && !new_mods.alt_key()
                    {
                        let now = Instant::now();
                        if let Some(prev) = last_shift_press {
                            if now.duration_since(prev) <= Duration::from_millis(400) {
                                app_state.execute_ide_action(IdeAction::SearchEverywhere);
                                last_shift_press = None;
                                pending_redraw = true;
                            } else {
                                last_shift_press = Some(now);
                            }
                        } else {
                            last_shift_press = Some(now);
                        }
                    }
                    modifiers = new_mods;
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor_pos = surface_canvas.to_logical_pos(position.x, position.y);
                    let win_w = surface_canvas.width as i32;
                    let win_h = surface_canvas.height as i32;

                    let dragged_split =
                        app_state.update_splitter_drag(cursor_pos.0, cursor_pos.1, win_w, win_h);
                    let dragged_scroll = app_state.update_scrollbar_drag(cursor_pos.1);
                    let dragged_ide_file = left_mouse_down
                        && !dragged_split
                        && !dragged_scroll
                        && app_state.update_ide_drag(cursor_pos.0, cursor_pos.1);
                    let dragged_text = left_mouse_down
                        && !dragged_split
                        && !dragged_scroll
                        && !dragged_ide_file
                        && app_state.handle_input_mouse_drag(
                            cursor_pos.0,
                            cursor_pos.1,
                            &surface_canvas,
                        );
                    if dragged_split || dragged_scroll || dragged_ide_file || dragged_text {
                        if dragged_text {
                            sync_ime_position(&window, &app_state, &surface_canvas);
                        }
                        pending_redraw = true;
                    } else if app_state.handle_mouse_move(cursor_pos.0, cursor_pos.1) {
                        pending_redraw = true;
                    }
                    sync_window_cursor(
                        &window,
                        &app_state,
                        &surface_canvas,
                        cursor_pos,
                        &mut last_cursor_icon,
                    );
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => match state {
                    ElementState::Pressed => {
                        left_mouse_down = true;
                        let win_w = surface_canvas.width as i32;
                        let win_h = surface_canvas.height as i32;

                        if !window.is_maximized() {
                            if let Some(dir) =
                                detect_resize_direction(cursor_pos.0, cursor_pos.1, win_w, win_h)
                            {
                                left_mouse_down = false;
                                let _ = window.drag_resize_window(dir);
                                return;
                            }
                        }

                        if app_state.try_begin_splitter_drag(
                            cursor_pos.0,
                            cursor_pos.1,
                            win_w,
                            win_h,
                        ) || app_state.try_begin_scrollbar_drag(cursor_pos.0, cursor_pos.1)
                        {
                            sync_ime_position(&window, &app_state, &surface_canvas);
                            pending_redraw = true;
                            return;
                        }

                        if app_state.is_point_in_toolbar_blank(cursor_pos.0, cursor_pos.1) {
                            let now = Instant::now();
                            let is_double = last_titlebar_click
                                .map(|(prev_t, prev_p)| {
                                    now.duration_since(prev_t) <= Duration::from_millis(350)
                                        && (cursor_pos.0 - prev_p.0).abs() <= 4
                                        && (cursor_pos.1 - prev_p.1).abs() <= 4
                                })
                                .unwrap_or(false);
                            if is_double {
                                let next = !window.is_maximized();
                                window.set_maximized(next);
                                app_state.window_maximized = next;
                                last_titlebar_click = None;
                            } else {
                                last_titlebar_click = Some((now, cursor_pos));
                                left_mouse_down = false;
                                let _ = window.drag_window();
                            }
                            pending_redraw = true;
                            return;
                        }

                        let now = Instant::now();
                        let streak = last_input_click
                            .and_then(|(prev_t, prev_p, prev_s)| {
                                if now.duration_since(prev_t) <= Duration::from_millis(380)
                                    && (cursor_pos.0 - prev_p.0).abs() <= 4
                                    && (cursor_pos.1 - prev_p.1).abs() <= 4
                                {
                                    Some((prev_s + 1).min(3))
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(1);

                        app_state.begin_ide_drag_candidate_at(cursor_pos.0, cursor_pos.1);
                        app_state.handle_mouse_click_with_mods(
                            cursor_pos.0,
                            cursor_pos.1,
                            modifiers.shift_key(),
                            &surface_canvas,
                        );
                        surface_canvas.set_scale_factor(app_state.effective_scale());
                        sync_window_title(&window, &mut app_state);
                        if streak == 2 {
                            let _ = app_state.handle_input_double_click(
                                cursor_pos.0,
                                cursor_pos.1,
                                &surface_canvas,
                            );
                            last_input_click = Some((now, cursor_pos, 2));
                        } else if streak >= 3 {
                            let _ = app_state.handle_input_triple_click(
                                cursor_pos.0,
                                cursor_pos.1,
                                &surface_canvas,
                            );
                            last_input_click = None;
                        } else {
                            last_input_click = Some((now, cursor_pos, 1));
                        }
                        if let Some(cmd) = app_state.take_window_control_cmd() {
                            match cmd {
                                WindowControlCmd::Minimize => {
                                    window.set_minimized(true);
                                }
                                WindowControlCmd::ToggleMaximize => {
                                    let next = !window.is_maximized();
                                    window.set_maximized(next);
                                    app_state.window_maximized = next;
                                }
                                WindowControlCmd::Close => {
                                    app_state.save_all_persisted_state();
                                    *control_flow = ControlFlow::Exit;
                                }
                            }
                        }
                        sync_ime_position(&window, &app_state, &surface_canvas);
                        pending_redraw = true;
                    }
                    ElementState::Released => {
                        left_mouse_down = false;
                        let _ = app_state.finish_ide_drag_at(cursor_pos.0, cursor_pos.1);
                        app_state.end_splitter_drag();
                        app_state.save_global_settings();
                        app_state.end_scrollbar_drag();
                        app_state.end_mouse_text_drag();
                        sync_window_cursor(
                            &window,
                            &app_state,
                            &surface_canvas,
                            cursor_pos,
                            &mut last_cursor_icon,
                        );
                        pending_redraw = true;
                    }
                    _ => {}
                },
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Right,
                    ..
                } => {
                    app_state.open_context_menu_at(cursor_pos.0, cursor_pos.1, &surface_canvas);
                    sync_ime_position(&window, &app_state, &surface_canvas);
                    pending_redraw = true;
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    let (raw_dx, raw_dy) = match delta {
                        MouseScrollDelta::LineDelta(x, y) => (-x, -y),
                        MouseScrollDelta::PixelDelta(p) => {
                            let px_per_line =
                                (22.0 * surface_canvas.scale_factor).max(12.0) as f64;
                            ((-p.x / px_per_line) as f32, (-p.y / px_per_line) as f32)
                        }
                        _ => (0.0, 0.0),
                    };

                    if modifiers.control_key() && !modifiers.alt_key() {
                        if raw_dy < -0.01 {
                            app_state.execute_ide_action(IdeAction::ZoomInUi);
                            surface_canvas.set_scale_factor(app_state.effective_scale());
                            pending_redraw = true;
                        } else if raw_dy > 0.01 {
                            app_state.execute_ide_action(IdeAction::ZoomOutUi);
                            surface_canvas.set_scale_factor(app_state.effective_scale());
                            pending_redraw = true;
                        }
                    } else if raw_dx.abs() > 0.001 || raw_dy.abs() > 0.001 {
                        let now = Instant::now();
                        let dir = if raw_dy.abs() > 0.001 {
                            raw_dy.signum()
                        } else {
                            raw_dx.signum()
                        };
                        if let Some(prev) = last_wheel_time {
                            let dt_ms = now.duration_since(prev).as_millis();
                            if dt_ms <= 65 && (dir * last_wheel_dir) > 0.0 {
                                wheel_velocity_boost = (wheel_velocity_boost + 0.48).min(4.0);
                            } else if dt_ms > 140 || (dir * last_wheel_dir) < 0.0 {
                                wheel_velocity_boost = 1.0;
                                if (dir * last_wheel_dir) < 0.0 {
                                    wheel_accum_y = 0.0;
                                    wheel_accum_x = 0.0;
                                }
                            } else {
                                wheel_velocity_boost = (wheel_velocity_boost * 0.85).max(1.0);
                            }
                        } else {
                            wheel_velocity_boost = 1.0;
                        }
                        last_wheel_time = Some(now);
                        if dir != 0.0 {
                            last_wheel_dir = dir;
                        }

                        if modifiers.shift_key() && raw_dx.abs() < 0.01 {
                            wheel_accum_x += raw_dy * 36.0 * wheel_velocity_boost;
                        } else {
                            wheel_accum_y += raw_dy * 3.5 * wheel_velocity_boost;
                            wheel_accum_x += raw_dx * 36.0 * wheel_velocity_boost;
                        }

                        let mut lines_delta = wheel_accum_y.trunc() as i32;
                        if lines_delta == 0 && raw_dy.abs() >= 0.25 {
                            lines_delta = if raw_dy > 0.0 { 1 } else { -1 };
                            wheel_accum_y = 0.0;
                        } else {
                            wheel_accum_y -= lines_delta as f32;
                        }

                        let horiz_px_delta = wheel_accum_x.trunc() as i32;
                        wheel_accum_x -= horiz_px_delta as f32;

                        if lines_delta != 0 || horiz_px_delta != 0 {
                            app_state.handle_mouse_wheel_2d(
                                cursor_pos.0,
                                cursor_pos.1,
                                lines_delta,
                                horiz_px_delta,
                            );
                            pending_redraw = true;
                        }
                    }
                }
                WindowEvent::KeyboardInput {
                    event:
                        KeyEvent {
                            logical_key,
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } => {
                    let ctrl = modifiers.control_key();
                    let alt = modifiers.alt_key();
                    let shift = modifiers.shift_key();

                    match &logical_key {
                        // 1. IDEA + Cursor 组合快捷键
                        Key::Character(ch) if ctrl && alt && ch.eq_ignore_ascii_case("s") => {
                            app_state.execute_ide_action(IdeAction::OpenSettingsModal);
                        }
                        Key::Character(ch) if ctrl && alt && ch.eq_ignore_ascii_case("u") => {
                            app_state.execute_ide_action(IdeAction::SwitchUiLanguage);
                        }
                        Key::Character(ch) if ctrl && alt && ch.eq_ignore_ascii_case("l") => {
                            app_state.execute_ide_action(IdeAction::ReformatCode);
                        }
                        Key::Character(ch) if ctrl && alt && ch.eq_ignore_ascii_case("z") => {
                            app_state.execute_ide_action(IdeAction::RevertLastCheckpoint);
                        }
                        Key::Character(ch) if ctrl && alt && ch.eq_ignore_ascii_case("n") => {
                            app_state.execute_ide_action(IdeAction::NewFolderInProject);
                        }
                        Key::Character(ch) if ctrl && shift && ch.eq_ignore_ascii_case("a") => {
                            app_state.execute_ide_action(IdeAction::FindAction);
                        }
                        Key::Character(ch) if ctrl && shift && ch.eq_ignore_ascii_case("n") => {
                            app_state.execute_ide_action(IdeAction::GoToFile);
                        }
                        Key::Character(ch) if ctrl && shift && ch.eq_ignore_ascii_case("f") => {
                            app_state.execute_ide_action(IdeAction::FindInFiles);
                        }
                        Key::Character(ch) if ctrl && shift && ch.eq_ignore_ascii_case("k") => {
                            app_state.execute_ide_action(IdeAction::GitPush);
                        }
                        Key::Character(ch) if ctrl && shift && ch.eq_ignore_ascii_case("z") => {
                            app_state.execute_ide_action(IdeAction::RedoEdit);
                        }
                        Key::Character(ch) if ctrl && !alt && !shift && ch.eq_ignore_ascii_case("o") => {
                            app_state.execute_ide_action(IdeAction::OpenFolder);
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
                            } else if app_state.focus == FocusTarget::TerminalInput {
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
                            }
                        }
                        Key::Character(ch) if ctrl && ch.eq_ignore_ascii_case("k") => {
                            app_state.execute_ide_action(IdeAction::InlineAiEdit);
                        }
                        Key::Character(ch) if ctrl && ch.eq_ignore_ascii_case("p") => {
                            app_state.execute_ide_action(IdeAction::SearchEverywhere);
                        }
                        Key::Character(ch) if ctrl && ch.eq_ignore_ascii_case("s") => {
                            app_state.execute_ide_action(IdeAction::SaveActiveFile);
                        }
                        Key::Character(ch) if ctrl && ch.eq_ignore_ascii_case("d") => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x04");
                            } else {
                                app_state.execute_ide_action(IdeAction::DuplicateLine);
                            }
                        }
                        Key::Character(ch) if ctrl && ch.eq_ignore_ascii_case("y") => {
                            if app_state.agent_hub.inline_edit.is_some() {
                                app_state.accept_inline_diff();
                            } else {
                                app_state.execute_ide_action(IdeAction::DeleteLine);
                            }
                        }
                        Key::Character(ch) if ctrl && ch.eq_ignore_ascii_case("b") => {
                            app_state.execute_ide_action(IdeAction::GoToDeclaration);
                        }
                        Key::Character(ch) if ctrl && ch.eq_ignore_ascii_case("t") => {
                            app_state.execute_ide_action(IdeAction::GitPull);
                        }
                        Key::Character(ch) if ctrl && ch.eq_ignore_ascii_case("n") => {
                            app_state.execute_ide_action(IdeAction::NewFileInProject);
                        }
                        Key::Character(ch) if ctrl && *ch == "/" => {
                            app_state.execute_ide_action(IdeAction::ToggleLineComment);
                        }
                        Key::Character(ch) if ctrl && *ch == "`" => {
                            app_state.execute_ide_action(IdeAction::SwitchTheme);
                        }
                        Key::Character(ch) if ctrl && !alt && (*ch == "=" || *ch == "+") => {
                            app_state.execute_ide_action(IdeAction::ZoomInUi);
                            surface_canvas.set_scale_factor(app_state.effective_scale());
                        }
                        Key::Character(ch) if ctrl && !alt && *ch == "-" => {
                            app_state.execute_ide_action(IdeAction::ZoomOutUi);
                            surface_canvas.set_scale_factor(app_state.effective_scale());
                        }
                        Key::Character(ch) if ctrl && !alt && *ch == "0" => {
                            app_state.execute_ide_action(IdeAction::ResetUiZoom);
                            surface_canvas.set_scale_factor(app_state.effective_scale());
                        }
                        Key::Character(ch) if ctrl && ch.eq_ignore_ascii_case("z") => {
                            app_state.execute_ide_action(IdeAction::UndoEdit);
                        }
                        // Alt+0..9 IDEA Tool Window 快捷键
                        Key::Character(ch) if alt && *ch == "1" => {
                            app_state.execute_ide_action(IdeAction::ToggleProjectWindow);
                        }
                        Key::Character(ch) if alt && *ch == "0" => {
                            app_state.execute_ide_action(IdeAction::ToggleCommitWindow);
                        }
                        Key::Character(ch) if alt && *ch == "7" => {
                            app_state.execute_ide_action(IdeAction::ToggleStructureWindow);
                        }
                        Key::Character(ch) if alt && *ch == "8" => {
                            app_state.execute_ide_action(IdeAction::ToggleAgentHubWindow);
                        }
                        Key::Character(ch) if alt && ch.eq_ignore_ascii_case("l") => {
                            app_state.execute_ide_action(IdeAction::SendSelectionToAgent);
                        }
                        // 功能键 F4..F12
                        Key::F4 if ctrl => {
                            app_state.execute_ide_action(IdeAction::CloseActiveTab);
                        }
                        Key::F8 if ctrl => {
                            app_state.execute_ide_action(IdeAction::ToggleBreakpoint);
                        }
                        Key::F9 if shift => {
                            app_state.execute_ide_action(IdeAction::DebugActiveConfig);
                        }
                        Key::F10 if shift => {
                            app_state.execute_ide_action(IdeAction::RunActiveConfig);
                        }
                        Key::F12 if alt => {
                            app_state.execute_ide_action(IdeAction::ToggleTerminalWindow);
                        }
                        Key::Insert if alt => {
                            app_state.execute_ide_action(IdeAction::NewFileInProject);
                        }
                        Key::Delete => {
                            if alt {
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
                            } else if app_state.agent_hub.inline_edit.is_some() {
                                app_state.accept_inline_diff();
                            } else if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\t");
                            } else if app_state.focus == FocusTarget::Editor {
                                if shift {
                                    app_state.active_buffer_mut().unindent_selection_or_line();
                                } else {
                                    app_state.active_buffer_mut().indent_selection_or_line();
                                }
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
                                || app_state.project_popup_open
                                || app_state.more_menu_popup_open;
                            app_state.active_menu_category = None;
                            app_state.git_branch_popup_open = false;
                            app_state.agent_provider_popup_open = false;
                            app_state.agent_model_popup_open = false;
                            app_state.agent_mode_popup_open = false;
                            app_state.agent_sessions_popup_open = false;
                            app_state.run_config_popup_open = false;
                            app_state.project_popup_open = false;
                            app_state.more_menu_popup_open = false;
                            if had_menu || had_popup {
                                // 首次 Esc 优先关闭上下文菜单或弹出下拉框
                            } else if app_state.focus == FocusTarget::ImagePreviewModal {
                                app_state.close_image_preview();
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
                        Key::Space if !ctrl && !alt => {
                            app_state.insert_text_at_focus(" ");
                        }
                        Key::Enter => match app_state.focus {
                            FocusTarget::SearchEverywhereModal => {
                                if let Some(ide_search::SearchEverywhereItem::File {
                                    full_path,
                                    ..
                                }) = app_state.search_results.first().cloned()
                                {
                                    app_state.open_file(full_path);
                                    app_state.focus = FocusTarget::Editor;
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
                                let pty = app_state.terminal.active_pty_mut();
                                pty.clear_selection();
                                pty.scroll_offset = 0;
                                pty.send_raw_bytes(b"\r");
                            }
                            FocusTarget::GitCommitInput => {
                                app_state.submit_git_commit(false);
                            }
                            FocusTarget::GitLogSearchInput => {
                                app_state.git_log_scroll = 0;
                            }
                            FocusTarget::AiConfigModal(_) => {
                                app_state.save_ai_config_modal();
                            }
                            FocusTarget::SettingsModal => {
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::OpenProjectChoiceModal => {
                                if let Some(p) = app_state.pending_open_workspace.take() {
                                    if shift {
                                        app_state.switch_workspace(p);
                                        sync_window_title(&window, &mut app_state);
                                    } else {
                                        app_state.open_workspace_in_new_window(p);
                                    }
                                }
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::EditMessageRewindModal => {
                                app_state.confirm_edit_message_rewind(!shift);
                            }
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
                            } else if app_state.focus == FocusTarget::Editor {
                                let vis = app_state.editor_visible_lines.max(4) as isize;
                                app_state.active_buffer_mut().move_page(-vis, shift);
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::PageDown => {
                            if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[6~");
                            } else if app_state.focus == FocusTarget::Editor {
                                let vis = app_state.editor_visible_lines.max(4) as isize;
                                app_state.active_buffer_mut().move_page(vis, shift);
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::ArrowUp => {
                            if app_state.focus == FocusTarget::AgentChatInput {
                                app_state.move_agent_input_vertical(-1, shift, &surface_canvas);
                            } else if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[A");
                            } else if app_state.focus == FocusTarget::Editor {
                                app_state.active_buffer_mut().move_up(shift);
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::ArrowDown => {
                            if app_state.focus == FocusTarget::AgentChatInput {
                                app_state.move_agent_input_vertical(1, shift, &surface_canvas);
                            } else if app_state.focus == FocusTarget::TerminalInput {
                                app_state.terminal.active_pty_mut().send_raw_bytes(b"\x1b[B");
                            } else if app_state.focus == FocusTarget::Editor {
                                app_state.active_buffer_mut().move_down(shift);
                                app_state.ensure_editor_cursor_visible();
                            }
                        }
                        Key::Character(ch) if !ctrl && !alt => {
                            app_state.insert_text_at_focus(ch);
                        }
                        _ => {}
                    }
                    app_state.touch_caret_activity();
                    surface_canvas.set_scale_factor(app_state.effective_scale());
                    sync_ime_position(&window, &app_state, &surface_canvas);
                    pending_redraw = true;
                }
                _ => {}
            },
            _ => {}
        }
    });
}
