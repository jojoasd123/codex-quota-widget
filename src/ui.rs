use crate::quota::{self, Snapshot};
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::{Dwm::*, Gdi::*};
use windows_sys::Win32::System::SystemServices::SS_OWNERDRAW;
use windows_sys::Win32::System::{
    LibraryLoader::{GetModuleHandleW, GetProcAddress},
    Registry::*,
    Time::*,
};
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::{Controls::*, HiDpi::*, Shell::*, WindowsAndMessaging::*};

const WIDTH: i32 = 360;
const BG: u32 = rgb(249, 250, 247);
const INK: u32 = rgb(29, 43, 37);
const MUTED: u32 = rgb(70, 82, 74);
const GREEN: u32 = rgb(35, 104, 76);
const AMBER: u32 = rgb(148, 77, 12);
const TRAY: u32 = WM_APP + 1;
const HOVER: u32 = WM_APP + 2;
const REFRESH: usize = 101;
const PIN: usize = 102;
const HIDE: usize = 103;
const MORE: usize = 104;
const SHOW: usize = 105;
const EXIT: usize = 106;
const RESETS: usize = 107;

const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | g << 8 | b << 16
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

enum Request {
    Refresh,
    Stop,
}
enum Event {
    Loading,
    Complete(Result<Snapshot, String>),
    Stopped,
}

struct App {
    hwnd: HWND,
    dpi: i32,
    brush: HBRUSH,
    fonts: [HFONT; 4],
    labels: Vec<HWND>,
    bars: Vec<(i32, i64)>,
    lines: Vec<i32>,
    buttons: [HWND; 4],
    list: HWND,
    icon: HICON,
    snapshot: Option<Snapshot>,
    error: Option<String>,
    loading: bool,
    pinned: bool,
    expanded: bool,
    closing: bool,
    height: i32,
    tick: u32,
    taskbar_created: u32,
    glass: bool,
    theme: HTHEME,
    hovered: usize,
    hover: [f32; 4],
    motion: bool,
    requests: Sender<Request>,
    events: Receiver<Event>,
}

impl App {
    fn px(&self, value: i32) -> i32 {
        (value * self.dpi + 48) / 96
    }

    unsafe fn fonts(&mut self) {
        for font in self.fonts {
            if !font.is_null() {
                DeleteObject(font);
            }
        }
        for (index, (height, weight)) in [(13, 400), (14, 600), (58, 600), (12, 400)]
            .into_iter()
            .enumerate()
        {
            self.fonts[index] = CreateFontW(
                -self.px(height),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                0,
                0,
                CLEARTYPE_QUALITY as u32,
                0,
                wide(if index == 2 {
                    "Segoe UI"
                } else {
                    "Microsoft YaHei UI"
                })
                .as_ptr(),
            );
        }
    }

