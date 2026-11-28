// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The window. Servo draws three views into offscreen surfaces of the window's GL context,
//! and each frame copies them into place:
//!
//! 1. **the interface** (`ui.eon.invalid`), the whole window: sidebar, bar, panels;
//! 2. **the page**, into the rectangle the interface leaves for it (it says where);
//! 3. **the overlay** — address suggestions, the command bar — above the page, only while
//!    it is open, so the page is never resized for it.
//!
//! Input goes to whichever view is under the pointer, keys to whichever has focus, and the
//! browser's own shortcuts are taken first.

use std::cell::Cell;
use std::rc::Rc;

use euclid::default::{Point2D, Rect, Size2D};
use servo::{
    Cursor, DevicePoint, EventLoopWaker, InputEvent, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, OffscreenRenderingContext, RenderingContext, WebView,
    WebViewPoint, WheelDelta, WheelEvent, WheelMode, WindowRenderingContext,
};
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{CursorIcon, Window, WindowId};

use crate::browser::{Area, Browser, Host, Options};
use crate::input;

/// Where the page sits before the interface has said (CSS px): the sidebar's width and
/// the bar's height of the default layout.
const DEFAULT_PAGE: Area = Area {
    x: 240.0,
    y: 48.0,
    w: 0.0,
    h: 0.0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Chrome,
    Page,
    Overlay,
}

#[derive(Debug)]
struct Wake;

#[derive(Clone)]
struct Waker(EventLoopProxy<Wake>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        let _ = self.0.send_event(Wake);
    }
}

/// What the browser asks of the window, kept until the next event is handled.
struct Shared {
    window: Rc<Window>,
    /// The page's rectangle, CSS px; `w`/`h` of 0 mean "to the window's edge".
    page: Cell<Area>,
    overlay: Cell<Option<Area>>,
    relayout: Cell<bool>,
    focus: Cell<Option<Focus>>,
    private_window: Cell<bool>,
}

struct WindowHost(Rc<Shared>);

impl Host for WindowHost {
    fn redraw(&self) {
        self.0.window.request_redraw();
    }
    fn set_cursor(&self, cursor: Cursor) {
        self.0.window.set_cursor(cursor_icon(cursor));
    }
    fn set_title(&self, title: &str) {
        self.0.window.set_title(title);
    }
    fn set_layout(&self, page: Area, overlay: Option<Area>) {
        if self.0.page.replace(page) != page || self.0.overlay.replace(overlay) != overlay {
            self.0.relayout.set(true);
            self.0.window.request_redraw();
        }
    }
    fn focus_page(&self) {
        self.0.focus.set(Some(Focus::Page));
    }
    fn focus_overlay(&self) {
        self.0.focus.set(Some(Focus::Overlay));
    }
    fn focus_chrome(&self) {
        self.0.focus.set(Some(Focus::Chrome));
    }
    fn open_private_window(&self) {
        self.0.private_window.set(true);
    }
    fn relayout(&self) {
        self.0.relayout.set(true);
    }
}

/// A rectangle in physical pixels, from the top left.
#[derive(Clone, Copy)]
struct Px {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Px {
    fn contains(self, p: PhysicalPosition<f64>) -> bool {
        p.x >= f64::from(self.x)
            && p.y >= f64::from(self.y)
            && p.x < f64::from(self.x + self.w)
            && p.y < f64::from(self.y + self.h)
    }
    fn size(self) -> PhysicalSize<u32> {
        PhysicalSize::new(self.w.max(1) as u32, self.h.max(1) as u32)
    }
}

struct Running {
    shared: Rc<Shared>,
    ctx: Rc<WindowRenderingContext>,
    ui_ctx: Rc<OffscreenRenderingContext>,
    page_ctx: Rc<OffscreenRenderingContext>,
    overlay_ctx: Rc<OffscreenRenderingContext>,
    browser: Rc<Browser>,
    cursor: Cell<PhysicalPosition<f64>>,
    mods: Cell<ModifiersState>,
    focus: Cell<Focus>,
}

impl Running {
    fn to_px(&self, area: Area) -> Px {
        let scale = self.shared.window.scale_factor();
        let size = self.shared.window.inner_size();
        let (win_w, win_h) = (size.width as i32, size.height as i32);
        let px = |v: f64| (v * scale).round() as i32;
        let left = px(area.x);
        let top = px(area.y);
        let width = if area.w > 0.0 {
            px(area.w)
        } else {
            win_w - left
        };
        let height = if area.h > 0.0 {
            px(area.h)
        } else {
            win_h - top
        };
        Px {
            x: left.clamp(0, win_w),
            y: top.clamp(0, win_h),
            w: width.clamp(1, win_w),
            h: height.clamp(1, win_h),
        }
    }

