//! The game's window (SDL2), with the small API the GLES layer expects
//! (the same shape as touchHLE's window, see gles/gles1_on_gl2.rs).

#[allow(dead_code)]
pub enum GLVersion {
    GLES11,
    GL21Compat,
}

pub struct GLContext(sdl2::video::GLContext);

impl GLContext {
    pub fn is_current(&self) -> bool {
        self.0.is_current()
    }
}

/// One input event from the window, keyboard or a game controller.
pub enum Input {
    /// Left mouse button: (phase 0 down / 1 move / 3 up, window x, y).
    Mouse(u32, f32, f32),
    /// Right mouse button down / up.
    RightMouse(bool),
    /// The mouse wheel turned: notches, positive away from the user.
    Wheel(i32),
    /// A key went down or up (key repeats are dropped); shift held.
    Key(sdl2::keyboard::Scancode, bool, bool),
    /// A controller button went down or up.
    Pad(sdl2::controller::Button, bool),
    /// A controller axis moved (-32768..32767; triggers 0..32767).
    Axis(sdl2::controller::Axis, i16),
    /// The mouse moved with no button held (window x, y).
    Hover(f32, f32),
    /// A controller was connected (true) or disconnected (false).
    PadPlugged(bool),
}

/// Local Match player number of this window (`--player=N`, default 1).
pub static PLAYER: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
/// The only controller (index in connection order) that drives this window.
pub static ONLY_PAD: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
/// Start in a window instead of full screen (`--windowed`, test runs, Local
/// Match). Alt+Enter switches between the two at any time.
pub static WINDOWED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// The picture's aspect ratio (landscape 480 x 320).
const ASPECT: f32 = 480.0 / 320.0;

/// The largest 3:2 rectangle centred in a `w` x `h` area (x, y, width,
/// height): the rest is black bars.
pub fn fit(w: u32, h: u32) -> (f32, f32, f32, f32) {
    let (w, h) = (w as f32, h as f32);
    let (vw, vh) = if w / h > ASPECT { (h * ASPECT, h) } else { (w, w / ASPECT) };
    ((w - vw) / 2.0, (h - vh) / 2.0, vw, vh)
}

pub struct Window {
    _sdl: sdl2::Sdl,
    video_ctx: sdl2::VideoSubsystem,
    window: sdl2::video::Window,
    events: sdl2::EventPump,
    /// None if SDL's game-controller subsystem didn't start.
    controllers: Option<sdl2::GameControllerSubsystem>,
    /// Open controllers (kept open so their events arrive).
    pads: Vec<sdl2::controller::GameController>,
    pub quit: bool,
    /// The Return key of an Alt+Enter: its key-up is not passed on either.
    alt_enter: Option<sdl2::keyboard::Scancode>,
}

impl Window {
    /// A window of `w` x `h` pixels.
    /// A push-style 16-bit audio output (the run loop feeds it).
    pub fn open_audio(&self, rate: i32, channels: u8) -> Result<sdl2::audio::AudioQueue<i16>, String> {
        let audio = self._sdl.audio()?;
        let spec = sdl2::audio::AudioSpecDesired { freq: Some(rate), channels: Some(channels), samples: Some(1024) };
        let q = audio.open_queue::<i16, _>(None, &spec)?;
        q.resume();
        Ok(q)
    }

    pub fn new(title: &str, w: u32, h: u32) -> Result<Window, String> {
        // Controllers keep working while the window is in the background: with
        // two windows on one computer (Local Match), each player's pad drives
        // its own window whichever one has focus.
        sdl2::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
        let sdl = sdl2::init()?;
        let video_ctx = sdl.video()?;
        let player = PLAYER.get().copied().unwrap_or(1);
        let title = if player > 1 { format!("{title} - PLAYER {player}") } else { title.to_string() };
        let window = video_ctx
            .window(&title, w, h)
            .position_centered()
            .opengl()
            .allow_highdpi()
            .resizable()
            .build()
            .map_err(|e| e.to_string())?;
        let mut window = window;
        if !WINDOWED.get().copied().unwrap_or(false) {
            // Borderless, at the desktop's own resolution (no mode change).
            window.set_fullscreen(sdl2::video::FullscreenType::Desktop)?;
        }
        let events = sdl.event_pump()?;
        let controllers = sdl.game_controller().ok();
        Ok(Window { _sdl: sdl, video_ctx, window, events, controllers, pads: Vec::new(), quit: false, alt_enter: None })
    }

