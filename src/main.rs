use eframe::egui;
use parking_lot::RwLock;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::Arc;
use std::thread;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// ---------------------------------------------------------------------------
//  Raw FFI for ShellExecuteW (avoids windows crate type issues)
// ---------------------------------------------------------------------------

unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: HWND,
        lpOperation: PCWSTR,
        lpFile: PCWSTR,
        lpParameters: PCWSTR,
        lpDirectory: PCWSTR,
        nShowCmd: i32,
    ) -> isize;
}

// ---------------------------------------------------------------------------
//  Rule engine
// ---------------------------------------------------------------------------

const M_CTRL: u16 = 1;
const M_ALT: u16 = 2;
const M_SHIFT: u16 = 4;
const M_WIN: u16 = 8;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
enum RuleAction {
    Keys(String),
    Text(String),
    Run(String),
    Url(String),
    Toggle,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Rule {
    vk: u16,
    mods: u16,
    action: RuleAction,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct AppState {
    rules: Vec<Rule>,
    enabled: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            enabled: true,
        }
    }
}

static VK_NAMES: &[(&str, u16)] = &[
    ("Backspace", 0x08), ("Tab", 0x09), ("Enter", 0x0D), ("Esc", 0x1B), ("Space", 0x20),
    ("PgUp", 0x21), ("PgDn", 0x22), ("End", 0x23), ("Home", 0x24),
    ("Left", 0x25), ("Up", 0x26), ("Right", 0x27), ("Down", 0x28),
    ("Insert", 0x2D), ("Delete", 0x2E),
    ("CapsLock", 0x14), ("NumLock", 0x90), ("ScrollLock", 0x91),
    ("PrintScreen", 0x2C), ("PrtScn", 0x2C), ("Pause", 0x13),
    ("Plus", 0xBB), ("Equal", 0xBB), ("Minus", 0xBD),
    ("Comma", 0xBC), ("Period", 0xBE), ("Dot", 0xBE),
    ("Slash", 0xBF), ("Backslash", 0x5C), ("Semicolon", 0xBA),
    ("Quote", 0xDE), ("Backquote", 0xC0), ("Grave", 0xC0),
    ("LBracket", 0xDB), ("RBracket", 0xDD),
];

fn parse_combo(s: &str) -> Option<(u16, u16)> {
    let s = s.trim();
    if s.is_empty() { return None; }
    let mut vk: u16 = 0;
    let mut mods: u16 = 0;
    for part in s.split('+') {
        let t = part.trim().to_lowercase();
        if t.is_empty() { return None; }
        match t.as_str() {
            "ctrl" | "control" => mods |= M_CTRL,
            "alt" => mods |= M_ALT,
            "shift" => mods |= M_SHIFT,
            "win" | "super" | "meta" => mods |= M_WIN,
            _ => {
                let v = name_to_vk(&t)?;
                if is_modifier_vk(v) { return None; }
                vk = v;
            }
        }
    }
    if vk == 0 { return None; }
    Some((vk, mods))
}

fn name_to_vk(s: &str) -> Option<u16> {
    let s = s.trim();
    if s.len() == 1 {
        let c = s.chars().next()?;
        if c >= 'a' && c <= 'z' { return Some(c as u16 & 0x5F); }
        if c >= '0' && c <= '9' { return Some(c as u16); }
        unsafe {
            let mut buf = [0u16; 1];
            c.encode_utf16(&mut buf);
            let v = VkKeyScanW(buf[0]);
            if v != -1 { return Some((v & 0xFF) as u16); }
        }
        return None;
    }
    if s.starts_with('f') && s.len() > 1 {
        let num: u32 = s[1..].parse().ok()?;
        if num >= 1 && num <= 24 { return Some((0x70u32 + num - 1) as u16); }
    }
    for (name, vk) in VK_NAMES {
        if name.eq_ignore_ascii_case(s) { return Some(*vk); }
    }
    None
}

