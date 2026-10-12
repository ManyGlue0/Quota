//! The window shell (shared with Anotes): one frameless, always-on-top window
//! whose *bounds* are the visible shape. The screen edge belongs to Holster:
//! closed, the window is hidden; Holster's dock (or the shortcut, or the tray)
//! opens it, and the panel unfolds out of the edge at the height of the tile
//! that was clicked.
//! Morphing animates the native window rect with springs, so the real DWM
//! acrylic, shadow and rounded corners follow every frame and there are never
//! transparent areas that could swallow clicks.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmFlush, DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::settings::Prefs;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub l: f64,
    pub t: f64,
    pub r: f64,
    pub b: f64,
}

impl Rect {
    fn arr(self) -> [f64; 4] {
        [self.l, self.t, self.r, self.b]
    }
    fn from_arr(a: [f64; 4]) -> Self {
        Self { l: a[0], t: a[1], r: a[2], b: a[3] }
    }
    fn h(&self) -> f64 {
        self.b - self.t
    }
    fn w(&self) -> f64 {
        self.r - self.l
    }
}

// ---- monitors -------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub name: String,
    pub primary: bool,
    #[serde(skip)]
    pub bounds: Rect,
    #[serde(skip)]
    pub work: Rect,
    pub scale: f64,
    pub width: i32,
    pub height: i32,
}

pub fn monitors() -> Vec<MonitorInfo> {
    unsafe extern "system" fn cb(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
        let out = &mut *(data as *mut Vec<MonitorInfo>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut info as *mut _ as *mut _) != 0 {
            let (mut dx, mut dy) = (96u32, 96u32);
            GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let r = |r: RECT| Rect { l: r.left as f64, t: r.top as f64, r: r.right as f64, b: r.bottom as f64 };
            let name_len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            let m = info.monitorInfo;
            out.push(MonitorInfo {
                name: String::from_utf16_lossy(&info.szDevice[..name_len]),
                primary: m.dwFlags & 1 != 0,
                bounds: r(m.rcMonitor),
                work: r(m.rcWork),
                scale: dx as f64 / 96.0,
                width: m.rcMonitor.right - m.rcMonitor.left,
                height: m.rcMonitor.bottom - m.rcMonitor.top,
            });
        }
        1
    }
    let mut out: Vec<MonitorInfo> = Vec::new();
    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(cb), &mut out as *mut _ as LPARAM);
    }
    out
}

// ---- spring animator --------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct Spring {
    /// Angular frequency (rad/s) and damping ratio, per edge [l, t, r, b].
    pub omega: [f64; 4],
    pub zeta: [f64; 4],
    /// Per-edge start delay in seconds, for staggered morphs.
    pub delay: [f64; 4],
}

impl Spring {
    pub const fn uniform(omega: f64, zeta: f64) -> Self {
        Self { omega: [omega; 4], zeta: [zeta; 4], delay: [0.0; 4] }
    }
}

type Callback = Box<dyn FnOnce() + Send>;

struct AnimState {
    cur: [f64; 4],
    vel: [f64; 4],
    target: [f64; 4],
    spring: Spring,
    elapsed: f64,
    running: bool,
    near: Option<Callback>,
    done: Option<Callback>,
}

pub struct Animator {
    hwnd: isize,
    st: Mutex<AnimState>,
    wake: Condvar,
    applied: Mutex<[i32; 4]>,
}

impl Animator {
    fn new(hwnd: isize, initial: Rect) -> Arc<Self> {
        let me = Arc::new(Self {
            hwnd,
            st: Mutex::new(AnimState {
                cur: initial.arr(),
                vel: [0.0; 4],
                target: initial.arr(),
                spring: Spring::uniform(20.0, 1.0),
                elapsed: 0.0,
                running: false,
                near: None,
                done: None,
            }),
            wake: Condvar::new(),
            applied: Mutex::new([i32::MIN; 4]),
        });
        let worker = me.clone();
        std::thread::Builder::new().name("quota-animator".into()).spawn(move || worker.run()).unwrap();
        me
    }

    /// Animates towards `target`, keeping current velocity (smooth retargeting).
    pub fn animate(&self, target: Rect, spring: Spring, near: Option<Callback>, done: Option<Callback>) {
        let mut st = self.st.lock().unwrap();
        st.target = target.arr();
        st.spring = spring;
        st.elapsed = 0.0;
        st.near = near;
        st.done = done;
        st.running = true;
        self.wake.notify_one();
    }