    unsafe fn control(&self, class: &str, text: &str, style: u32, id: usize) -> HWND {
        CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            self.hwnd,
            id as HMENU,
            GetModuleHandleW(null()),
            null(),
        )
    }

    #[allow(clippy::too_many_arguments)] // Native geometry plus font, color and alignment.
    unsafe fn label(
        &mut self,
        text: &str,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        font: usize,
        color: u32,
        right: bool,
    ) {
        let hwnd = self.control("STATIC", text, SS_OWNERDRAW, 0);
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            (color | ((right as u32) << 24)) as isize,
        );
        SendMessageW(hwnd, WM_SETFONT, self.fonts[font] as usize, 0);
        MoveWindow(
            hwnd,
            self.px(x),
            self.px(y),
            self.px(width),
            self.px(height),
            0,
        );
        self.labels.push(hwnd);
    }

    unsafe fn initialize(&mut self) {
        self.dpi = GetDpiForWindow(self.hwnd).max(96) as i32;
        self.fonts();
        self.theme = OpenThemeData(self.hwnd, wide("WINDOW").as_ptr());
        self.material();
        let corners: u32 = 2;
        DwmSetWindowAttribute(
            self.hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &corners as *const _ as _,
            4,
        );
        DwmSetWindowAttribute(self.hwnd, DWMWA_TEXT_COLOR as u32, &INK as *const _ as _, 4);
        self.icon = make_icon();
        SendMessageW(
            self.hwnd,
            WM_SETICON,
            ICON_SMALL as usize,
            self.icon as isize,
        );
        SendMessageW(self.hwnd, WM_SETICON, ICON_BIG as usize, self.icon as isize);
        for (index, (text, id)) in [
            ("其他额度", MORE),
            ("置顶", PIN),
            ("刷新", REFRESH),
            ("收起", HIDE),
        ]
        .into_iter()
        .enumerate()
        {
            self.buttons[index] =
                self.control("BUTTON", text, WS_TABSTOP | BS_OWNERDRAW as u32, id);
            SetWindowSubclass(self.buttons[index], Some(button_proc), id, 0);
        }
        self.list = self.control(
            "LISTBOX",
            "重置机会到期时间",
            WS_TABSTOP
                | WS_VSCROLL
                | LBS_OWNERDRAWFIXED as u32
                | LBS_HASSTRINGS as u32
                | LBS_NOINTEGRALHEIGHT as u32
                | LBS_NOTIFY as u32,
            RESETS,
        );
        self.taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        self.tray(NIM_ADD);
        self.layout();
        SetTimer(self.hwnd, 1, 500, None);
    }

    unsafe fn material(&mut self) {
        let backdrop = DWMSBT_TRANSIENTWINDOW;
        self.glass = DwmSetWindowAttribute(
            self.hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE as u32,
            &backdrop as *const _ as _,
            4,
        ) >= 0;
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        self.glass &= DwmExtendFrameIntoClientArea(self.hwnd, &margins) >= 0;
        let mut contrast: HIGHCONTRASTW = zeroed();
        contrast.cbSize = size_of::<HIGHCONTRASTW>() as u32;
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            &mut contrast as *mut _ as _,
            0,
        );
        let mut transparency: u32 = 1;
        let mut size = 4;
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize").as_ptr(),
            wide("EnableTransparency").as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut transparency as *mut _ as _,
            &mut size,
        );
        self.glass &= transparency != 0 && contrast.dwFlags & HCF_HIGHCONTRASTON == 0;
        // ponytail: this local Win11 widget uses the native accent policy so glass survives
        // deactivation. If Windows removes it, the system backdrop remains the fallback.
        #[repr(C)]
        struct CompositionAttribute {
            kind: u32,
            data: *mut std::ffi::c_void,
            size: usize,
        }
        type SetComposition = unsafe extern "system" fn(HWND, *mut CompositionAttribute) -> i32;
        if let Some(address) = GetProcAddress(
            GetModuleHandleW(wide("user32.dll").as_ptr()),
            c"SetWindowCompositionAttribute".as_ptr() as _,
        ) {
            let set: SetComposition = std::mem::transmute(address);
            let region = CreateRectRgn(0, 0, -1, -1);
            let blur = DWM_BLURBEHIND {
                dwFlags: DWM_BB_ENABLE | DWM_BB_BLURREGION | DWM_BB_TRANSITIONONMAXIMIZED,
                fEnable: self.glass as i32,
                hRgnBlur: region,
                fTransitionOnMaximized: 1,
            };
            DwmEnableBlurBehindWindow(self.hwnd, &blur);
            DeleteObject(region);
            // ACCENT_POLICY: state, flags, ABGR tint, animation id.
            let mut accent = [if self.glass { 4u32 } else { 0 }, 0, 0xCCF6FAF4, 0];
            let mut attribute = CompositionAttribute {
                kind: 19,
                data: accent.as_mut_ptr() as _,
                size: size_of_val(&accent),
            };
            let automatic = DWMSBT_AUTO;
            DwmSetWindowAttribute(
                self.hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE as u32,
                &automatic as *const _ as _,
                4,
            );
            if set(self.hwnd, &mut attribute) == 0 && self.glass {
                DwmSetWindowAttribute(
                    self.hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE as u32,
                    &backdrop as *const _ as _,
                    4,
                );
            }
        }
        if !self.brush.is_null() {
            DeleteObject(self.brush);
        }
        self.brush = CreateSolidBrush(if self.glass { 0 } else { BG });
        // Keep native caption buttons legible on the light glass tint.
        let dark_mode: i32 = 0;
        DwmSetWindowAttribute(
            self.hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &dark_mode as *const _ as _,
            4,
        );
        let caption = BG;
        DwmSetWindowAttribute(
            self.hwnd,
            DWMWA_CAPTION_COLOR as u32,
            &caption as *const _ as _,
            4,
        );
        let border = rgb(192, 214, 205);
        DwmSetWindowAttribute(
            self.hwnd,
            DWMWA_BORDER_COLOR as u32,
            &border as *const _ as _,
            4,
        );
        let mut enabled: i32 = 1;
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            &mut enabled as *mut _ as _,
            0,
        );
        self.motion = enabled != 0;
    }

    unsafe fn animate(&mut self) {
        let mut active = false;
        for (index, button) in self.buttons.into_iter().enumerate() {
            let target = (GetDlgCtrlID(button) as usize == self.hovered) as i32 as f32;
            let previous = self.hover[index];
            self.hover[index] = if !self.motion || (target - previous).abs() < 0.015 {
                target
            } else {
                previous + (target - previous) * 0.4
            };
            active |= self.hover[index] != target;
            if self.hover[index] != previous {
                InvalidateRect(button, null(), 0);
            }
        }
        if !active {
            KillTimer(self.hwnd, 2);
        }
    }

    unsafe fn tray(&self, message: u32) {
        let mut data: NOTIFYICONDATAW = zeroed();
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = 1;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.hIcon = self.icon;
        data.uCallbackMessage = TRAY;
        let tip = self
            .snapshot
            .as_ref()
            .and_then(|s| {
                s.windows.first().map(|w| {
                    format!(
                        "Codex · {}剩余 {}% · {} 次重置{}",
                        w.label(),
                        w.remaining,
                        s.reset_count
                            .map(|v| v.to_string())
                            .unwrap_or("未知".into()),
                        if self.error.is_some() {
                            " · 数据待更新"
                        } else {
                            ""
                        }
                    )
                })
            })
            .unwrap_or("Codex · 正在读取额度".into());
        for (to, from) in data.szTip.iter_mut().take(127).zip(tip.encode_utf16()) {
            *to = from;
        }
        Shell_NotifyIconW(message, &data);
    }

    unsafe fn layout(&mut self) {
        let was_visible = IsWindowVisible(self.hwnd) != 0;
        SendMessageW(self.hwnd, WM_SETREDRAW, 0, 0);
        for hwnd in self.labels.drain(..) {
            DestroyWindow(hwnd);
        }
        self.bars.clear();
        self.lines.clear();
        let snapshot = self.snapshot.clone();
        let now = quota::now();
        let mut y = 22;
        let status = if self.error.is_some() {
            "● 待更新"
        } else if self.loading {
            "● 同步中"
        } else {
            "● 已同步"
        };
        self.label(
            status,
            246,
            y,
            92,
            20,
            3,
            if self.error.is_some() { AMBER } else { GREEN },
            true,
        );

        let mut windows = snapshot
            .as_ref()
            .map(|s| s.windows.clone())
            .unwrap_or_default();
        let main_bucket = windows
            .first()
            .map(|w| w.bucket.clone())
            .unwrap_or_default();
        let extras: Vec<_> = windows
            .iter()
            .filter(|w| w.bucket != main_bucket)
            .cloned()
            .collect();
        windows.retain(|w| w.bucket == main_bucket);
        if windows.is_empty() {
            self.label("当前额度", 22, y, 200, 20, 0, MUTED, false);
            self.label("—", 20, y + 21, 180, 82, 2, INK, false);
            self.label(
                if snapshot.is_some() {
                    "账号暂未提供额度窗口"
                } else {
                    "正在连接已登录的 Codex…"
                },
                22,
                y + 115,
                316,
                24,
                0,
                MUTED,
                false,
            );
            y += 151;
        } else {
            for (index, window) in windows.iter().enumerate() {
                let title = if window.bucket == "codex" {
                    format!("{}剩余", window.label())
                } else {
                    format!("{} · {}", window.name, window.label())
                };
                if index == 0 {
                    self.label(&title, 22, y, 220, 20, 0, MUTED, false);
                    let color = if window.remaining <= 20 { AMBER } else { INK };
                    self.label(
                        &format!("{}%", window.remaining),
                        18,
                        y + 20,
                        180,
                        83,
                        2,
                        color,
                        false,
                    );
                    let countdown = window
                        .resets_at
                        .map(|t| quota::countdown(t, now))
                        .unwrap_or("时间未知".into());
                    self.label(&countdown, 187, y + 48, 151, 22, 1, INK, true);
                    self.label(
                        if window.resets_at.is_some_and(|t| t <= now) {
                            "等待服务端更新"
                        } else {
                            "后自动恢复"
                        },
                        187,
                        y + 75,
                        151,
                        20,
                        3,
                        MUTED,
                        true,
                    );
                    self.bars.push((y + 112, window.remaining));
                    let date = window
                        .resets_at
                        .map(local_date)
                        .unwrap_or("暂未提供".into());
                    self.label(
                        &format!("恢复时间  {date}"),
                        22,
                        y + 130,
                        316,
                        23,
                        3,
                        MUTED,
                        false,
                    );
                    y += 172;
                } else {
                    self.label(&window.label(), 22, y, 200, 22, 0, MUTED, false);
                    self.label(
                        &format!("{}%", window.remaining),
                        246,
                        y,
                        92,
                        22,
                        1,
                        INK,
                        true,
                    );
                    self.bars.push((y + 30, window.remaining));
                    self.label(
                        &format!(
                            "恢复时间  {}",
                            window
                                .resets_at
                                .map(local_date)
                                .unwrap_or("暂未提供".into())
                        ),
                        22,
                        y + 45,
                        316,
                        20,
                        3,
                        MUTED,
                        false,
                    );
                    y += 82;
                }
            }
        }

        self.lines.push(y);
        y += 21;
        self.label("可用重置", 22, y, 200, 24, 1, INK, false);
        let count = snapshot.as_ref().and_then(|s| s.reset_count);
        self.label(
            &count.map(|c| format!("{c} 次")).unwrap_or("—".into()),
            246,
            y,
            92,
            24,
            1,
            GREEN,
            true,
        );
        y += 30;
        self.label(
            "到期前可在 Codex 中手动使用",
            22,
            y,
            316,
            22,
            3,
            MUTED,
            false,
        );
        y += 30;

        let selection = SendMessageW(self.list, LB_GETCURSEL, 0, 0);
        SendMessageW(self.list, WM_SETREDRAW, 0, 0);
        SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
        SendMessageW(self.list, WM_SETFONT, self.fonts[0] as usize, 0);
        SendMessageW(self.list, LB_SETITEMHEIGHT, 0, self.px(38) as isize);
        let rows = snapshot.as_ref().and_then(|s| s.resets.as_ref());
        let row_count = rows.map_or(0, Vec::len);
        ShowWindow(self.list, if row_count == 0 { SW_HIDE } else { SW_SHOWNA });
        if let Some(rows) = rows.filter(|v| !v.is_empty()) {
            for (index, reset) in rows.iter().enumerate() {
                let date = reset.expires_at.map(local_date).unwrap_or(
                    if reset.never_expires {
                        "无到期限制"
                    } else {
                        "到期时间未知"
                    }
                    .into(),
                );
                let remaining = match reset.expires_at {
                    Some(t) if t <= now => "已到期 · 待同步".into(),
                    Some(t) if t - now >= 86400 => format!("{} 天后", (t - now + 86399) / 86400),
                    Some(t) => quota::countdown(t, now),
                    None => "—".into(),
                };
                let row = wide(&format!("{:02}\t{date}\t{remaining}", index + 1));
                SendMessageW(self.list, LB_ADDSTRING, 0, row.as_ptr() as isize);
            }
            MoveWindow(
                self.list,
                self.px(20),
                self.px(y),
                self.px(320),
                self.px(row_count.min(5) as i32 * 38),
                0,
            );
            if selection >= 0 {
                SendMessageW(
                    self.list,
                    LB_SETCURSEL,
                    (selection as usize).min(row_count - 1),
                    0,
                );
            }
            y += row_count.min(5) as i32 * 38;
        } else {
            let message = if count == Some(0) {
                "暂无可用重置机会"
            } else if snapshot.is_some() {
                "到期明细暂不可用"
            } else {
                "等待同步重置明细…"
            };
            self.label(message, 22, y + 7, 316, 26, 0, MUTED, false);
            y += 43;
        }
        SendMessageW(self.list, WM_SETREDRAW, 1, 0);
        ShowWindow(self.list, if row_count == 0 { SW_HIDE } else { SW_SHOWNA });
        if count.is_some_and(|c| c > row_count as u64) && row_count > 0 {
            self.label(
                &format!("已返回 {row_count} 条明细，其余暂不可用"),
                22,
                y + 5,
                316,
                22,
                3,
                AMBER,
                false,
            );
            y += 31;
        }
        if self.expanded {
            y += 13;
            self.lines.push(y);
            y += 18;
            for window in &extras {
                self.label(&window.name, 22, y, 316, 22, 0, INK, false);
                y += 28;
                self.label(
                    &format!("{}  ·  剩余 {}%", window.label(), window.remaining),
                    22,
                    y,
                    316,
                    20,
                    3,
                    MUTED,
                    false,
                );
                y += 24;
                self.label(
                    &format!(
                        "恢复  {}",
                        window
                            .resets_at
                            .map(local_date)
                            .unwrap_or("时间未知".into())
                    ),
                    22,
                    y,
                    316,
                    20,
                    3,
                    MUTED,
                    false,
                );
                y += 35;
            }
        }
        y += 15;
        self.lines.push(y);
        y += 15;
        let freshness = if let Some(error) = &self.error {
            error.clone()
        } else if let Some(snapshot) = &snapshot {
            format!(
                "{} 更新  ·  {}",
                local_time(snapshot.captured_at),
                snapshot.source
            )
        } else {
            "首次连接通常需要几秒钟".into()
        };
        let status_height = if self.error.is_some() { 42 } else { 23 };
        self.label(
            &freshness,
            22,
            y,
            316,
            status_height,
            3,
            if self.error.is_some() { AMBER } else { MUTED },
            false,
        );
        y += status_height + 13;
        for (index, hwnd) in self.buttons.into_iter().enumerate() {
            MoveWindow(
                hwnd,
                self.px(20 + index as i32 * 82),
                self.px(y),
                self.px(74),
                self.px(33),
                0,
            );
        }
        EnableWindow(self.buttons[0], (!extras.is_empty()) as i32);
        EnableWindow(self.buttons[2], (!self.loading) as i32);
        SetWindowTextW(
            self.buttons[0],
            wide(if self.expanded {
                "收起其他"
            } else {
                "其他额度"
            })
            .as_ptr(),
        );
        SetWindowTextW(
            self.buttons[1],
            wide(if self.pinned { "已置顶" } else { "置顶" }).as_ptr(),
        );
        SetWindowTextW(
            self.buttons[2],
            wide(if self.loading { "同步中" } else { "刷新" }).as_ptr(),
        );
        y += 53;
        if self.height != y {
            self.height = y;
            let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32;
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: self.px(WIDTH),
                bottom: self.px(y),
            };
            AdjustWindowRectExForDpi(&mut rect, style, 0, WS_EX_CONTROLPARENT, self.dpi as u32);
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            let mut position: RECT = zeroed();
            GetWindowRect(self.hwnd, &mut position);
            let mut monitor: MONITORINFO = zeroed();
            monitor.cbSize = size_of::<MONITORINFO>() as u32;
            GetMonitorInfoW(
                MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            );
            let x = position
                .left
                .min(monitor.rcWork.right - width - self.px(12))
                .max(monitor.rcWork.left);
            let y = position
                .top
                .min(monitor.rcWork.bottom - height - self.px(12))
                .max(monitor.rcWork.top);
            SetWindowPos(
                self.hwnd,
                null_mut(),
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        SendMessageW(self.hwnd, WM_SETREDRAW, 1, 0);
        if !was_visible {
            ShowWindow(self.hwnd, SW_HIDE);
        }
        RedrawWindow(
            self.hwnd,
            null(),
            null_mut(),
            RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN,
        );
        self.tray(NIM_MODIFY);
    }

    unsafe fn action(&mut self, id: usize) {
        match id {
            REFRESH if !self.loading => {
                self.loading = true;
                let _ = self.requests.send(Request::Refresh);
                self.layout();
            }
            PIN => {
                self.pinned = !self.pinned;
                SetWindowPos(
                    self.hwnd,
                    if self.pinned {
                        HWND_TOPMOST
                    } else {
                        HWND_NOTOPMOST
                    },
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                self.layout();
            }
            HIDE => {
                ShowWindow(self.hwnd, SW_HIDE);
            }
            SHOW => {
                ShowWindow(self.hwnd, SW_RESTORE);
                SetForegroundWindow(self.hwnd);
            }
            MORE => {
                self.expanded = !self.expanded;
                self.layout();
            }
            EXIT if !self.closing => {
                self.closing = true;
                ShowWindow(self.hwnd, SW_HIDE);
                let _ = self.requests.send(Request::Stop);
            }
            _ => {}
        }
    }

    unsafe fn tick(&mut self) {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Loading => {
                    self.loading = true;
                    changed = true;
                }
                Event::Complete(result) => {
                    self.loading = false;
                    match result {
                        Ok(snapshot) => {
                            self.snapshot = Some(snapshot);
                            self.error = None;
                        }
                        Err(error) => {
                            self.error = Some(error);
                        }
                    }
                    changed = true;
                }
                Event::Stopped => {
                    DestroyWindow(self.hwnd);
                    return;
                }
            }
        }
        self.tick += 1;
        if (changed || self.tick.is_multiple_of(60)) && !self.closing {
            self.layout();
        }
    }

    unsafe fn paint(&self) {
        let mut ps = zeroed();
        let dc = BeginPaint(self.hwnd, &mut ps);
        let mut rect: RECT = zeroed();
        GetClientRect(self.hwnd, &mut rect);
        if let Some(canvas) = Canvas::new(dc, rect, self.glass) {
            for y in &self.lines {
                canvas.round_rect(
                    RECT {
                        left: self.px(22),
                        top: self.px(*y),
                        right: self.px(338),
                        bottom: self.px(*y) + 1,
                    },
                    INK,
                    28,
                    0.0,
                );
            }
            for (y, remaining) in &self.bars {
                let track = RECT {
                    left: self.px(22),
                    top: self.px(*y),
                    right: self.px(338),
                    bottom: self.px(*y + 7),
                };
                canvas.round_rect(track, INK, 25, self.px(4) as f32);
                let fill = RECT {
                    right: self.px(22 + 316 * *remaining as i32 / 100),
                    ..track
                };
                canvas.round_rect(
                    fill,
                    if *remaining <= 20 { AMBER } else { GREEN },
                    245,
                    self.px(4) as f32,
                );
                canvas.round_rect(
                    RECT {
                        top: track.top + 1,
                        bottom: track.top + self.px(2),
                        left: fill.left + self.px(3),
                        right: fill.right - self.px(3),
                    },
                    rgb(190, 255, 219),
                    85,
                    1.0,
                );
            }
        } else {
            FillRect(dc, &ps.rcPaint, self.brush);
        }
        EndPaint(self.hwnd, &ps);
    }

    unsafe fn draw_item(&self, draw: &DRAWITEMSTRUCT) {
        let Some(canvas) = Canvas::new(draw.hDC, draw.rcItem, self.glass) else {
            return;
        };
        if draw.CtlType == ODT_STATIC {
            let mut buffer = [0u16; 512];
            let len =
                GetWindowTextW(draw.hwndItem, buffer.as_mut_ptr(), buffer.len() as i32) as usize;
            let text = String::from_utf16_lossy(&buffer[..len]);
            let flags = GetWindowLongPtrW(draw.hwndItem, GWLP_USERDATA) as u32;
            let font = SendMessageW(draw.hwndItem, WM_GETFONT, 0, 0) as HFONT;
            canvas.text(
                self.theme,
                &text,
                draw.rcItem,
                font,
                flags & 0xffffff,
                DT_TOP
                    | DT_WORDBREAK
                    | DT_NOPREFIX
                    | if flags & 0x1000000 != 0 {
                        DT_RIGHT
                    } else {
                        DT_LEFT
                    },
            );
        } else if draw.CtlID as usize == RESETS {
            if draw.itemID == u32::MAX {
                return;
            }
            let len = SendMessageW(self.list, LB_GETTEXTLEN, draw.itemID as usize, 0);
            if len < 0 {
                return;
            }
            let mut buffer = vec![0u16; len as usize + 1];
            SendMessageW(
                self.list,
                LB_GETTEXT,
                draw.itemID as usize,
                buffer.as_mut_ptr() as isize,
            );
            let text = String::from_utf16_lossy(&buffer[..len as usize]);
            let parts: Vec<_> = text.split('\t').collect();
            let selected = draw.itemState & ODS_SELECTED != 0;
            let mut plate = draw.rcItem;
            InflateRect(&mut plate, 0, -self.px(3));
            canvas.round_rect(
                plate,
                if selected { GREEN } else { rgb(255, 255, 255) },
                if selected { 30 } else { 82 },
                self.px(8) as f32,
            );
            if parts.len() == 3 {
                let mut rect = draw.rcItem;
                rect.left += self.px(8);
                rect.right = rect.left + self.px(27);
                canvas.text(
                    self.theme,
                    parts[0],
                    rect,
                    self.fonts[3],
                    MUTED,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                );
                rect.left = self.px(37);
                rect.right = self.px(224);
                canvas.text(
                    self.theme,
                    parts[1],
                    rect,
                    self.fonts[0],
                    INK,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                );
                rect.left = self.px(224);
                rect.right = draw.rcItem.right - self.px(8);
                canvas.text(
                    self.theme,
                    parts[2],
                    rect,
                    self.fonts[3],
                    MUTED,
                    DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
                );
            }
            if draw.itemState & ODS_FOCUS != 0 {
                canvas.outline(plate, GREEN, self.px(8) as f32);
            }
        } else {
            let id = draw.CtlID as usize;
            let disabled = draw.itemState & ODS_DISABLED != 0;
            let pressed = draw.itemState & ODS_SELECTED != 0;
            let hover = self
                .buttons
                .iter()
                .position(|b| *b == draw.hwndItem)
                .map(|i| self.hover[i])
                .unwrap_or(0.0);
            let mut rect = draw.rcItem;
            if pressed && self.motion {
                InflateRect(
                    &mut rect,
                    (-(rect.right - rect.left) as f32 * 0.02).round() as i32,
                    (-(rect.bottom - rect.top) as f32 * 0.02).round() as i32,
                );
            }
            let primary = id == REFRESH && !disabled;
            let pinned = id == PIN && self.pinned;
            let color = if primary || pinned {
                GREEN
            } else {
                rgb(255, 255, 255)
            };
            let alpha = if primary {
                (228.0 + hover * 27.0) as u8
            } else if pinned {
                50
            } else {
                (100.0 + hover * 90.0) as u8
            };
            canvas.round_rect(rect, color, alpha, self.px(9) as f32);
            canvas.outline(
                rect,
                if primary {
                    rgb(155, 224, 188)
                } else {
                    rgb(255, 255, 255)
                },
                self.px(9) as f32,
            );
            let mut buffer = [0u16; 64];
            let len = GetWindowTextW(draw.hwndItem, buffer.as_mut_ptr(), 64) as usize;
            canvas.text(
                self.theme,
                &String::from_utf16_lossy(&buffer[..len]),
                rect,
                self.fonts[3],
                if disabled {
                    MUTED
                } else if primary {
                    rgb(255, 255, 255)
                } else {
                    INK
                },
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            if draw.itemState & ODS_FOCUS != 0 {
                InflateRect(&mut rect, -self.px(3), -self.px(3));
                canvas.outline(
                    rect,
                    if primary { rgb(255, 255, 255) } else { GREEN },
                    self.px(6) as f32,
                );
            }
        }
    }

    unsafe fn menu(&mut self) {
        let menu = CreatePopupMenu();
        for (id, text) in [
            (SHOW, "显示小窗"),
            (REFRESH, "立即刷新"),
            (PIN, "窗口置顶"),
            (EXIT, "退出"),
        ] {
            AppendMenuW(
                menu,
                MF_STRING
                    | if id == PIN && self.pinned {
                        MF_CHECKED
                    } else {
                        0
                    },
                id,
                wide(text).as_ptr(),
            );
        }
        let mut point = zeroed();
        GetCursorPos(&mut point);
        SetForegroundWindow(self.hwnd);
        let action = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            0,
            self.hwnd,
            null(),
        );
        DestroyMenu(menu);
        self.action(action as usize);
        PostMessageW(self.hwnd, WM_NULL, 0, 0);
    }
}