fn vk_to_name(vk: u16) -> String {
    if (b'A' as u16..=b'Z' as u16).contains(&vk) { return char::from_u32(vk as u32).unwrap_or('?').to_string(); }
    if (b'0' as u16..=b'9' as u16).contains(&vk) { return char::from_u32(vk as u32).unwrap_or('?').to_string(); }
    if (0x70..=0x87).contains(&vk) { return format!("F{}", vk - 0x70 + 1); }
    for (name, vk_val) in VK_NAMES {
        if *vk_val == vk { return name.to_string(); }
    }
    format!("0x{:02X}", vk)
}

fn combo_to_string(vk: u16, mods: u16) -> String {
    let mut s = String::new();
    if mods & M_CTRL != 0 { s.push_str("Ctrl+"); }
    if mods & M_ALT != 0 { s.push_str("Alt+"); }
    if mods & M_SHIFT != 0 { s.push_str("Shift+"); }
    if mods & M_WIN != 0 { s.push_str("Win+"); }
    s.push_str(&vk_to_name(vk));
    s
}

fn is_modifier_vk(vk: u16) -> bool {
    matches!(vk, 0x11 | 0x12 | 0x10 | 0x5B | 0x5C | 0xA2 | 0xA3 | 0xA4 | 0xA5 | 0xA0 | 0xA1)
}

fn current_mods() -> u16 {
    unsafe {
        let mut m = 0;
        if (GetAsyncKeyState(0x11) as u16) & 0x8000u16 != 0 { m |= M_CTRL; }
        if (GetAsyncKeyState(0x12) as u16) & 0x8000u16 != 0 { m |= M_ALT; }
        if (GetAsyncKeyState(0x10) as u16) & 0x8000u16 != 0 { m |= M_SHIFT; }
        if ((GetAsyncKeyState(0x5B) as u16) | (GetAsyncKeyState(0x5C) as u16)) & 0x8000u16 != 0 { m |= M_WIN; }
        m
    }
}

fn send_input(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn send_chord(combo: &str) {
    if let Some((vk, mods)) = parse_combo(combo) {
        let mut inputs: Vec<INPUT> = Vec::new();
        let mut key = |v: u16, up: bool| {
            let mut ki = KEYBDINPUT {
                wVk: VIRTUAL_KEY(v),
                wScan: 0,
                dwFlags: if up { KEYBD_EVENT_FLAGS(0x0002) } else { KEYBD_EVENT_FLAGS(0) },
                dwExtraInfo: 0,
                time: 0,
            };
            let mut i = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 { ki },
            };
            inputs.push(i);
        };
        if mods & M_CTRL != 0 { key(0x11, false); }
        if mods & M_ALT != 0 { key(0x12, false); }
        if mods & M_SHIFT != 0 { key(0x10, false); }
        if mods & M_WIN != 0 { key(0x5B, false); }
        key(vk, false);
        key(vk, true);
        if mods & M_WIN != 0 { key(0x5B, true); }
        if mods & M_SHIFT != 0 { key(0x10, true); }
        if mods & M_ALT != 0 { key(0x12, true); }
        if mods & M_CTRL != 0 { key(0x11, true); }
        send_input(&inputs);
    }
}

fn send_text(text: &str) {
    let mut inputs: Vec<INPUT> = Vec::new();
    for c in text.chars() {
        match c {
            '\n' | '\r' => {
                let mut ki = KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0x0D),
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    dwExtraInfo: 0,
                    time: 0,
                };
                let mut d = INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } };
                let mut u = d.clone();
                u.Anonymous.ki.dwFlags = KEYBD_EVENT_FLAGS(0x0002);
                inputs.push(d); inputs.push(u);
            }
            '\t' => {
                let mut ki = KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0x09),
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    dwExtraInfo: 0,
                    time: 0,
                };
                let mut d = INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } };
                let mut u = d.clone();
                u.Anonymous.ki.dwFlags = KEYBD_EVENT_FLAGS(0x0002);
                inputs.push(d); inputs.push(u);
            }
            _ => {
                let mut ki = KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: c as u16,
                    dwFlags: KEYBD_EVENT_FLAGS(0x0004),
                    dwExtraInfo: 0,
                    time: 0,
                };
                let mut d = INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } };
                let mut u = d.clone();
                u.Anonymous.ki.dwFlags = KEYBD_EVENT_FLAGS(0x0004 | 0x0002);
                inputs.push(d); inputs.push(u);
            }
        }
    }
    if !inputs.is_empty() {
        send_input(&inputs);
    }
}

