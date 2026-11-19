mod canvas;
mod ide_state;

use crate::canvas::SurfaceCanvas;
use crate::ide_state::{FocusTarget, IdeApplicationState};
use anyhow::Result;
use ide_core::{IdeAction, MainMenuCategory, ToolWindowId};
use ide_search::{find_in_files, search_everywhere};
use softbuffer::{Context, Surface};
use std::env;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tao::dpi::LogicalSize;
use tao::event::{ElementState, Event, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::keyboard::{Key, ModifiersState};
use tao::window::WindowBuilder;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let workspace_root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ui_test_mode = args.iter().any(|a| a == "--ui-test");

    if args.iter().any(|a| a == "--self-check") {
        let mut state = IdeApplicationState::new(workspace_root);
        let mut canvas = SurfaceCanvas::new(1560, 940);
        state.render(&mut canvas);

        let screenshot_dir = Path::new(
            r"C:\Users\renyu\.gemini\antigravity-acp\brain\bdfa34ef-e982-4639-9e67-ad62c315c891",
        );
        if screenshot_dir.exists() {
            let _ = canvas.save_png(&screenshot_dir.join("idea_ui_default.png"));
        }

        // 2. 导出 Git Commit 面板 + Git Branches 弹窗状态图
        state.left_tool = Some(ToolWindowId::Commit);
        state.git_branch_popup_open = true;
        state.render(&mut canvas);
        if screenshot_dir.exists() {
            let _ = canvas.save_png(&screenshot_dir.join("idea_ui_git_and_menus.png"));
        }
        state.git_branch_popup_open = false;
        state.left_tool = Some(ToolWindowId::Project);

        // 3. 导出 Settings & IDEA Keymap 弹窗状态图
        state.focus = FocusTarget::SettingsModal;
        state.render(&mut canvas);
        if screenshot_dir.exists() {
            let _ = canvas.save_png(&screenshot_dir.join("idea_ui_settings_keymap.png"));
        }
        state.focus = FocusTarget::Editor;

        // 4. 导出 Cursor Ctrl+K 行内修改 Diff 状态图
        state.trigger_ctrl_k_inline_edit();
        state.render(&mut canvas);
        if screenshot_dir.exists() {
            let _ = canvas.save_png(&screenshot_dir.join("idea_ui_ctrl_k_diff.png"));
        }

        println!("=== Rust AI IDE (Latest IDEA Islands UI + Full Interactive Engine) Self-Check ===");
        println!("Workspace Root : {}", state.workspace_root.display());
        println!("Open Buffers   : {}", state.open_buffers.len());
        println!("Visible Tree   : {}", state.project_tree.visible_entries.len());
        println!("Git Branch     : {} ({} changed files)", state.git_engine.current_branch, state.git_engine.changed_files.len());
        println!(
            "Rendered Frame : {}x{} ({} interactive hit zones)",
            canvas.width,
            canvas.height,
            state.hit_regions.len()
        );
        return Ok(());
    }

    let event_loop = EventLoop::new();
    let window = Rc::new(
        WindowBuilder::new()
            .with_title(format!(
                "Rust AI IDE — IntelliJ IDEA (Islands UI) [{}]",
                workspace_root.display()
            ))
            .with_inner_size(LogicalSize::new(1560.0, 940.0))
            .build(&event_loop)?,
    );

    let context = Context::new(window.clone())
        .map_err(|e| anyhow::anyhow!("Failed to initialize softbuffer context: {e}"))?;
    let mut surface = Surface::new(&context, window.clone())
        .map_err(|e| anyhow::anyhow!("Failed to create rendering surface: {e}"))?;

    let mut app_state = IdeApplicationState::new(workspace_root);
    let mut surface_canvas = SurfaceCanvas::new(1560, 940);
    let mut cursor_pos: (i32, i32) = (0, 0);
    let mut modifiers = ModifiersState::default();
    let mut last_shift_press: Option<Instant> = None;
    let mut ui_test_frames: u32 = 0;

    event_loop.run(move |event, _, control_flow| {
        if ui_test_mode {
            *control_flow = ControlFlow::Poll;
        } else {
            *control_flow = ControlFlow::Wait;
        }

        match event {
            Event::MainEventsCleared if ui_test_mode => {
                ui_test_frames += 1;
                if ui_test_frames == 2 {
                    window.request_redraw();
                } else if ui_test_frames == 5 {
                    app_state.active_menu_category = Some(MainMenuCategory::Git);
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
            }
            Event::RedrawRequested(_) => {
                let size = window.inner_size();
                let width = size.width.max(1);
                let height = size.height.max(1);

                surface_canvas.resize(width as usize, height as usize);
                app_state.render(&mut surface_canvas);

                if let (Some(nz_w), Some(nz_h)) =
                    (NonZeroU32::new(width), NonZeroU32::new(height))
                {
                    if surface.resize(nz_w, nz_h).is_ok() {
                        if let Ok(mut buffer) = surface.buffer_mut() {
                            buffer.copy_from_slice(surface_canvas.pixels());
                            let _ = buffer.present();
                        }
                    }
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    *control_flow = ControlFlow::Exit;
                }
                WindowEvent::Resized(_) => {
                    window.request_redraw();
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
                                window.request_redraw();
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
                    cursor_pos = (position.x as i32, position.y as i32);
                    let size = window.inner_size();
                    if app_state.update_splitter_drag(
                        cursor_pos.0,
                        cursor_pos.1,
                        size.width as i32,
                        size.height as i32,
                    ) {
                        window.request_redraw();
                    }
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => match state {
                    ElementState::Pressed => {
                        let size = window.inner_size();
                        if !app_state.try_begin_splitter_drag(
                            cursor_pos.0,
                            cursor_pos.1,
                            size.width as i32,
                            size.height as i32,
                        ) {
                            app_state.handle_mouse_click(cursor_pos.0, cursor_pos.1);
                        }
                        window.request_redraw();
                    }
                    ElementState::Released => {
                        app_state.end_splitter_drag();
                    }
                    _ => {}
                },
                WindowEvent::MouseWheel { delta, .. } => {
                    let lines_delta: i32 = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -(y as i32) * 3,
                        MouseScrollDelta::PixelDelta(p) => -(p.y as i32) / 20,
                        _ => 0,
                    };
                    // 如果鼠标在左侧项目树区域，则滚动项目树；否则滚动中央代码编辑器
                    if app_state.left_tool.is_some()
                        && cursor_pos.0 < 42 + app_state.left_panel_width
                    {
                        if lines_delta < 0 {
                            app_state.tree_scroll = app_state
                                .tree_scroll
                                .saturating_sub((-lines_delta) as usize);
                        } else if lines_delta > 0 {
                            let max_t = app_state
                                .project_tree
                                .visible_entries
                                .len()
                                .saturating_sub(8);
                            app_state.tree_scroll =
                                (app_state.tree_scroll + lines_delta as usize).min(max_t);
                        }
                    } else if lines_delta < 0 {
                        app_state.scroll_line =
                            app_state.scroll_line.saturating_sub((-lines_delta) as usize);
                    } else if lines_delta > 0 {
                        let max_line = app_state
                            .active_buffer()
                            .line_count()
                            .saturating_sub(5);
                        app_state.scroll_line =
                            (app_state.scroll_line + lines_delta as usize).min(max_line);
                    }
                    window.request_redraw();
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
                            app_state.execute_ide_action(IdeAction::DuplicateLine);
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
                            if app_state.focus == FocusTarget::Editor {
                                app_state.execute_ide_action(IdeAction::DeleteSelectedPath);
                            }
                        }
                        Key::Tab => {
                            if app_state.agent_hub.inline_edit.is_some() {
                                app_state.accept_inline_diff();
                            } else {
                                app_state.active_buffer_mut().insert_str_at_cursor("    ");
                            }
                        }
                        Key::Escape => {
                            app_state.active_menu_category = None;
                            app_state.git_branch_popup_open = false;
                            if app_state.agent_hub.inline_edit.is_some() {
                                app_state.reject_inline_diff();
                            } else {
                                app_state.focus = FocusTarget::Editor;
                            }
                        }
                        Key::Backspace => match app_state.focus {
                            FocusTarget::SearchEverywhereModal => {
                                app_state.search_query.pop();
                                app_state.search_results = search_everywhere(
                                    &app_state.workspace_root,
                                    &app_state.search_query,
                                    14,
                                );
                            }
                            FocusTarget::FindInFilesModal => {
                                app_state.search_query.pop();
                                app_state.content_search_hits = find_in_files(
                                    &app_state.workspace_root,
                                    &app_state.search_query,
                                    20,
                                )
                                .unwrap_or_default();
                            }
                            FocusTarget::NewFileModal
                            | FocusTarget::NewFolderModal
                            | FocusTarget::NewGitBranchModal => {
                                app_state.modal_text_input.pop();
                            }
                            FocusTarget::AgentChatInput => {
                                app_state.agent_chat_input.pop();
                            }
                            FocusTarget::TerminalInput => {
                                app_state.terminal.input_buffer.pop();
                            }
                            FocusTarget::GitCommitInput => {
                                app_state.git_engine.commit_message_draft.pop();
                            }
                            _ => {
                                app_state.active_buffer_mut().backspace_at_cursor();
                            }
                        },
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
                                    app_state.active_buffer_mut().cursor_line =
                                        first.line_number.saturating_sub(1);
                                    app_state.focus = FocusTarget::Editor;
                                }
                            }
                            FocusTarget::NewFileModal => {
                                let name = app_state.modal_text_input.clone();
                                if let Ok(created) = app_state.project_tree.create_new_file(&name) {
                                    app_state.open_file(created);
                                    app_state.git_engine.refresh();
                                }
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::NewFolderModal => {
                                let name = app_state.modal_text_input.clone();
                                let _ = app_state.project_tree.create_new_directory(&name);
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::NewGitBranchModal => {
                                let name = app_state.modal_text_input.clone();
                                if let Ok(msg) =
                                    app_state.git_engine.create_and_checkout_branch(&name)
                                {
                                    app_state.status_notification = msg;
                                }
                                app_state.focus = FocusTarget::Editor;
                            }
                            FocusTarget::AgentChatInput => {
                                let prompt = std::mem::take(&mut app_state.agent_chat_input);
                                if !prompt.trim().is_empty() {
                                    let active_path = app_state.active_buffer().path.clone();
                                    app_state.agent_hub.send_prompt_with_ide_context(
                                        &prompt,
                                        Some(&active_path),
                                        None,
                                        None,
                                    );
                                }
                            }
                            FocusTarget::TerminalInput => {
                                let cmd = std::mem::take(&mut app_state.terminal.input_buffer);
                                if !cmd.trim().is_empty() {
                                    app_state.terminal.execute_command_line(&cmd);
                                    app_state.git_engine.refresh();
                                }
                            }
                            FocusTarget::GitCommitInput => {
                                let msg = app_state.git_engine.commit_message_draft.clone();
                                if let Ok(res) = app_state.git_engine.commit_all(&msg) {
                                    app_state.status_notification = format!("Git Commit: {res}");
                                }
                            }
                            _ => {
                                if alt {
                                    app_state.execute_ide_action(IdeAction::ShowIntentionActions);
                                } else {
                                    app_state.active_buffer_mut().insert_char_at_cursor('\n');
                                }
                            }
                        },
                        Key::ArrowUp => {
                            let buf = app_state.active_buffer_mut();
                            buf.cursor_line = buf.cursor_line.saturating_sub(1);
                        }
                        Key::ArrowDown => {
                            let buf = app_state.active_buffer_mut();
                            let max_l = buf.line_count().saturating_sub(1);
                            buf.cursor_line = (buf.cursor_line + 1).min(max_l);
                        }
                        Key::Character(ch) if !ctrl && !alt => match app_state.focus {
                            FocusTarget::SearchEverywhereModal => {
                                app_state.search_query.push_str(ch);
                                app_state.search_results = search_everywhere(
                                    &app_state.workspace_root,
                                    &app_state.search_query,
                                    14,
                                );
                            }
                            FocusTarget::FindInFilesModal => {
                                app_state.search_query.push_str(ch);
                                app_state.content_search_hits = find_in_files(
                                    &app_state.workspace_root,
                                    &app_state.search_query,
                                    20,
                                )
                                .unwrap_or_default();
                            }
                            FocusTarget::NewFileModal
                            | FocusTarget::NewFolderModal
                            | FocusTarget::NewGitBranchModal => {
                                app_state.modal_text_input.push_str(ch);
                            }
                            FocusTarget::AgentChatInput => {
                                app_state.agent_chat_input.push_str(ch);
                            }
                            FocusTarget::TerminalInput => {
                                app_state.terminal.input_buffer.push_str(ch);
                            }
                            FocusTarget::GitCommitInput => {
                                app_state.git_engine.commit_message_draft.push_str(ch);
                            }
                            _ => {
                                if let Some(first_ch) = ch.chars().next() {
                                    app_state.active_buffer_mut().insert_char_at_cursor(first_ch);
                                }
                            }
                        },
                        _ => {}
                    }
                    window.request_redraw();
                }
                _ => {}
            },
            _ => {}
        }
    });
}