// A native paint buffer preserves per-pixel alpha; ordinary GDI text would erase the glass.
struct Canvas {
    handle: isize,
    dc: HDC,
    rect: RECT,
    bits: *mut RGBQUAD,
    stride: i32,
}

impl Canvas {
    unsafe fn new(target: HDC, rect: RECT, glass: bool) -> Option<Self> {
        let params = BP_PAINTPARAMS {
            cbSize: size_of::<BP_PAINTPARAMS>() as u32,
            dwFlags: BPPF_ERASE,
            ..zeroed()
        };
        let mut dc = null_mut();
        let handle = BeginBufferedPaint(target, &rect, BPBF_TOPDOWNDIB, &params, &mut dc);
        if handle == 0 {
            return None;
        }
        let mut bits = null_mut();
        let mut stride = 0;
        if GetBufferedPaintBits(handle, &mut bits, &mut stride) < 0 || bits.is_null() {
            EndBufferedPaint(handle, 0);
            return None;
        }
        let canvas = Self {
            handle,
            dc,
            rect,
            bits,
            stride,
        };
        if !glass {
            canvas.round_rect(rect, BG, 255, 0.0);
        }
        Some(canvas)
    }

    unsafe fn round_rect(&self, rect: RECT, color: u32, alpha: u8, radius: f32) {
        self.shape(rect, color, alpha, radius, false);
    }