fn execute_action(action: &RuleAction) {
    match action {
        RuleAction::Keys(combo) => send_chord(combo),
        RuleAction::Text(text) => send_text(text),
        RuleAction::Run(prog) => {
            let wide: Vec<u16> = OsStr::new(prog).encode_wide().collect();
            unsafe {
                ShellExecuteW(
                    HWND(std::ptr::null_mut()),
                    PCWSTR::null(),
                    PCWSTR::from_raw(wide.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    1,
                );
            }
        }
        RuleAction::Url(url) => {
            let wide: Vec<u16> = OsStr::new(url).encode_wide().collect();
            unsafe {
                ShellExecuteW(
                    HWND(std::ptr::null_mut()),
                    PCWSTR::null(),
                    PCWSTR::from_raw(wide.as_ptr()),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    1,
                );
            }
        }
        RuleAction::Toggle => {}
    }
}

// ---------------------------------------------------------------------------
//  Catalog
// ---------------------------------------------------------------------------

static CATALOG: &[(&str, &str, &str)] = &[
    ("Windows", "Open File Explorer", "Win+E"),
    ("Windows", "Run dialog", "Win+R"),
    ("Windows", "Settings", "Win+I"),
    ("Windows", "Show desktop", "Win+D"),
    ("Windows", "Task View", "Win+Tab"),
    ("Windows", "Action Center", "Win+A"),
    ("Windows", "Search", "Win+S"),
    ("Windows", "Clipboard history", "Win+V"),
    ("Windows", "Screen snip", "Win+Shift+S"),
    ("Windows", "Project / display", "Win+P"),
    ("Windows", "Quick link menu", "Win+X"),
    ("Windows", "Emoji picker", "Win+Period"),
    ("Windows", "New virtual desktop", "Win+Ctrl+D"),
    ("Windows", "Next virtual desktop", "Win+Ctrl+Right"),
    ("Windows", "Previous virtual desktop", "Win+Ctrl+Left"),
    ("Windows", "Task Manager", "Ctrl+Shift+Esc"),
    ("Windows", "Close window", "Alt+F4"),
    ("Windows", "Properties", "Alt+Enter"),
    ("Explorer", "New folder", "Ctrl+Shift+N"),
    ("Explorer", "Up one level", "Alt+Up"),
    ("Explorer", "Back", "Alt+Left"),
    ("Explorer", "Forward", "Alt+Right"),
    ("Explorer", "Address bar", "Alt+D"),
    ("Explorer", "Rename", "F2"),
    ("Explorer", "Refresh", "F5"),
    ("Explorer", "Delete permanently", "Shift+Delete"),
    ("Explorer", "Full screen", "F11"),
    ("Browser", "New tab", "Ctrl+T"),
    ("Browser", "Close tab", "Ctrl+W"),
    ("Browser", "Reopen closed tab", "Ctrl+Shift+T"),
    ("Browser", "Next tab", "Ctrl+Tab"),
    ("Browser", "Previous tab", "Ctrl+Shift+Tab"),
    ("Browser", "Address bar", "Ctrl+L"),
    ("Browser", "Bookmark this page", "Ctrl+D"),
    ("Browser", "Bookmarks bar", "Ctrl+Shift+B"),
    ("Browser", "Incognito / InPrivate", "Ctrl+Shift+N"),
    ("Browser", "Downloads", "Ctrl+J"),
    ("Browser", "History", "Ctrl+H"),
    ("Browser", "Find on page", "Ctrl+F"),
    ("Browser", "Reload", "Ctrl+R"),
    ("Browser", "Hard reload", "Ctrl+Shift+R"),
    ("Browser", "DevTools", "F12"),
    ("Browser", "Zoom in", "Ctrl+Plus"),
    ("Browser", "Zoom out", "Ctrl+Minus"),
    ("Browser", "Reset zoom", "Ctrl+0"),
    ("VS Code", "Command Palette", "Ctrl+Shift+P"),
    ("VS Code", "Quick Open file", "Ctrl+P"),
    ("VS Code", "Toggle terminal", "Ctrl+Backquote"),
    ("VS Code", "Toggle sidebar", "Ctrl+B"),
    ("VS Code", "Search in files", "Ctrl+Shift+F"),
    ("VS Code", "Toggle line comment", "Ctrl+Slash"),
    ("VS Code", "Move line up", "Alt+Up"),
    ("VS Code", "Move line down", "Alt+Down"),
    ("VS Code", "Duplicate line", "Shift+Alt+Up"),
    ("VS Code", "Select next occurrence", "Ctrl+D"),
    ("VS Code", "Delete line", "Ctrl+Shift+K"),
    ("VS Code", "Rename symbol", "F2"),
    ("Office", "Bold", "Ctrl+B"),
    ("Office", "Italic", "Ctrl+I"),
    ("Office", "Underline", "Ctrl+U"),
    ("Office", "Save", "Ctrl+S"),
    ("Office", "Undo", "Ctrl+Z"),
    ("Office", "Redo", "Ctrl+Y"),
    ("Office", "Find", "Ctrl+F"),
    ("Office", "Print", "Ctrl+P"),
    ("Office", "Page break (Word)", "Ctrl+Enter"),
    ("Office", "Toggle filter (Excel)", "Ctrl+Shift+L"),
    ("Office", "AutoSum (Excel)", "Alt+Equal"),
    ("Office", "Edit cell (Excel)", "F2"),
    ("Design", "Free transform", "Ctrl+T"),
    ("Design", "Duplicate layer", "Ctrl+J"),
    ("Design", "New layer", "Ctrl+Shift+N"),
    ("Design", "Merge down", "Ctrl+E"),
    ("Design", "Fit on screen", "Ctrl+0"),
    ("Design", "Step backward", "Ctrl+Alt+Z"),
    ("Design", "Step forward", "Ctrl+Shift+Z"),
    ("Design", "Brush tool", "B"),
    ("Design", "Move tool", "V"),
    ("Design", "Save as", "Ctrl+Shift+S"),
    ("Terminal", "New tab", "Ctrl+Shift+T"),
    ("Terminal", "Close tab", "Ctrl+Shift+W"),
    ("Terminal", "Next tab", "Ctrl+Tab"),
    ("Terminal", "Previous tab", "Ctrl+Shift+Tab"),
    ("Terminal", "Clear screen", "Ctrl+L"),
    ("Terminal", "Copy selection", "Ctrl+Shift+C"),
    ("Terminal", "Paste", "Ctrl+Shift+V"),
    ("Gaming", "Game Bar", "Win+G"),
    ("Gaming", "Record clip", "Win+Alt+R"),
    ("Gaming", "Toggle fullscreen", "Alt+Enter"),
    ("Gaming", "Fullscreen", "F11"),
    ("Gaming", "Screenshot (Game Bar)", "Win+Alt+PrintScreen"),
    ("General", "Copy", "Ctrl+C"),
    ("General", "Paste", "Ctrl+V"),
    ("General", "Cut", "Ctrl+X"),
    ("General", "Select all", "Ctrl+A"),
    ("General", "Undo", "Ctrl+Z"),
    ("General", "Redo", "Ctrl+Y"),
    ("General", "Save", "Ctrl+S"),
    ("General", "Open", "Ctrl+O"),
    ("General", "New", "Ctrl+N"),
    ("General", "Print", "Ctrl+P"),
    ("General", "Find", "Ctrl+F"),
    ("General", "Close", "Ctrl+W"),
    ("General", "Screenshot", "PrintScreen"),
    ("General", "Close program", "Alt+F4"),
];

static CATS: &[&str] = &[
    "All", "Windows", "Explorer", "Browser", "VS Code", "Office", "Design", "Terminal", "Gaming", "General",
];

// ---------------------------------------------------------------------------
//  Config
// ---------------------------------------------------------------------------

fn exe_dir() -> String {
    let mut buf = [0u16; 260];
    unsafe {
        let n = GetModuleFileNameW(HMODULE(std::ptr::null_mut()), buf.as_mut_ptr(), buf.len() as u32);
        let p: String = String::from_utf16_lossy(&buf[..n as usize]);
        if let Some(pos) = p.rfind('\\') { p[..=pos].to_string() } else { p }
    }
}

#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" {
    fn GetModuleFileNameW(hmodule: HMODULE, lpFilename: *mut u16, nSize: u32) -> u32;
}