    /// Jumps immediately (drags, resizes, display changes).
    pub fn jump(&self, target: Rect) {
        {
            let mut st = self.st.lock().unwrap();
            st.cur = target.arr();
            st.target = target.arr();
            st.vel = [0.0; 4];
            st.running = false;
            st.near = None;
            st.done = None;
        }
        self.apply(target);
    }

    fn apply(&self, r: Rect) {
        let px = [r.l.round() as i32, r.t.round() as i32, r.r.round() as i32, r.b.round() as i32];
        {
            let mut last = self.applied.lock().unwrap();
            if *last == px {
                return;
            }
            *last = px;
        }
        unsafe {
            SetWindowPos(
                self.hwnd as HWND,
                std::ptr::null_mut(),
                px[0],
                px[1],
                (px[2] - px[0]).max(1),
                (px[3] - px[1]).max(1),
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }

    fn run(&self) {
        let mut last = Instant::now();
        loop {
            {
                let mut st = self.st.lock().unwrap();
                while !st.running {
                    st = self.wake.wait(st).unwrap();
                    last = Instant::now();
                }
            }
            // Pace on the compositor's frame clock.
            if unsafe { DwmFlush() } != 0 {
                std::thread::sleep(Duration::from_millis(8));
            }
            let now = Instant::now();
            let dt = now.duration_since(last).as_secs_f64().min(1.0 / 30.0);
            last = now;

            let (rect, near, done) = {
                let mut st = self.st.lock().unwrap();
                if !st.running {
                    continue;
                }
                st.elapsed += dt;
                let steps = (dt / 0.002).ceil().max(1.0) as usize;
                let h = dt / steps as f64;
                let mut settled = true;
                let mut max_dist: f64 = 0.0;
                for i in 0..4 {
                    if st.elapsed < st.spring.delay[i] {
                        settled &= (st.cur[i] - st.target[i]).abs() < 0.25;
                        max_dist = max_dist.max((st.cur[i] - st.target[i]).abs());
                        continue;
                    }
                    let w = st.spring.omega[i];
                    let k = w * w;
                    let c = 2.0 * st.spring.zeta[i] * w;
                    for _ in 0..steps {
                        let a = -k * (st.cur[i] - st.target[i]) - c * st.vel[i];
                        st.vel[i] += a * h;
                        st.cur[i] += st.vel[i] * h;
                    }
                    let d = (st.cur[i] - st.target[i]).abs();
                    max_dist = max_dist.max(d);
                    settled &= d < 0.25 && st.vel[i].abs() < 4.0;
                }
                let near = if max_dist < 6.0 { st.near.take() } else { None };
                let mut done = None;
                if settled {
                    st.cur = st.target;
                    st.vel = [0.0; 4];
                    st.running = false;
                    done = st.done.take();
                }
                (Rect::from_arr(st.cur), near, done)
            };
            self.apply(rect);
            if let Some(f) = near {
                f();
            }
            if let Some(f) = done {
                f();
            }
        }
    }
}

// ---- shell ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Hidden. Holster's dock, the shortcut or the tray icon opens it.
    Closed,
    Opening,
    Open,
    Closing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellState {
    pub mode: Mode,
    pub scale: f64,
    /// Final panel size in CSS pixels, so the UI can lay out once and let the
    /// window reveal it instead of reflowing on every animation frame.
    pub panel_width: f64,
    pub panel_height: f64,
}

struct Geometry {
    /// Where the panel unfolds from and folds back into: a sliver tucked into
    /// the screen edge, centered on the Holster tile that opened it.
    seed: Rect,
    panel: Rect,
    scale: f64,
}

/// Where the panel lives: the monitor Holster's dock was clicked on and the
/// vertical center of the clicked tile (physical px). Kept after closing, so
/// the shortcut and the tray open it in the same place.
#[derive(Default)]
struct Place {
    monitor: Option<String>,
    anchor_y: Option<f64>,
    /// While open: the panel's top edge (physical px), kept as content grows.
    top: Option<f64>,
}

struct State {
    mode: Mode,
    /// Height of the UI's content (CSS px): the panel is as tall as that.
    content_h: f64,
    prev_foreground: isize,
    opened_at: Instant,
    blur_suppressed: bool,
}

pub struct Shell {
    app: AppHandle,
    hwnd: isize,
    prefs: Arc<Prefs>,
    anim: Arc<Animator>,
    state: Mutex<State>,
    /// Locked after `state` when both are needed, never before.
    place: Mutex<Place>,
}

static CLOSED: AtomicBool = AtomicBool::new(true);
static SHELL_HWND: AtomicIsize = AtomicIsize::new(0);
static SHELL: OnceLock<Arc<Shell>> = OnceLock::new();

const OPEN_SPRING: Spring = Spring {
    // Width springs out from the edge first (slight overshoot), then height follows.
    omega: [21.0, 17.0, 21.0, 17.0],
    zeta: [0.78, 0.9, 0.9, 0.9],
    delay: [0.0, 0.035, 0.0, 0.035],
};
/// Height changes while open (a card expanding, a page switch).
const GLIDE_SPRING: Spring = Spring::uniform(22.0, 0.92);
/// The panel's width (CSS px) and its smallest height.
const PANEL_WIDTH: f64 = 340.0;
const MIN_HEIGHT: f64 = 160.0;
/// A popover, not a wall: taller content scrolls inside.
const MAX_HEIGHT: f64 = 720.0;
const CLOSE_SPRING: Spring = Spring {
    omega: [24.0, 26.0, 24.0, 26.0],
    zeta: [1.0, 1.0, 1.0, 1.0],
    delay: [0.03, 0.0, 0.03, 0.0],
};
/// Where the panel unfolds from before Holster has told us (fraction of the
/// work-area height): Holster's own default tab position.
const DEFAULT_ANCHOR: f64 = 0.42;

fn ex_style(hwnd: isize, set: u32, clear: u32) {
    unsafe {
        let cur = GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE) as u32;
        SetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE, ((cur | set) & !clear) as isize);
    }
}