    fn page_px(&self) -> Px {
        self.to_px(self.shared.page.get())
    }

    fn overlay_px(&self) -> Option<Px> {
        self.shared.overlay.get().map(|a| self.to_px(a))
    }

    fn layout(&self) {
        let size = self.shared.window.inner_size();
        self.ctx.resize(size);
        if let Some(v) = self.browser.chrome_view() {
            v.resize(PhysicalSize::new(size.width.max(1), size.height.max(1)));
        }
        if let Some(v) = self.browser.active_view() {
            v.resize(self.page_px().size());
        }
        if let (Some(v), Some(o)) = (self.browser.overlay_view(), self.overlay_px()) {
            v.resize(o.size());
        }
        self.shared.window.request_redraw();
    }

    fn paint(&self) {
        if self.ctx.make_current().is_err() {
            return;
        }
        let overlay = self.overlay_px();
        if let Some(v) = self.browser.chrome_view() {
            v.paint();
        }
        if let Some(v) = self.browser.active_view() {
            v.paint();
        }
        if overlay.is_some() {
            if let Some(v) = self.browser.overlay_view() {
                v.paint();
            }
        }
        self.ctx.prepare_for_rendering();
        let gl = self.ctx.glow_gl_api();
        let size = self.shared.window.inner_size();
        let wh = size.height as i32;
        // GL counts from the bottom.
        let gl_rect = |p: Px| Rect::new(Point2D::new(p.x, wh - p.y - p.h), Size2D::new(p.w, p.h));
        if let Some(blit) = self.ui_ctx.render_to_parent_callback() {
            blit(
                &gl,
                Rect::new(Point2D::new(0, 0), Size2D::new(size.width as i32, wh)),
            );
        }
        if let Some(blit) = self.page_ctx.render_to_parent_callback() {
            blit(&gl, gl_rect(self.page_px()));
        }
        if let (Some(o), Some(blit)) = (overlay, self.overlay_ctx.render_to_parent_callback()) {
            blit(&gl, gl_rect(o));
        }
        self.ctx.present();
    }

    /// The view under the pointer and the point inside it.
    fn target(&self) -> Option<(WebView, Focus, DevicePoint)> {
        let p = self.cursor.get();
        let inside =
            |r: Px| DevicePoint::new((p.x - f64::from(r.x)) as f32, (p.y - f64::from(r.y)) as f32);
        if let Some(o) = self.overlay_px().filter(|o| o.contains(p)) {
            return Some((self.browser.overlay_view()?, Focus::Overlay, inside(o)));
        }
        let page = self.page_px();
        if page.contains(p) {
            return Some((self.browser.active_view()?, Focus::Page, inside(page)));
        }
        let v = self.browser.chrome_view()?;
        Some((v, Focus::Chrome, DevicePoint::new(p.x as f32, p.y as f32)))
    }

    fn set_focus(&self, f: Focus) {
        self.focus.set(f);
        if let Some(ui) = self.browser.chrome_view() {
            ui.set_focused(f == Focus::Chrome);
        }
        if let Some(page) = self.browser.active_view() {
            page.set_focused(f == Focus::Page);
        }
        if let Some(o) = self.browser.overlay_view() {
            o.set_focused(f == Focus::Overlay);
        }
    }

    fn focused_view(&self) -> Option<WebView> {
        match self.focus.get() {
            Focus::Chrome => self.browser.chrome_view(),
            Focus::Page => self.browser.active_view(),
            Focus::Overlay => self.browser.overlay_view(),
        }
    }