    pub fn create_gl_context(&self, version: GLVersion) -> Result<GLContext, String> {
        let attr = self.video_ctx.gl_attr();
        match version {
            GLVersion::GLES11 => {
                attr.set_context_version(1, 1);
                attr.set_context_profile(sdl2::video::GLProfile::GLES);
            }
            GLVersion::GL21Compat => {
                attr.set_context_version(2, 1);
                attr.set_context_profile(sdl2::video::GLProfile::Compatibility);
            }
        }
        self.window.gl_create_context().map(GLContext)
    }

    pub fn gl_get_proc_address(&self, procname: &str) -> *const std::ffi::c_void {
        self.video_ctx.gl_get_proc_address(procname) as *const _
    }

    /// # Safety
    /// The context must belong to this window.
    pub unsafe fn make_gl_context_current(&self, gl_ctx: &GLContext) {
        self.window.gl_make_current(&gl_ctx.0).unwrap();
    }

    pub fn swap_window(&self) {
        self.window.gl_swap_window();
    }

    /// Size of the drawable area in pixels.
    pub fn drawable_size(&self) -> (u32, u32) {
        self.window.drawable_size()
    }

    /// Handles window events: returns mouse, keyboard and controller input.
    /// Controllers are opened as they are found (also when plugged in later).
    pub fn poll_events(&mut self) -> Vec<Input> {
        use sdl2::event::Event;
        use sdl2::keyboard::{Mod, Scancode};
        use sdl2::mouse::MouseButton;
        let mut out = Vec::new();
        let mut added = Vec::new();
        let mut toggle = false;
        let pad_ids: Vec<u32> = self.pads.iter().map(|p| p.instance_id()).collect();
        let wanted = |which: u32| match ONLY_PAD.get().copied().flatten() {
            None => true,
            Some(n) => pad_ids.iter().position(|&i| i == which) == Some(n),
        };
        for e in self.events.poll_iter() {
            match e {
                Event::Quit { .. } => self.quit = true,
                Event::MouseButtonDown { mouse_btn: MouseButton::Left, x, y, .. } => out.push(Input::Mouse(0, x as f32, y as f32)),
                Event::MouseMotion { mousestate, x, y, .. } if mousestate.left() => out.push(Input::Mouse(1, x as f32, y as f32)),
                Event::MouseMotion { x, y, .. } => out.push(Input::Hover(x as f32, y as f32)),
                Event::MouseButtonUp { mouse_btn: MouseButton::Left, x, y, .. } => out.push(Input::Mouse(3, x as f32, y as f32)),
                Event::MouseButtonDown { mouse_btn: MouseButton::Right, .. } => out.push(Input::RightMouse(true)),
                Event::MouseButtonUp { mouse_btn: MouseButton::Right, .. } => out.push(Input::RightMouse(false)),
                Event::MouseWheel { y, direction, .. } if y != 0 => {
                    let flipped = direction == sdl2::mouse::MouseWheelDirection::Flipped;
                    out.push(Input::Wheel(if flipped { -y } else { y }));
                }
                // Alt+Enter: full screen <-> window (not passed to the game).
                Event::KeyDown { scancode: Some(k @ (Scancode::Return | Scancode::KpEnter)), keymod, .. }
                    if keymod.intersects(Mod::LALTMOD | Mod::RALTMOD) =>
                {
                    if self.alt_enter.is_none() {
                        self.alt_enter = Some(k);
                        toggle = true;
                    }
                }
                Event::KeyUp { scancode: Some(k), .. } if self.alt_enter == Some(k) => self.alt_enter = None,
                Event::KeyDown { scancode: Some(k), repeat: false, keymod, .. } => {
                    out.push(Input::Key(k, true, keymod.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD)))
                }
                Event::KeyUp { scancode: Some(k), keymod, .. } => {
                    out.push(Input::Key(k, false, keymod.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD)))
                }
                Event::ControllerDeviceAdded { which, .. } => added.push(which),
                Event::ControllerDeviceRemoved { which, .. } if pad_ids.contains(&which) => {
                    if let Some(i) = self.pads.iter().position(|p| p.instance_id() == which) {
                        println!("controller disconnected: {}", self.pads[i].name());
                        self.pads.remove(i);
                    }
                    out.push(Input::PadPlugged(false));
                }
                Event::ControllerButtonDown { button, which, .. } if wanted(which) => out.push(Input::Pad(button, true)),
                Event::ControllerButtonUp { button, which, .. } if wanted(which) => out.push(Input::Pad(button, false)),
                Event::ControllerAxisMotion { axis, value, which, .. } if wanted(which) => out.push(Input::Axis(axis, value)),
                _ => {}
            }
        }
        if toggle {
            self.toggle_fullscreen();
        }
        if let Some(gc) = &self.controllers {
            for which in added {
                if let Ok(pad) = gc.open(which) {
                    println!("controller connected: {}", pad.name());
                    self.pads.push(pad);
                    out.push(Input::PadPlugged(true));
                }
            }
        }
        out
    }

    /// Shows or hides the system's mouse cursor (the menus draw their own).
    pub fn show_cursor(&self, on: bool) {
        self._sdl.mouse().show_cursor(on);
    }

    /// Controllers connected now.
    pub fn pad_count(&self) -> usize {
        self.pads.iter().filter(|p| p.attached()).count()
    }

    /// The window's size in the units mouse events use.
    pub fn size(&self) -> (u32, u32) {
        self.window.size()
    }

    /// Size of the desktop the window is on (in pixels).
    pub fn desktop_size(&self) -> (u32, u32) {
        let i = self.window.display_index().unwrap_or(0);
        self.video_ctx.desktop_display_mode(i).map_or((960, 640), |m| (m.w as u32, m.h as u32))
    }

    /// Where the picture is, in the units mouse events use (see `fit`).
    pub fn picture(&self) -> (f32, f32, f32, f32) {
        let (w, h) = self.window.size();
        fit(w, h)
    }

    /// Full screen (borderless, desktop resolution) <-> a window.
    pub fn toggle_fullscreen(&mut self) {
        use sdl2::video::FullscreenType;
        let to = if self.window.fullscreen_state() == FullscreenType::Off { FullscreenType::Desktop } else { FullscreenType::Off };
        if let Err(e) = self.window.set_fullscreen(to) {
            println!("window: full screen switch failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn picture_keeps_3_2() {
        assert_eq!(super::fit(960, 640), (0.0, 0.0, 960.0, 640.0));
        // 16:9 screen: bars left and right.
        assert_eq!(super::fit(1920, 1080), (150.0, 0.0, 1620.0, 1080.0));
        // 4:3 screen: bars top and bottom.
        let (x, y, w, h) = super::fit(1600, 1200);
        assert_eq!((x, w), (0.0, 1600.0));
        assert!((y - 66.667).abs() < 0.01 && (h - 1066.667).abs() < 0.01);
    }

    #[test]
    fn scale_from_desktop() {
        use crate::gfx::auto_scale;
        assert_eq!(auto_scale(1920, 1080), 4);
        assert_eq!(auto_scale(2560, 1440), 5);
        assert_eq!(auto_scale(3840, 2160), 7);
        assert_eq!(auto_scale(1280, 720), 3);
    }
}