fn config_path() -> String {
    format!("{}keyflow.toml", exe_dir())
}

fn load_rules() -> AppState {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => match toml::from_str::<AppState>(&content) {
            Ok(state) => state,
            Err(_) => AppState::default(),
        },
        Err(_) => AppState::default(),
    }
}

fn save_rules(state: &AppState) {
    let path = config_path();
    if let Ok(toml_str) = toml::to_string(state) {
        let _ = std::fs::write(&path, toml_str);
    }
}

fn find_rule_for(rules: &[Rule], vk: u16, mods: u16) -> Option<usize> {
    rules.iter().position(|r| r.vk == vk && r.mods == mods)
}

fn find_rule_by_action(rules: &[Rule], action: &RuleAction) -> Option<usize> {
    rules.iter().position(|r| r.action == *action)
}

// ---------------------------------------------------------------------------
//  Keyboard hook (WH_KEYBOARD_LL)
// ---------------------------------------------------------------------------

type HookState = Arc<RwLock<AppState>>;

unsafe extern "system" fn low_level_keyboard_proc(
    code: i32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    if code == HC_ACTION as i32 {
        let vk;
        let injected;
        unsafe {
            let kbs = &*(l_param.0 as *const KBDLLHOOKSTRUCT);
            vk = kbs.vkCode as u16;
            injected = (kbs.flags.0 & 0x00000001) != 0;
        }
        if !injected {
            let mods = current_mods();
            let down = w_param.0 == WM_KEYDOWN as usize || w_param.0 == WM_SYSKEYDOWN as usize;
            if down {
                let state = HOOK_STATE.read();
                if let Some(idx) = find_rule_for(&state.rules, vk, mods) {
                    let rule = state.rules[idx].clone();
                    if state.enabled || matches!(rule.action, RuleAction::Toggle) {
                        drop(state);
                        execute_action(&rule.action);
                        if matches!(rule.action, RuleAction::Toggle) {
                            let mut st = HOOK_STATE.write();
                            st.enabled = !st.enabled;
                        }
                        return LRESULT(1);
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, code, w_param, l_param) }
}

static HOOK_STATE: once_cell::sync::Lazy<HookState> = once_cell::sync::Lazy::new(|| {
    Arc::new(RwLock::new(load_rules()))
});

fn hook_thread() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let proc: HOOKPROC = Some(low_level_keyboard_proc);
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, proc, Some(hinst.into()), 0);
        if hook.is_err() {
            eprintln!("Failed to install keyboard hook");
            return;
        }
        let hook = hook.unwrap();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = UnhookWindowsHookEx(hook);
    }
}