impl Shell {
    pub fn install(app: &AppHandle, prefs: Arc<Prefs>) -> Arc<Shell> {
        let window = app.get_webview_window("main").expect("main window");
        let hwnd = window.hwnd().expect("hwnd").0 as isize;
        SHELL_HWND.store(hwnd, Ordering::SeqCst);
        unsafe {
            let pref = DWMWCP_ROUND;
            DwmSetWindowAttribute(
                hwnd as HWND,
                DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                &pref as *const _ as *const _,
                std::mem::size_of_val(&pref) as u32,
            );
            SetWindowSubclass(hwnd as HWND, Some(subclass_proc), 1, 0);
        }
        // Closed: never take focus, never appear in Alt+Tab.
        ex_style(hwnd, WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW, WS_EX_APPWINDOW);

        let placeholder = Rect { l: 0.0, t: 0.0, r: 1.0, b: 1.0 };
        let shell = Arc::new(Shell {
            app: app.clone(),
            hwnd,
            prefs,
            anim: Animator::new(hwnd, placeholder),
            state: Mutex::new(State {
                mode: Mode::Closed,
                content_h: 480.0,
                prev_foreground: 0,
                opened_at: Instant::now(),
                blur_suppressed: false,
            }),
            place: Mutex::new(Place::default()),
        });
        let _ = SHELL.set(shell.clone());
        // Stays hidden until something opens it: the edge belongs to Holster.
        shell.anim.jump(shell.geometry().seed);
        shell
    }

    fn geometry(&self) -> Geometry {
        let (monitor, anchor_y) = {
            let p = self.place.lock().unwrap();
            (p.monitor.clone(), p.anchor_y)
        };
        let mons = monitors();
        let mon = monitor
            .as_ref()
            .and_then(|n| mons.iter().find(|m| &m.name == n))
            .or_else(|| mons.iter().find(|m| m.primary))
            .or(mons.first())
            .cloned()
            .unwrap_or(MonitorInfo {
                name: String::new(),
                primary: true,
                bounds: Rect { l: 0.0, t: 0.0, r: 1920.0, b: 1080.0 },
                work: Rect { l: 0.0, t: 0.0, r: 1920.0, b: 1040.0 },
                scale: 1.0,
                width: 1920,
                height: 1080,
            });
        let s = mon.scale;
        let work = mon.work;
        let edge = work.r;
        // Tuck the seed past the screen edge so the shape grows out of it,
        // unless another monitor (or a right-side taskbar) is there.
        let neighbour_right = mons.iter().any(|m| {
            m.name != mon.name && m.bounds.l >= mon.bounds.r - 1.0 && m.bounds.t < mon.bounds.b && m.bounds.b > mon.bounds.t
        });
        let overhang = if neighbour_right || work.r < mon.bounds.r { 0.0 } else { (10.0 * s).round() };
        let (th, margin) = ((88.0 * s).round(), (8.0 * s).round());
        let cy = anchor_y
            .unwrap_or(work.t + DEFAULT_ANCHOR * work.h())
            .clamp(work.t + th / 2.0 + margin, (work.b - th / 2.0 - margin).max(work.t + th / 2.0 + margin));
        let top = (cy - th / 2.0).round();
        let seed = Rect { l: edge + overhang - 1.0, t: top, r: edge + overhang, b: top + th };

        // A compact column, as tall as its content, unfolding beside the tile:
        // centered on it at first, then keeping its top while it grows.
        let pw = (PANEL_WIDTH * s).round();
        let (gap, vm) = ((10.0 * s).round(), (12.0 * s).round());
        let content_h = self.state.lock().unwrap().content_h;
        let ph = (content_h.min(MAX_HEIGHT) * s).round().min(work.h() - 2.0 * vm);
        let wanted_top = self.place.lock().unwrap().top.unwrap_or(cy - ph / 2.0);
        let pt = wanted_top.min(work.b - vm - ph).max(work.t + vm).round();
        let panel = Rect { l: edge - gap - pw, t: pt, r: edge - gap, b: pt + ph };
        Geometry { seed, panel, scale: s }
    }