    unsafe fn outline(&self, rect: RECT, color: u32, radius: f32) {
        self.shape(rect, color, 150, radius, true);
    }

    unsafe fn shape(&self, rect: RECT, color: u32, alpha: u8, radius: f32, outline: bool) {
        let half_x = (rect.right - rect.left) as f32 / 2.0;
        let half_y = (rect.bottom - rect.top) as f32 / 2.0;
        if half_x <= 0.0 || half_y <= 0.0 {
            return;
        }
        let radius = radius.min(half_x).min(half_y);
        for y in rect.top.max(self.rect.top)..rect.bottom.min(self.rect.bottom) {
            for x in rect.left.max(self.rect.left)..rect.right.min(self.rect.right) {
                let dx = ((x - rect.left) as f32 + 0.5 - half_x).abs() - (half_x - radius);
                let dy = ((y - rect.top) as f32 + 0.5 - half_y).abs() - (half_y - radius);
                let distance = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius;
                let coverage = if outline {
                    (0.5 - distance).clamp(0.0, 1.0) - (-0.5 - distance).clamp(0.0, 1.0)
                } else {
                    (0.5 - distance).clamp(0.0, 1.0)
                };
                let a = (alpha as f32 * coverage) as u32;
                let pixel = &mut *self
                    .bits
                    .add(((y - self.rect.top) * self.stride + x - self.rect.left) as usize);
                pixel.rgbRed = (((color & 255) * a + pixel.rgbRed as u32 * (255 - a)) / 255) as u8;
                pixel.rgbGreen =
                    ((((color >> 8) & 255) * a + pixel.rgbGreen as u32 * (255 - a)) / 255) as u8;
                pixel.rgbBlue =
                    ((((color >> 16) & 255) * a + pixel.rgbBlue as u32 * (255 - a)) / 255) as u8;
                pixel.rgbReserved = (a + pixel.rgbReserved as u32 * (255 - a) / 255) as u8;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn text(
        &self,
        theme: HTHEME,
        text: &str,
        mut rect: RECT,
        font: HFONT,
        color: u32,
        flags: u32,
    ) {
        let old = SelectObject(self.dc, font);
        let opts = DTTOPTS {
            dwSize: size_of::<DTTOPTS>() as u32,
            dwFlags: DTT_COMPOSITED | DTT_TEXTCOLOR,
            crText: color,
            ..zeroed()
        };
        let result = DrawThemeTextEx(
            theme,
            self.dc,
            0,
            0,
            wide(text).as_ptr(),
            -1,
            flags,
            &mut rect,
            &opts,
        );
        if result < 0 {
            self.round_rect(rect, BG, 255, 0.0);
            SetBkMode(self.dc, TRANSPARENT as i32);
            SetTextColor(self.dc, color);
            DrawTextW(self.dc, wide(text).as_ptr(), -1, &mut rect, flags);
            BufferedPaintSetAlpha(self.handle, &rect, 255);
        }
        SelectObject(self.dc, old);
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            EndBufferedPaint(self.handle, 1);
        }
    }
}

unsafe extern "system" fn button_proc(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
    id: usize,
    _data: usize,
) -> isize {
    match message {
        WM_MOUSEMOVE => {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut track);
            PostMessageW(GetParent(hwnd), HOVER, id, 0);
        }
        WM_MOUSELEAVE => {
            PostMessageW(GetParent(hwnd), HOVER, 0, 0);
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(button_proc), id);
        }
        _ => {}
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe fn fill(dc: HDC, rect: RECT, color: u32) {
    let brush = CreateSolidBrush(color);
    FillRect(dc, &rect, brush);
    DeleteObject(brush);
}

fn local_system_time(timestamp: i64) -> Option<SYSTEMTIME> {
    let ticks = timestamp
        .checked_add(11_644_473_600)?
        .checked_mul(10_000_000)?;
    if ticks < 0 {
        return None;
    }
    let filetime = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks as u64 >> 32) as u32,
    };
    unsafe {
        let mut utc: SYSTEMTIME = zeroed();
        let mut local: SYSTEMTIME = zeroed();
        if FileTimeToSystemTime(&filetime, &mut utc) == 0
            || SystemTimeToTzSpecificLocalTime(null(), &utc, &mut local) == 0
        {
            None
        } else {
            Some(local)
        }
    }
}