    fn after_event(&self) {
        self.browser.servo().spin_event_loop();
        self.browser.flush_ui();
        if self.shared.relayout.replace(false) {
            self.layout();
        }
        if let Some(f) = self.shared.focus.take() {
            self.set_focus(f);
        }
        if self.shared.private_window.replace(false) {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).arg("--private").spawn();
            }
        }
    }
}

struct App {
    proxy: EventLoopProxy<Wake>,
    private: bool,
    start: Option<Url>,
    running: Option<Running>,
    failed: Option<String>,
}

impl App {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Running, String> {
        let attrs = Window::default_attributes()
            .with_title(if self.private {
                "EON Browser (private)"
            } else {
                "EON Browser"
            })
            .with_inner_size(LogicalSize::new(1360.0, 860.0))
            .with_min_inner_size(LogicalSize::new(560.0, 360.0));
        let window = Rc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        let display = event_loop.display_handle().map_err(|e| e.to_string())?;
        let handle = window.window_handle().map_err(|e| e.to_string())?;
        let size = window.inner_size();
        let ctx = Rc::new(
            WindowRenderingContext::new(display, handle, size).map_err(|e| format!("{e:?}"))?,
        );
        ctx.make_current().map_err(|e| format!("{e:?}"))?;

        let data = crate::paths::data_dir();
        let tor_dir = if self.private {
            std::env::temp_dir().join(format!("eon-browser-tor-{}", std::process::id()))
        } else {
            data.join("tor")
        };
        let route = crate::route_client::RouteClient::start(&tor_dir).map(std::sync::Arc::new);
        let servo = crate::engine::builder(
            self.private,
            &data,
            route.as_ref().map(|r| r.proxy_uri()),
            Box::new(Waker(self.proxy.clone())),
        )
        .build();
        // Servo's log, on stderr, only when asked for (`RUST_LOG=servo_net=debug`): for
        // diagnosing a site, never on by default.
        if std::env::var_os("RUST_LOG").is_some() {
            servo.setup_logging();
        }
        let scale = window.scale_factor();
        let full = PhysicalSize::new(size.width.max(1), size.height.max(1));
        let ui_ctx = Rc::new(ctx.offscreen_context(full));
        let page_ctx = Rc::new(ctx.offscreen_context(full));
        let overlay_ctx = Rc::new(ctx.offscreen_context(PhysicalSize::new(640, 420)));