    fn emit_state(&self) {
        let _ = self.app.emit("shell", self.snapshot());
    }

    pub fn snapshot(&self) -> ShellState {
        let mode = self.state.lock().unwrap().mode;
        let g = self.geometry();
        ShellState {
            mode,
            scale: g.scale,
            panel_width: g.panel.w() / g.scale,
            panel_height: g.panel.h() / g.scale,
        }
    }

    pub fn mode(&self) -> Mode {
        self.state.lock().unwrap().mode
    }

    /// Holster's dock asked to open (a click on the Quota tile). The panel
    /// unfolds on the monitor that was clicked, out of the tile's height.
    pub fn open_from_dock(self: &Arc<Self>, anchor_y: Option<f64>) {
        if self.mode() == Mode::Closed {
            let (cx, cy) = Self::cursor();
            let mons = monitors();
            let hit = |x: f64, y: f64| {
                mons.iter().find(|m| x >= m.bounds.l && x < m.bounds.r && y >= m.bounds.t && y < m.bounds.b)
            };
            let monitor = hit(cx, anchor_y.unwrap_or(cy)).or_else(|| hit(cx, cy)).map(|m| m.name.clone());
            let mut p = self.place.lock().unwrap();
            if monitor.is_some() {
                p.monitor = monitor;
            }
            if anchor_y.is_some() {
                p.anchor_y = anchor_y;
            }
        }
        self.open();
    }

    /// Unfolds the panel out of the edge and takes focus.
    pub fn open(self: &Arc<Self>) {
        let from_hidden = {
            let mut st = self.state.lock().unwrap();
            if matches!(st.mode, Mode::Open | Mode::Opening) {
                drop(st);
                // Already out: bring it forward (a click on the dock tile).
                unsafe { PostMessageW(self.hwnd as HWND, WM_TAKE_FOCUS, 0, 0) };
                return;
            }
            let from_hidden = st.mode == Mode::Closed;
            st.mode = Mode::Opening;
            st.opened_at = Instant::now();
            let fg = unsafe { GetForegroundWindow() } as isize;
            if fg != self.hwnd {
                st.prev_foreground = fg;
            }
            from_hidden
        };
        CLOSED.store(false, Ordering::SeqCst);
        ex_style(self.hwnd, 0, WS_EX_NOACTIVATE);
        let g = self.geometry();
        if from_hidden {
            self.anim.jump(g.seed);
        }
        self.place.lock().unwrap().top = Some(g.panel.t);
        self.show_window();
        self.emit_state();
        // Focus has to be taken on the window's own thread (see WM_TAKE_FOCUS):
        // a plain SetForegroundWindow is refused while another app owns the
        // foreground, and without focus there is no blur to close on.
        unsafe { PostMessageW(self.hwnd as HWND, WM_TAKE_FOCUS, 0, 0) };
        self.anim.animate(g.panel, OPEN_SPRING, Some(self.finish_opening()), None);
    }

    /// Completion of the open morph: flips Opening to Open, which reveals the
    /// content. Every animation that retargets an opening panel must carry it,
    /// or the panel stays invisible and click-through.
    fn finish_opening(self: &Arc<Self>) -> Callback {
        let me = self.clone();
        Box::new(move || {
            let mut st = me.state.lock().unwrap();
            if st.mode == Mode::Opening {
                st.mode = Mode::Open;
                drop(st);
                me.emit_state();
            }
        })
    }

