// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 EON contributors

//! The window. Servo draws the bar and the page into two offscreen surfaces of the
//! window's GL context; each frame copies them into place, the bar on top. Input goes to
//! whichever is under the pointer, keys to whichever has focus, and the browser's own
//! shortcuts are taken first.

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

use crate::browser::{Browser, Host, Options};
use crate::input;

/// The bar's height when no suggestions are open (CSS pixels): tab strip + navigation.
const BASE_CHROME: u32 = 84;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Chrome,
    Page,
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
    chrome_css: Cell<u32>,
    relayout: Cell<bool>,
    focus_page: Cell<bool>,
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
    fn chrome_height(&self, css_px: u32) {
        if self.0.chrome_css.replace(css_px) != css_px {
            self.0.relayout.set(true);
            self.0.window.request_redraw();
        }
    }
    fn focus_page(&self) {
        self.0.focus_page.set(true);
    }
    fn open_private_window(&self) {
        self.0.private_window.set(true);
    }
}

struct Running {
    shared: Rc<Shared>,
    ctx: Rc<WindowRenderingContext>,
    ui_ctx: Rc<OffscreenRenderingContext>,
    page_ctx: Rc<OffscreenRenderingContext>,
    browser: Rc<Browser>,
    cursor: Cell<PhysicalPosition<f64>>,
    mods: Cell<ModifiersState>,
    focus: Cell<Focus>,
}

impl Running {
    fn scale(&self) -> f64 {
        self.shared.window.scale_factor()
    }

    /// Physical heights: the bar as drawn now, and the bar the page sits under.
    fn heights(&self) -> (u32, u32) {
        let s = self.scale();
        let base = (f64::from(BASE_CHROME) * s).round() as u32;
        let ui = (f64::from(self.shared.chrome_css.get().max(BASE_CHROME)) * s).round() as u32;
        (ui, base)
    }

    fn layout(&self) {
        let size = self.shared.window.inner_size();
        let (ui_h, base_h) = self.heights();
        self.ctx.resize(size);
        let page = PhysicalSize::new(size.width.max(1), size.height.saturating_sub(base_h).max(1));
        let ui = PhysicalSize::new(size.width.max(1), ui_h.min(size.height).max(1));
        if let Some(v) = self.browser.chrome_view() {
            v.resize(ui);
        }
        if let Some(v) = self.browser.active_view() {
            v.resize(page);
        }
        self.shared.window.request_redraw();
    }

    fn paint(&self) {
        if self.ctx.make_current().is_err() {
            return;
        }
        if let Some(v) = self.browser.active_view() {
            v.paint();
        }
        if let Some(v) = self.browser.chrome_view() {
            v.paint();
        }
        self.ctx.prepare_for_rendering();
        let gl = self.ctx.glow_gl_api();
        let size = self.shared.window.inner_size();
        let (w, h) = (size.width as i32, size.height as i32);
        let (ui_h, base_h) = self.heights();
        let (ui_h, base_h) = (ui_h as i32, base_h as i32);
        // GL counts from the bottom: the page fills the bottom, the bar sits on top of it.
        if let Some(blit) = self.page_ctx.render_to_parent_callback() {
            blit(
                &gl,
                Rect::new(Point2D::new(0, 0), Size2D::new(w, (h - base_h).max(1))),
            );
        }
        if let Some(blit) = self.ui_ctx.render_to_parent_callback() {
            blit(
                &gl,
                Rect::new(
                    Point2D::new(0, (h - ui_h).max(0)),
                    Size2D::new(w, ui_h.min(h)),
                ),
            );
        }
        self.ctx.present();
    }

    /// The view under the pointer and the point inside it.
    fn target(&self) -> Option<(WebView, Focus, DevicePoint)> {
        let p = self.cursor.get();
        let (ui_h, base_h) = self.heights();
        if p.y < f64::from(ui_h) {
            let v = self.browser.chrome_view()?;
            Some((v, Focus::Chrome, DevicePoint::new(p.x as f32, p.y as f32)))
        } else {
            let v = self.browser.active_view()?;
            let y = p.y - f64::from(base_h);
            Some((v, Focus::Page, DevicePoint::new(p.x as f32, y as f32)))
        }
    }

    fn set_focus(&self, f: Focus) {
        self.focus.set(f);
        if let Some(ui) = self.browser.chrome_view() {
            ui.set_focused(f == Focus::Chrome);
        }
        if let Some(page) = self.browser.active_view() {
            page.set_focused(f == Focus::Page);
        }
    }

    fn focused_view(&self) -> Option<WebView> {
        match self.focus.get() {
            Focus::Chrome => self.browser.chrome_view(),
            Focus::Page => self.browser.active_view(),
        }
    }

    fn after_event(&self) {
        self.browser.servo().spin_event_loop();
        self.browser.flush_ui();
        if self.shared.relayout.replace(false) {
            self.layout();
        }
        if self.shared.focus_page.replace(false) {
            self.set_focus(Focus::Page);
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
            .with_inner_size(LogicalSize::new(1280.0, 820.0))
            .with_min_inner_size(LogicalSize::new(480.0, 320.0));
        let window = Rc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        let display = event_loop.display_handle().map_err(|e| e.to_string())?;
        let handle = window.window_handle().map_err(|e| e.to_string())?;
        let size = window.inner_size();
        let ctx = Rc::new(
            WindowRenderingContext::new(display, handle, size).map_err(|e| format!("{e:?}"))?,
        );
        ctx.make_current().map_err(|e| format!("{e:?}"))?;

        let data = crate::paths::data_dir();
        let servo =
            crate::engine::builder(self.private, &data, Box::new(Waker(self.proxy.clone())))
                .build();
        let scale = window.scale_factor();
        let base_h = (f64::from(BASE_CHROME) * scale).round() as u32;
        let ui_ctx =
            Rc::new(ctx.offscreen_context(PhysicalSize::new(size.width.max(1), base_h.max(1))));
        let page_ctx = Rc::new(ctx.offscreen_context(PhysicalSize::new(
            size.width.max(1),
            size.height.saturating_sub(base_h).max(1),
        )));

        let shared = Rc::new(Shared {
            window: window.clone(),
            chrome_css: Cell::new(BASE_CHROME),
            relayout: Cell::new(false),
            focus_page: Cell::new(false),
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
            },
        );
        browser.attach_chrome(ui_ctx.clone());
        browser.open_start(self.start.take());
        let running = Running {
            shared,
            ctx,
            ui_ctx,
            page_ctx,
            browser,
            cursor: Cell::new(PhysicalPosition::new(0.0, 0.0)),
            mods: Cell::new(ModifiersState::empty()),
            focus: Cell::new(Focus::Page),
        };
        running.set_focus(Focus::Page);
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
                event_loop.exit();
                return;
            }
            WindowEvent::RedrawRequested => r.paint(),
            WindowEvent::Resized(_) => r.layout(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                for v in [r.browser.chrome_view(), r.browser.active_view()]
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
                    if s == input::Shortcut::FocusAddress {
                        r.set_focus(Focus::Chrome);
                    }
                    r.browser.shortcut(s);
                    if s == input::Shortcut::NewTab {
                        r.set_focus(Focus::Chrome);
                    }
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