fn local_date(timestamp: i64) -> String {
    local_system_time(timestamp)
        .map(|t| {
            format!(
                "{:04}/{:02}/{:02} {:02}:{:02}",
                t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute
            )
        })
        .unwrap_or("时间未知".into())
}
fn local_time(timestamp: i64) -> String {
    local_system_time(timestamp)
        .map(|t| format!("{:02}:{:02}", t.wHour, t.wMinute))
        .unwrap_or("—".into())
}

unsafe fn make_icon() -> HICON {
    let screen = GetDC(null_mut());
    let dc = CreateCompatibleDC(screen);
    let bitmap = CreateCompatibleBitmap(screen, 32, 32);
    let old = SelectObject(dc, bitmap);
    fill(
        dc,
        RECT {
            left: 0,
            top: 0,
            right: 32,
            bottom: 32,
        },
        GREEN,
    );
    let font = CreateFontW(
        -27,
        0,
        0,
        0,
        600,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        0,
        0,
        ANTIALIASED_QUALITY as u32,
        0,
        wide("Segoe UI").as_ptr(),
    );
    let previous = SelectObject(dc, font);
    SetTextColor(dc, rgb(255, 255, 255));
    SetBkMode(dc, TRANSPARENT as i32);
    let mut rect = RECT {
        left: 0,
        top: -1,
        right: 32,
        bottom: 31,
    };
    DrawTextW(
        dc,
        wide("C").as_ptr(),
        -1,
        &mut rect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
    SelectObject(dc, previous);
    SelectObject(dc, old);
    let bits = [0u8; 128];
    let mask = CreateBitmap(32, 32, 1, 1, bits.as_ptr() as _);
    let info = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: bitmap,
    };
    let icon = CreateIconIndirect(&info);
    DeleteObject(font);
    DeleteObject(mask);
    DeleteObject(bitmap);
    DeleteDC(dc);
    ReleaseDC(null_mut(), screen);
    icon
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        let app = &mut *(create.lpCreateParams as *mut App);
        app.hwnd = hwnd;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    match message {
        WM_CREATE => {
            (*pointer).initialize();
            0
        }
        WM_PAINT => {
            (*pointer).paint();
            0
        }
        WM_ERASEBKGND => 1,
        WM_CTLCOLORSTATIC => {
            let dc = wparam as HDC;
            SetBkColor(dc, if (*pointer).glass { 0 } else { BG });
            SetTextColor(
                dc,
                GetWindowLongPtrW(lparam as HWND, GWLP_USERDATA) as u32 & 0xffffff,
            );
            (*pointer).brush as isize
        }
        WM_CTLCOLORLISTBOX => {
            SetBkColor(wparam as HDC, if (*pointer).glass { 0 } else { BG });
            (*pointer).brush as isize
        }
        WM_DRAWITEM => {
            (*pointer).draw_item(&*(lparam as *const DRAWITEMSTRUCT));
            1
        }
        WM_COMMAND => {
            (*pointer).action(wparam & 0xffff);
            0
        }
        WM_TIMER if wparam == 2 => {
            (*pointer).animate();
            0
        }
        WM_TIMER => {
            (*pointer).tick();
            0
        }
        WM_SYSCOMMAND if wparam & 0xfff0 == SC_MINIMIZE as usize => {
            (*pointer).action(HIDE);
            0
        }
        WM_CLOSE => {
            (*pointer).action(EXIT);
            0
        }
        HOVER => {
            if (*pointer).hovered != wparam {
                (*pointer).hovered = wparam;
                SetTimer(hwnd, 2, 15, None);
                (*pointer).animate();
            }
            0
        }
        WM_THEMECHANGED | WM_SETTINGCHANGE | WM_DWMCOMPOSITIONCHANGED => {
            let app = &mut *pointer;
            if app.theme != 0 {
                CloseThemeData(app.theme);
            }
            app.theme = OpenThemeData(hwnd, wide("WINDOW").as_ptr());
            app.material();
            app.layout();
            0
        }
        WM_DPICHANGED => {
            let app = &mut *pointer;
            app.dpi = (wparam & 0xffff) as i32;
            app.fonts();
            app.height = 0;
            let rect = &*(lparam as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                rect.left,
                rect.top,
                0,
                0,
                SWP_NOZORDER | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            app.layout();
            0
        }
        TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => (*pointer).action(SHOW),
                WM_RBUTTONUP | WM_CONTEXTMENU => (*pointer).menu(),
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            KillTimer(hwnd, 1);
            KillTimer(hwnd, 2);
            (*pointer).tray(NIM_DELETE);
            DestroyIcon((*pointer).icon);
            PostQuitMessage(0);
            0
        }
        _ if message == (*pointer).taskbar_created && message != 0 => {
            (*pointer).tray(NIM_ADD);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

pub fn run() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        BufferedPaintInit();
        let class = wide("CodexQuotaWidget");
        let existing = FindWindowW(class.as_ptr(), null());
        if !existing.is_null() {
            ShowWindow(existing, SW_RESTORE);
            SetForegroundWindow(existing);
            return;
        }
        let (request_tx, request_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            loop {
                if event_tx.send(Event::Loading).is_err() {
                    break;
                }
                let result = quota::fetch();
                if event_tx.send(Event::Complete(result)).is_err() {
                    break;
                }
                match request_rx.recv_timeout(Duration::from_secs(60)) {
                    Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    _ => {}
                }
            }
            let _ = event_tx.send(Event::Stopped);
        });
        let mut app = Box::new(App {
            hwnd: null_mut(),
            dpi: 96,
            brush: null_mut(),
            fonts: [null_mut(); 4],
            labels: Vec::new(),
            bars: Vec::new(),
            lines: Vec::new(),
            buttons: [null_mut(); 4],
            list: null_mut(),
            icon: null_mut(),
            snapshot: None,
            error: None,
            loading: true,
            pinned: false,
            expanded: false,
            closing: false,
            height: 0,
            tick: 0,
            taskbar_created: 0,
            glass: false,
            theme: 0,
            hovered: 0,
            hover: [0.0; 4],
            motion: true,
            requests: request_tx,
            events: event_rx,
        });
        let instance = GetModuleHandleW(null());
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            lpszClassName: class.as_ptr(),
            ..zeroed()
        };
        RegisterClassExW(&wc);
        let mut area: RECT = zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut _ as _, 0);
        let hwnd = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class.as_ptr(),
            wide("Codex 额度").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            area.right - 415,
            area.top + 80,
            380,
            540,
            null_mut(),
            null_mut(),
            instance,
            &mut *app as *mut App as _,
        );
        if hwnd.is_null() {
            let _ = app.requests.send(Request::Stop);
            MessageBoxW(
                null_mut(),
                wide("无法创建窗口，请重新启动程序。").as_ptr(),
                wide("Codex 额度").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        } else {
            ShowWindow(hwnd, SW_SHOW);
            let mut message = zeroed();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                if message.message == WM_KEYDOWN && message.wParam == 0x74 {
                    app.action(REFRESH);
                    continue;
                }
                if message.message == WM_KEYDOWN && message.wParam == 0x1b {
                    app.action(HIDE);
                    continue;
                }
                if IsDialogMessageW(hwnd, &message) == 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        let _ = app.requests.send(Request::Stop);
        let _ = worker.join();
        for font in app.fonts {
            DeleteObject(font);
        }
        DeleteObject(app.brush);
        if app.theme != 0 {
            CloseThemeData(app.theme);
        }
        BufferedPaintUnInit();
    }
}