    pub fn close(self: &Arc<Self>) {
        {
            let mut st = self.state.lock().unwrap();
            if matches!(st.mode, Mode::Closed | Mode::Closing) {
                return;
            }
            st.mode = Mode::Closing;
        }
        self.emit_state();
        let me = self.clone();
        // Let the content fade out before the shape folds back into the edge.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(90));
            if me.mode() != Mode::Closing {
                return;
            }
            let prev = me.state.lock().unwrap().prev_foreground;
            // Hand focus back to where it came from (a game, an editor). Done on
            // the window thread, like taking it, so Windows accepts the switch.
            unsafe { SendMessageW(me.hwnd as HWND, WM_RESTORE_FOCUS, prev as WPARAM, 0) };
            ex_style(me.hwnd, WS_EX_NOACTIVATE, 0);
            CLOSED.store(true, Ordering::SeqCst);
            let g = me.geometry();
            let me2 = me.clone();
            me.anim.animate(
                g.seed,
                CLOSE_SPRING,
                None,
                Some(Box::new(move || {
                    let mut st = me2.state.lock().unwrap();
                    if st.mode == Mode::Closing {
                        st.mode = Mode::Closed;
                        me2.place.lock().unwrap().top = None;
                        drop(st);
                        me2.hide_window();
                        me2.emit_state();
                    }
                })),
            );
        });
    }

    pub fn toggle(self: &Arc<Self>) {
        match self.mode() {
            Mode::Closed | Mode::Closing => self.open(),
            Mode::Open | Mode::Opening => self.close(),
        }
    }

    fn cursor() -> (f64, f64) {
        let mut p = POINT { x: 0, y: 0 };
        unsafe { GetCursorPos(&mut p) };
        (p.x as f64, p.y as f64)
    }

    /// The UI measured its content (CSS px): the panel follows it, keeping its
    /// top edge where it is unless the new height would run off the screen.
    pub fn set_content_height(self: &Arc<Self>, height: f64) {
        let mode = {
            let mut st = self.state.lock().unwrap();
            let h = height.clamp(MIN_HEIGHT, 4000.0).round();
            if (st.content_h - h).abs() < 0.5 {
                return;
            }
            st.content_h = h;
            st.mode
        };
        let g = self.geometry();
        match mode {
            Mode::Opening => self.anim.animate(g.panel, OPEN_SPRING, Some(self.finish_opening()), None),
            Mode::Open => self.anim.animate(g.panel, GLIDE_SPRING, None, None),
            _ => {}
        }
        self.emit_state();
    }

    /// Re-applies geometry after display, DPI, work-area or settings changes.
    pub fn relayout(&self) {
        let mode = self.mode();
        if matches!(mode, Mode::Open | Mode::Opening) {
            self.anim.jump(self.geometry().panel);
        }
        // A jump drops the open morph's completion; the panel is in place now.
        if mode == Mode::Opening {
            let mut st = self.state.lock().unwrap();
            if st.mode == Mode::Opening {
                st.mode = Mode::Open;
            }
        }
        self.emit_state();
    }

    fn window_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd as HWND) != 0 }
    }

    fn show_window(&self) {
        if !self.window_visible() {
            unsafe {
                ShowWindow(self.hwnd as HWND, SW_SHOWNOACTIVATE);
                SetWindowPos(self.hwnd as HWND, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    fn hide_window(&self) {
        unsafe { ShowWindow(self.hwnd as HWND, SW_HIDE) };
    }

    pub fn suppress_blur(&self, on: bool) {
        self.state.lock().unwrap().blur_suppressed = on;
    }

    fn may_close_on_blur(&self) -> bool {
        let st = self.state.lock().unwrap();
        matches!(st.mode, Mode::Open | Mode::Opening)
            && !st.blur_suppressed
            && st.opened_at.elapsed() > Duration::from_millis(350)
            && self.prefs.get().click_outside_closes
    }

    /// Click outside: after a short debounce (focus can flicker while Windows
    /// shuffles activation), ask the UI to close. The UI has the final say,
    /// since it knows about the pin, open menus, sheets and IME composition.
    pub fn on_blur(self: &Arc<Self>) {
        if !self.may_close_on_blur() {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let refocused = unsafe { GetForegroundWindow() } as isize == me.hwnd;
            if !refocused && me.may_close_on_blur() {
                let _ = me.app.emit("shell-blur", ());
            }
        });
    }
}

/// Brings the window to the foreground and focuses the webview. Called on the
/// window's thread: attaching to the current foreground thread's input lets
/// Windows accept the switch even when another app is in front.
unsafe fn take_focus(hwnd: HWND) {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    let fg = GetForegroundWindow();
    if fg != hwnd {
        let me = GetCurrentThreadId();
        let other = if fg.is_null() { 0 } else { GetWindowThreadProcessId(fg, std::ptr::null_mut()) };
        let attached = other != 0 && other != me && AttachThreadInput(me, other, 1) != 0;
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if attached {
            AttachThreadInput(me, other, 0);
        }
    }
    if let Some(w) = SHELL.get().and_then(|s| s.app.get_webview_window("main")) {
        let _ = w.set_focus();
    }
}

const WM_RELAYOUT: u32 = WM_APP + 7;
const WM_TAKE_FOCUS: u32 = WM_APP + 8;
const WM_RESTORE_FOCUS: u32 = WM_APP + 9;

/// Gives the foreground back to `prev` if Quota still has it.
unsafe fn restore_focus(hwnd: HWND, prev: HWND) {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    if prev.is_null() || IsWindow(prev) == 0 || GetForegroundWindow() != hwnd {
        return;
    }
    let me = GetCurrentThreadId();
    let other = GetWindowThreadProcessId(prev, std::ptr::null_mut());
    let attached = other != 0 && other != me && AttachThreadInput(me, other, 1) != 0;
    SetForegroundWindow(prev);
    if attached {
        AttachThreadInput(me, other, 0);
    }
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        // Keep the acrylic backdrop in its "active" look even when unfocused,
        // so the panel stays frosted rather than a flat fallback color.
        WM_NCACTIVATE => return DefSubclassProc(hwnd, msg, 1, lparam),
        WM_MOUSEACTIVATE if CLOSED.load(Ordering::SeqCst) => return MA_NOACTIVATE as LRESULT,
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            if msg != WM_SETTINGCHANGE || wparam as u32 == SPI_SETWORKAREA {
                PostMessageW(hwnd, WM_RELAYOUT, 0, 0);
            }
        }
        WM_RELAYOUT => {
            if let Some(shell) = SHELL.get() {
                let shell = shell.clone();
                // Monitor info settles a moment after the change notification.
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(120));
                    shell.relayout();
                });
            }
            return 0;
        }
        WM_RESTORE_FOCUS => {
            restore_focus(hwnd, wparam as HWND);
            return 0;
        }
        WM_TAKE_FOCUS => {
            if !CLOSED.load(Ordering::SeqCst) {
                take_focus(hwnd);
            }
            return 0;
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

/// Frosted acrylic via the window accent policy. Unlike the DWM system
/// backdrop (which falls back to a flat color whenever the window is not
/// active), this stays frosted while the panel is pinned and unfocused.
pub fn apply_material(hwnd: isize, dark: bool) -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    #[repr(C)]
    struct AccentPolicy {
        state: u32,
        flags: u32,
        gradient: u32,
        animation: u32,
    }
    #[repr(C)]
    struct WinCompAttrData {
        attrib: u32,
        data: *mut std::ffi::c_void,
        size: usize,
    }
    type SetWca = unsafe extern "system" fn(HWND, *mut WinCompAttrData) -> i32;
    unsafe {
        // Make sure the DWM system backdrop is off so the two don't fight.
        let none: u32 = 1; // DWMSBT_NONE
        DwmSetWindowAttribute(hwnd as HWND, 38, &none as *const _ as *const _, 4);
        let user32: Vec<u16> = "user32.dll\0".encode_utf16().collect();
        let module = GetModuleHandleW(user32.as_ptr());
        let Some(proc) = GetProcAddress(module, c"SetWindowCompositionAttribute".as_ptr() as *const u8) else {
            return false;
        };
        let set: SetWca = std::mem::transmute(proc);
        // Gradient is ABGR: a soft tint that keeps the blur legible.
        let gradient = if dark { 0x5C201C1Cu32 } else { 0x66F4F2F2u32 };
        let mut policy = AccentPolicy { state: 4 /* ACRYLICBLURBEHIND */, flags: 2, gradient, animation: 0 };
        let mut data = WinCompAttrData {
            attrib: 19, // WCA_ACCENT_POLICY
            data: &mut policy as *mut _ as *mut _,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        set(hwnd as HWND, &mut data) != 0
    }
}