        let shared = Rc::new(Shared {
            window: window.clone(),
            page: Cell::new(DEFAULT_PAGE),
            overlay: Cell::new(None),
            relayout: Cell::new(true),
            focus: Cell::new(None),
            private_window: Cell::new(false),
        });
        let browser = Browser::new(
            servo,
            page_ctx.clone(),
            scale as f32,
            Box::new(WindowHost(shared.clone())),
            Options {
                private: self.private,
                data,
                fixtures: None,
                rules: None,
                route,
            },
        );
        browser.restore_route();
        browser.attach_chrome(ui_ctx.clone());
        browser.attach_overlay(overlay_ctx.clone());
        browser.open_start(self.start.take());
        let running = Running {
            shared,
            ctx,
            ui_ctx,
            page_ctx,
            overlay_ctx,
            browser,
            cursor: Cell::new(PhysicalPosition::new(0.0, 0.0)),
            mods: Cell::new(ModifiersState::empty()),
            focus: Cell::new(Focus::Page),
        };
        running.set_focus(Focus::Page);
        running.layout();
        Ok(running)
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.running.is_some() {
            return;
        }
        match self.start(event_loop) {
            Ok(r) => self.running = Some(r),
            Err(e) => {
                self.failed = Some(e);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: Wake) {
        if let Some(r) = &self.running {
            r.after_event();
        }
    }

    #[allow(clippy::too_many_lines)] // one match over the window's events
    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(r) = &self.running else { return };
        match event {
            WindowEvent::CloseRequested => {
                r.browser.shutdown();
                event_loop.exit();
                return;
            }
            WindowEvent::RedrawRequested => r.paint(),
            WindowEvent::Resized(_) => r.layout(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                for v in [
                    r.browser.chrome_view(),
                    r.browser.active_view(),
                    r.browser.overlay_view(),
                ]
                .into_iter()
                .flatten()
                {
                    v.set_hidpi_scale_factor(euclid::Scale::new(scale_factor as f32));
                }
                r.layout();
            }
            WindowEvent::ModifiersChanged(m) => r.mods.set(m.state()),
            WindowEvent::CursorMoved { position, .. } => {
                r.cursor.set(position);
                if let Some((v, _, p)) = r.target() {
                    v.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                        WebViewPoint::Device(p),
                    )));
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some((v, focus, p)) = r.target() {
                    let action = match state {
                        ElementState::Pressed => MouseButtonAction::Down,
                        ElementState::Released => MouseButtonAction::Up,
                    };
                    if action == MouseButtonAction::Down && r.focus.get() != focus {
                        // A click outside the overlay closes it, as a menu would.
                        if r.focus.get() == Focus::Overlay {
                            r.browser.close_overlay();
                        }
                        r.set_focus(focus);
                    }
                    let button = match button {
                        winit::event::MouseButton::Left => MouseButton::Primary,
                        winit::event::MouseButton::Right => MouseButton::Secondary,
                        winit::event::MouseButton::Middle => MouseButton::Auxiliary,
                        winit::event::MouseButton::Back => MouseButton::Back,
                        winit::event::MouseButton::Forward => MouseButton::Forward,
                        winit::event::MouseButton::Other(n) => MouseButton::Other(n),
                    };
                    // The mouse's back and forward buttons are the browser's.
                    if action == MouseButtonAction::Down && focus == Focus::Page {
                        match button {
                            MouseButton::Back => r.browser.shortcut(input::Shortcut::Back),
                            MouseButton::Forward => r.browser.shortcut(input::Shortcut::Forward),
                            _ => {}
                        }
                    }
                    v.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                        action,
                        button,
                        WebViewPoint::Device(p),
                    )));
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some((v, _, p)) = r.target() {
                    let (x, y, mode) = match delta {
                        MouseScrollDelta::LineDelta(dx, dy) => (
                            f64::from(dx) * 76.0,
                            f64::from(dy) * 76.0,
                            WheelMode::DeltaLine,
                        ),
                        MouseScrollDelta::PixelDelta(d) => (d.x, d.y, WheelMode::DeltaPixel),
                    };
                    v.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                        WheelDelta { x, y, z: 0.0, mode },
                        WebViewPoint::Device(p),
                    )));
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let mods = r.mods.get();
                if let Some(s) = input::shortcut(&event, mods) {
                    if matches!(s, input::Shortcut::FocusAddress | input::Shortcut::NewTab) {
                        r.set_focus(Focus::Chrome);
                    }
                    r.browser.shortcut(s);
                } else if let Some(v) = r.focused_view() {
                    v.notify_input_event(InputEvent::Keyboard(input::keyboard(&event, mods)));
                }
            }
            _ => {}
        }
        r.after_event();
    }
}

/// Open the window and run until it is closed.
pub fn run(private: bool, start: Option<Url>) -> Result<(), String> {
    let event_loop = EventLoop::<Wake>::with_user_event()
        .build()
        .map_err(|e| e.to_string())?;
    let mut app = App {
        proxy: event_loop.create_proxy(),
        private,
        start,
        running: None,
        failed: None,
    };
    event_loop.run_app(&mut app).map_err(|e| e.to_string())?;
    match app.failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn cursor_icon(c: Cursor) -> CursorIcon {
    match c {
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::Text | Cursor::VerticalText => CursorIcon::Text,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Help => CursorIcon::Help,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Move | Cursor::AllScroll => CursorIcon::Move,
        Cursor::NotAllowed | Cursor::NoDrop => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EwResize | Cursor::EResize | Cursor::WResize | Cursor::ColResize => {
            CursorIcon::EwResize
        }
        Cursor::NsResize | Cursor::NResize | Cursor::SResize | Cursor::RowResize => {
            CursorIcon::NsResize
        }
        Cursor::NeswResize | Cursor::NeResize | Cursor::SwResize => CursorIcon::NeswResize,
        Cursor::NwseResize | Cursor::NwResize | Cursor::SeResize => CursorIcon::NwseResize,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::None | Cursor::Default => CursorIcon::Default,
    }
}