// ---------------------------------------------------------------------------
//  GUI
// ---------------------------------------------------------------------------

struct KeyFlowApp {
    state: HookState,
    tab: usize,
    cat_sel: usize,
    search: String,
    scroll_offset: f32,
    capturing: bool,
    replace_idx: Option<usize>,
    hotkey_vk: u16,
    hotkey_mods: u16,
    custom_type: usize,
    custom_value: String,
}

impl Default for KeyFlowApp {
    fn default() -> Self {
        Self {
            state: HOOK_STATE.clone(),
            tab: 0,
            cat_sel: 0,
            search: String::new(),
            scroll_offset: 0.0,
            capturing: false,
            replace_idx: None,
            hotkey_vk: 0,
            hotkey_mods: 0,
            custom_type: 0,
            custom_value: String::new(),
        }
    }
}

const ROW_H: f32 = 58.0;
const ROW_GAP: f32 = 8.0;

impl KeyFlowApp {
    fn ui(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("KeyFlow").size(28.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(if self.state.read().enabled { "Pause" } else { "Resume" }).clicked() {
                        let mut s = self.state.write();
                        s.enabled = !s.enabled;
                        save_rules(&s);
                    }
                });
            });
            ui.label("Browse real shortcuts by niche, then replace any of them with your own keys.");
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.selectable_label(self.tab == 0, "Replace shortcuts").clicked() {
                    self.tab = 0;
                }
                if ui.selectable_label(self.tab == 1, "Custom keys").clicked() {
                    self.tab = 1;
                }
            });
            ui.separator();

            match self.tab {
                0 => self.draw_replace_tab(ui),
                1 => self.draw_custom_tab(ui),
                _ => {}
            }
        });
    }

    fn draw_replace_tab(&mut self, ui: &mut egui::Ui) {
        let cat = CATS[self.cat_sel];

        ui.horizontal(|ui| {
            for (i, c) in CATS.iter().enumerate() {
                if ui.selectable_label(self.cat_sel == i, *c).clicked() {
                    self.cat_sel = i;
                }
            }
        });

        ui.label("Search:");
        ui.text_edit_singleline(&mut self.search);

        let view: Vec<usize> = (0..CATALOG.len())
            .filter(|&i| {
                if self.cat_sel != 0 && CATALOG[i].0 != cat { return false; }
                if !self.search.is_empty() {
                    let s = self.search.to_lowercase();
                    if !CATALOG[i].1.to_lowercase().contains(&s)
                        && !CATALOG[i].2.to_lowercase().contains(&s)
                    {
                        return false;
                    }
                }
                true
            })
            .collect();

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .max_height(ui.available_height() - 100.0)
            .show_rows(ui, ROW_H + ROW_GAP, view.len(), |ui, row_range| {
                for vi in row_range {
                    let ci = view[vi];
                    let cname = CATALOG[ci].1;
                    let cshortcut = CATALOG[ci].2;
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(cname).strong());
                            ui.label(egui::RichText::new(CATALOG[ci].0).small().color(egui::Color32::GRAY));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let target_action = RuleAction::Keys(cshortcut.to_string());
                            let state = self.state.read();
                            let replaced = find_rule_by_action(&state.rules, &target_action).is_some();
                            drop(state);
                            if replaced {
                                if ui.button("✕ Undo").clicked() {
                                    let idx = find_rule_by_action(&HOOK_STATE.read().rules, &target_action).unwrap();
                                    let mut st = HOOK_STATE.write();
                                    st.rules.remove(idx);
                                    save_rules(&st);
                                }
                                ui.label(egui::RichText::new("Replaced").color(egui::Color32::GREEN));
                            } else {
                                if ui.button("Replace").clicked() {
                                    self.capturing = true;
                                    self.replace_idx = Some(ci);
                                }
                            }
                        });
                    });
                    ui.separator();
                }
            });

        ui.separator();
        ui.label(if self.state.read().enabled {
            "Pick a shortcut → Replace → press your new keys"
        } else {
            "Paused"
        });
    }

    fn draw_custom_tab(&mut self, ui: &mut egui::Ui) {
        let state = self.state.read();

        ui.label("Your custom hotkeys");

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .max_height(ui.available_height() - 200.0)
            .show(ui, |ui| {
                if state.rules.is_empty() {
                    ui.label("No custom hotkeys yet — add one below.");
                }
                let rules = state.rules.clone();
                drop(state);
                for (i, rule) in rules.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(combo_to_string(rule.vk, rule.mods));
                        let desc = match &rule.action {
                            RuleAction::Toggle => "Pauses / resumes KeyFlow".to_string(),
                            RuleAction::Keys(k) => format!("Replaces with: {}", k),
                            RuleAction::Text(t) => format!("Types: {}", t),
                            RuleAction::Run(p) => format!("Opens: {}", p),
                            RuleAction::Url(u) => format!("Opens link: {}", u),
                        };
                        ui.label(desc);
                        if ui.button("✕").clicked() {
                            let mut st = HOOK_STATE.write();
                            st.rules.remove(i);
                            save_rules(&st);
                        }
                    });
                    ui.separator();
                }
            });

        ui.separator();
        ui.group(|ui| {
            ui.label("Add new hotkey");

            ui.horizontal(|ui| {
                ui.label("Hotkey:");
                if self.capturing {
                    ui.label(egui::RichText::new("Press your key combination…").strong());
                } else {
                    let label = if self.hotkey_vk != 0 {
                        combo_to_string(self.hotkey_vk, self.hotkey_mods)
                    } else {
                        "Click, then press keys".to_string()
                    };
                    if ui.button(&label).clicked() {
                        self.capturing = true;
                    }
                }
            });

            ui.label("Action:");
            ui.radio_value(&mut self.custom_type, 0, "Text");
            ui.radio_value(&mut self.custom_type, 1, "Keys");
            ui.radio_value(&mut self.custom_type, 2, "Program");
            ui.radio_value(&mut self.custom_type, 3, "URL");
            ui.radio_value(&mut self.custom_type, 4, "Toggle");

            ui.label("Value:");
            if self.custom_type != 4 {
                ui.text_edit_singleline(&mut self.custom_value);
            } else {
                ui.label("(none)");
            }

            if ui.button("+ Add hotkey").clicked() {
                if self.hotkey_vk == 0 {
                    return;
                }
                if self.custom_type != 4 && self.custom_value.trim().is_empty() {
                    return;
                }
                let action = match self.custom_type {
                    0 => RuleAction::Text(self.custom_value.trim().to_string()),
                    1 => RuleAction::Keys(self.custom_value.trim().to_string()),
                    2 => RuleAction::Run(self.custom_value.trim().to_string()),
                    3 => RuleAction::Url(self.custom_value.trim().to_string()),
                    _ => RuleAction::Toggle,
                };
                if find_rule_for(&HOOK_STATE.read().rules, self.hotkey_vk, self.hotkey_mods).is_some() {
                    return;
                }
                let rule = Rule {
                    vk: self.hotkey_vk,
                    mods: self.hotkey_mods,
                    action,
                };
                let mut st = HOOK_STATE.write();
                st.rules.push(rule);
                save_rules(&st);
                self.hotkey_vk = 0;
                self.hotkey_mods = 0;
                self.custom_value.clear();
            }
        });

        ui.separator();
        ui.label(if self.state.read().enabled { "Active" } else { "Paused" });
    }
}

