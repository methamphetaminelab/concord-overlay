mod avatars;
mod config;
mod draw;
mod voice;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken,
            channel::{self, Event},
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, QueueHandle,
            globals::registry_queue_init,
            protocol::{wl_output::WlOutput, wl_shm, wl_surface::WlSurface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, exit};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tiny_skia::Pixmap;

const RECONNECT: Duration = Duration::from_secs(2);
const AVATAR_RETRY: Duration = Duration::from_secs(600);

enum Avatar {
    Pending,
    Ready(Pixmap),
    Failed(Instant),
}

struct Surface {
    layer: LayerSurface,
    output: Option<WlOutput>,
    size: (u32, u32),
    configured: bool,
    buffer: Option<Buffer>,
    drawn_opacity: Option<u8>,
}

struct App {
    cfg: config::Config,
    handle: LoopHandle<'static, App>,
    qh: QueueHandle<App>,
    registry: RegistryState,
    outputs: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: Option<SlotPool>,
    surface: Option<Surface>,
    scale: i32,
    socket_path: PathBuf,
    lines: voice::LineBuf,
    members: Vec<voice::Member>,
    last_change: Instant,
    fade_timer: Option<RegistrationToken>,
    avatars: HashMap<String, Avatar>,
    fetch: mpsc::Sender<String>,
    last_log: String,
    announced: bool,
}

impl App {
    fn log(&mut self, msg: String) {
        if msg != self.last_log {
            eprintln!("concord-overlay: {msg}");
            self.last_log = msg;
        }
    }