impl eframe::App for KeyFlowApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx, _frame);

        // Handle key capture via egui input
        ctx.input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Key { key, pressed, modifiers, .. } if *pressed => {
                        if self.capturing {
                            if *key == egui::Key::Escape {
                                self.capturing = false;
                                self.replace_idx = None;
                            } else {
                                let vk = egui_key_to_vk(*key);
                                let mods = egui_mods_to_mods(modifiers);
                                if vk != 0 && !is_modifier_vk(vk) {
                                    if let Some(ci) = self.replace_idx {
                                        let original_combo = CATALOG[ci].2.to_string();
                                        let target_action = RuleAction::Keys(original_combo);
                                        let mut st = HOOK_STATE.write();
                                        if let Some(idx) = find_rule_for(&st.rules, vk, mods) {
                                            st.rules.remove(idx);
                                        }
                                        st.rules.push(Rule { vk, mods, action: target_action });
                                        save_rules(&st);
                                        self.capturing = false;
                                        self.replace_idx = None;
                                    } else {
                                        self.hotkey_vk = vk;
                                        self.hotkey_mods = mods;
                                        self.capturing = false;
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
    }
}

fn egui_key_to_vk(key: egui::Key) -> u16 {
    match key {
        egui::Key::A => 0x41, egui::Key::B => 0x42, egui::Key::C => 0x43,
        egui::Key::D => 0x44, egui::Key::E => 0x45, egui::Key::F => 0x46,
        egui::Key::G => 0x47, egui::Key::H => 0x48, egui::Key::I => 0x49,
        egui::Key::J => 0x4A, egui::Key::K => 0x4B, egui::Key::L => 0x4C,
        egui::Key::M => 0x4D, egui::Key::N => 0x4E, egui::Key::O => 0x4F,
        egui::Key::P => 0x50, egui::Key::Q => 0x51, egui::Key::R => 0x52,
        egui::Key::S => 0x53, egui::Key::T => 0x54, egui::Key::U => 0x55,
        egui::Key::V => 0x56, egui::Key::W => 0x57, egui::Key::X => 0x58,
        egui::Key::Y => 0x59, egui::Key::Z => 0x5A,
        egui::Key::Num0 => 0x30, egui::Key::Num1 => 0x31, egui::Key::Num2 => 0x32,
        egui::Key::Num3 => 0x33, egui::Key::Num4 => 0x34, egui::Key::Num5 => 0x35,
        egui::Key::Num6 => 0x36, egui::Key::Num7 => 0x37, egui::Key::Num8 => 0x38,
        egui::Key::Num9 => 0x39,
        egui::Key::F1 => 0x70, egui::Key::F2 => 0x71, egui::Key::F3 => 0x72,
        egui::Key::F4 => 0x73, egui::Key::F5 => 0x74, egui::Key::F6 => 0x75,
        egui::Key::F7 => 0x76, egui::Key::F8 => 0x77, egui::Key::F9 => 0x78,
        egui::Key::F10 => 0x79, egui::Key::F11 => 0x7A, egui::Key::F12 => 0x7B,
        egui::Key::Escape => 0x1B, egui::Key::Space => 0x20,
        egui::Key::Enter => 0x0D, egui::Key::Tab => 0x09,
        egui::Key::Backspace => 0x08, egui::Key::Delete => 0x2E,
        egui::Key::ArrowLeft => 0x25, egui::Key::ArrowUp => 0x26,
        egui::Key::ArrowRight => 0x27, egui::Key::ArrowDown => 0x28,
        egui::Key::Home => 0x24, egui::Key::End => 0x23,
        egui::Key::PageUp => 0x21, egui::Key::PageDown => 0x22,
        egui::Key::Insert => 0x2D,
        egui::Key::Minus => 0xBD, egui::Key::Equals => 0xBB,
        egui::Key::Semicolon => 0xBA,
        egui::Key::Comma => 0xBC, egui::Key::Period => 0xBE,
        egui::Key::Slash => 0xBF, egui::Key::Backslash => 0x5C,
        egui::Key::Quote => 0xDE,
        _ => 0,
    }
}

fn egui_mods_to_mods(mods: &egui::Modifiers) -> u16 {
    let mut m = 0;
    if mods.ctrl { m |= M_CTRL; }
    if mods.alt { m |= M_ALT; }
    if mods.shift { m |= M_SHIFT; }
    if mods.mac_cmd { m |= M_WIN; }
    m
}

// ---------------------------------------------------------------------------
//  Main
// ---------------------------------------------------------------------------

fn main() -> eframe::Result {
    thread::spawn(move || {
        hook_thread();
    });

    let options = eframe::NativeOptions {
        ..Default::default()
    };
    eframe::run_native(
        "KeyFlow",
        options,
        Box::new(|_cc| Ok(Box::<KeyFlowApp>::default())),
    )
}