    fn try_connect(&mut self) -> bool {
        let stream = match UnixStream::connect(&self.socket_path) {
            Ok(s) => s,
            Err(e) => {
                if !matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) {
                    self.log(format!("error: {}: {e}", self.socket_path.display()));
                }
                return false;
            }
        };
        if let Err(e) = stream.set_nonblocking(true) {
            self.log(format!("error: socket: {e}"));
            return false;
        }
        self.lines = voice::LineBuf::default();
        let src = Generic::new(stream, Interest::READ, Mode::Level);
        let inserted = self.handle.insert_source(src, |_, stream, app| {
            let mut buf = [0u8; 16 * 1024];
            loop {
                let n = match (&**stream).read(&mut buf) {
                    Ok(0) => return Ok(app.disconnected("concord closed the voice socket".into())),
                    Ok(n) => n,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => return Ok(app.disconnected(format!("error: voice socket: {e}"))),
                };
                let lines = match app.lines.push(&buf[..n]) {
                    Ok(lines) => lines,
                    Err(e) => return Ok(app.disconnected(format!("error: {e}"))),
                };
                for line in lines {
                    if let Err(e) = app.apply_line(&line) {
                        return Ok(app.disconnected(format!("error: {e}")));
                    }
                }
            }
            app.refresh();
            Ok(PostAction::Continue)
        });
        if let Err(e) = inserted {
            self.log(format!("error: event loop: {e}"));
            return false;
        }
        self.announced = false;
        true
    }

    fn schedule_reconnect(&mut self) {
        let timer = Timer::from_duration(RECONNECT);
        self.handle
            .insert_source(timer, |_, _, app| {
                if app.try_connect() {
                    TimeoutAction::Drop
                } else {
                    TimeoutAction::ToDuration(RECONNECT)
                }
            })
            .expect("timer source");
    }

    fn disconnected(&mut self, why: String) -> PostAction {
        self.log(why);
        self.members.clear();
        self.refresh();
        self.schedule_reconnect();
        PostAction::Remove
    }

    fn apply_line(&mut self, line: &[u8]) -> Result<(), String> {
        let state = voice::parse_line(line)?;
        if !self.announced {
            self.announced = true;
            self.log(format!("connected to {}", self.socket_path.display()));
        }
        self.members = voice::visible(state, self.cfg.highlight_self);
        self.last_change = Instant::now();
        for url in self.members.iter().filter_map(|m| m.avatar.clone()) {
            let stale = match self.avatars.get(&url) {
                None => true,
                Some(Avatar::Failed(when)) => when.elapsed() > AVATAR_RETRY,
                Some(_) => false,
            };
            if stale {
                self.avatars.insert(url.clone(), Avatar::Pending);
                self.fetch.send(url).expect("avatar thread is alive");
            }
        }
        Ok(())
    }

    fn avatar_loaded(&mut self, (url, result): avatars::Loaded) {
        let entry = match result {
            Ok(img) => Avatar::Ready(img),
            Err(e) => {
                eprintln!("concord-overlay: error: avatar {url}: {e} (showing a placeholder)");
                Avatar::Failed(Instant::now())
            }
        };
        self.avatars.insert(url, entry);
        self.refresh();
    }

    fn target_output(&self) -> Option<WlOutput> {
        if self.cfg.monitor == "Any" {
            return None;
        }
        self.outputs.outputs().find(|o| {
            self.outputs.info(o).and_then(|i| i.name).as_deref() == Some(self.cfg.monitor.as_str())
        })
    }

    fn refresh(&mut self) {
        if self.members.is_empty() {
            self.surface = None;
            self.pool = None;
            if let Some(t) = self.fade_timer.take() {
                self.handle.remove(t);
            }
            return;
        }
        let lay = draw::layout(&self.cfg, self.members.len());
        let target = self.target_output();
        if target.is_some() && self.surface.as_ref().is_some_and(|s| s.output != target) {
            self.surface = None;
        }
        let Some(surf) = &mut self.surface else {
            if target.is_none() && self.cfg.monitor != "Any" {
                self.log(format!(
                    "warning: monitor {} not found, letting the compositor choose",
                    self.cfg.monitor
                ));
            }
            let wl = self.compositor.create_surface(&self.qh);
            let layer = self.layer_shell.create_layer_surface(
                &self.qh,
                wl,
                Layer::Overlay,
                Some("concord-overlay"),
                target.as_ref(),
            );
            layer.set_anchor(draw::anchor(&self.cfg));
            layer.set_size(lay.width, lay.height);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_exclusive_zone(0);
            let empty = Region::new(&self.compositor).expect("wl_compositor");
            layer.wl_surface().set_input_region(Some(empty.wl_region()));
            layer.commit();
            let size = (lay.width, lay.height);
            self.surface = Some(Surface {
                layer,
                output: target,
                size,
                configured: false,
                buffer: None,
                drawn_opacity: None,
            });
            return;
        };
        if surf.size != (lay.width, lay.height) {
            surf.size = (lay.width, lay.height);
            surf.configured = false;
            surf.layer.set_size(lay.width, lay.height);
            surf.layer.commit();
            return;
        }
        self.draw();
    }

    fn opacity(&self) -> (u8, Option<Duration>) {
        let idle = self.last_change.elapsed();
        let speaking = self.members.iter().any(|m| m.speaking);
        let o = draw::opacity(&self.cfg, idle, speaking);
        (
            (o * 255.0).round() as u8,
            draw::next_tick(&self.cfg, idle, speaking),
        )
    }

    fn draw(&mut self) {
        let (opacity, next) = self.opacity();
        let Some(surf) = self.surface.as_mut().filter(|s| s.configured) else {
            return;
        };
        let scale = self.scale.max(1) as u32;
        let lay = draw::layout(&self.cfg, self.members.len());
        let avatars = &self.avatars;
        let pix = draw::render(&self.cfg, &lay, &self.members, scale, |m| {
            match avatars.get(m.avatar.as_ref()?) {
                Some(Avatar::Ready(img)) => Some(img),
                _ => None,
            }
        });
        let (w, h) = (pix.width() as i32, pix.height() as i32);
        let pool = match &mut self.pool {
            Some(p) => p,
            None => match SlotPool::new(pix.data().len(), &self.shm) {
                Ok(p) => self.pool.insert(p),
                Err(e) => return eprintln!("concord-overlay: error: shm pool: {e}"),
            },
        };
        let buffer = match surf.buffer.take() {
            Some(b) if b.height() == h && b.stride() == w * 4 && b.canvas(pool).is_some() => b,
            _ => match pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) {
                Ok((b, _)) => b,
                Err(e) => return eprintln!("concord-overlay: error: shm buffer: {e}"),
            },
        };
        draw::to_argb(
            pix.data(),
            opacity,
            buffer.canvas(pool).expect("idle buffer"),
        );
        let wl = surf.layer.wl_surface();
        wl.set_buffer_scale(scale as i32);
        wl.damage_buffer(0, 0, w, h);
        buffer.attach_to(wl).expect("buffer from our pool");
        surf.layer.commit();
        surf.buffer = Some(buffer);
        surf.drawn_opacity = Some(opacity);
        if let Some(d) = next
            && self.fade_timer.is_none()
        {
            let timer = Timer::from_duration(d);
            self.fade_timer = Some(
                self.handle
                    .insert_source(timer, |_, _, app| app.fade_tick())
                    .expect("timer source"),
            );
        }
    }

    fn fade_tick(&mut self) -> TimeoutAction {
        let (opacity, next) = self.opacity();
        if self
            .surface
            .as_ref()
            .is_some_and(|s| s.drawn_opacity != Some(opacity))
        {
            self.draw();
        }
        match next {
            Some(d) => TimeoutAction::ToDuration(d),
            None => {
                self.fade_timer = None;
                TimeoutAction::Drop
            }
        }
    }

    fn drop_surface_if(&mut self, pred: impl Fn(&Surface) -> bool) {
        if self.surface.as_ref().is_some_and(pred) {
            self.surface = None;
            self.handle.insert_idle(|app| app.refresh());
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        factor: i32,
    ) {
        if self.scale != factor {
            self.scale = factor;
            self.draw();
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: smithay_client_toolkit::reexports::client::protocol::wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        self.refresh();
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {
        self.refresh();
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: WlOutput) {
        self.drop_surface_if(|s| s.output.as_ref() == Some(&output));
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.drop_surface_if(|s| &s.layer == layer);
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(s) = self.surface.as_mut().filter(|s| &s.layer == layer) {
            s.configured = true;
            self.draw();
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_registry!(App);
delegate_dispatch2!(App);

fn fail(code: i32, msg: impl std::fmt::Display) -> ! {
    let msg = msg.to_string();
    eprintln!("concord-overlay: {msg}");
    if code == 2 {
        match Command::new("notify-send")
            .args(["-a", "concord-overlay", "concord-overlay"])
            .arg(&msg)
            .status()
        {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("concord-overlay: notify-send failed: {s}"),
            Err(e) => eprintln!("concord-overlay: cannot run notify-send: {e}"),
        }
    }
    exit(code)
}

fn main() {
    let cfg_path =
        config::path().unwrap_or_else(|| fail(2, "neither XDG_CONFIG_HOME nor HOME is set"));
    let cfg =
        config::load(&cfg_path).unwrap_or_else(|e| fail(2, format_args!("config error: {e}")));
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .unwrap_or_else(|| fail(2, "XDG_RUNTIME_DIR is not set"));
    let cache =
        avatars::cache_dir().unwrap_or_else(|| fail(2, "neither XDG_CACHE_HOME nor HOME is set"));

    let conn = Connection::connect_to_env()
        .unwrap_or_else(|e| fail(1, format_args!("no Wayland display: {e}")));
    let (globals, mut queue) =
        registry_queue_init::<App>(&conn).unwrap_or_else(|e| fail(1, format_args!("Wayland: {e}")));
    let qh = queue.handle();
    let layer_shell = LayerShell::bind(&globals, &qh).unwrap_or_else(|_| {
        fail(3, "the compositor has no wlr-layer-shell (zwlr_layer_shell_v1), e.g. GNOME; the overlay cannot work in this session")
    });
    let compositor = CompositorState::bind(&globals, &qh)
        .unwrap_or_else(|e| fail(1, format_args!("wl_compositor: {e}")));
    let shm = Shm::bind(&globals, &qh).unwrap_or_else(|e| fail(1, format_args!("wl_shm: {e}")));

    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("event loop");
    let handle = event_loop.handle();
    let (done_tx, done_rx) = channel::channel::<avatars::Loaded>();
    handle
        .insert_source(done_rx, |ev, _, app| {
            if let Event::Msg(loaded) = ev {
                app.avatar_loaded(loaded);
            }
        })
        .expect("channel source");
    let fetch = avatars::spawn(cache, move |loaded| {
        let _ = done_tx.send(loaded);
    });

    let outputs = OutputState::new(&globals, &qh);
    let mut app = App {
        cfg,
        handle,
        qh,
        registry: RegistryState::new(&globals),
        outputs,
        compositor,
        layer_shell,
        shm,
        pool: None,
        surface: None,
        scale: 1,
        socket_path: PathBuf::from(runtime).join("concord-voice.sock"),
        lines: voice::LineBuf::default(),
        members: Vec::new(),
        last_change: Instant::now(),
        fade_timer: None,
        avatars: HashMap::new(),
        fetch,
        last_log: String::new(),
        announced: false,
    };
    queue
        .roundtrip(&mut app)
        .unwrap_or_else(|e| fail(1, format_args!("Wayland: {e}")));
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .expect("wayland source");
    if !app.try_connect() {
        app.schedule_reconnect();
    }
    if let Err(e) = event_loop.run(None, &mut app, |_| {}) {
        fail(1, format_args!("Wayland connection lost: {e}"));
    }
}
