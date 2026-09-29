use std::io::{self, IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};
use urushi::View;
use urushi_terminal::backend::native::NativeTerminal;
use urushi_terminal::{
    Command as TerminalCommand, CommandWriter, Event as TerminalEvent, EventSource,
    KeyboardEnhancementQuery, Position, RawModeControl, TerminalCapabilities, TerminalOutput,
    TerminalQuery, TerminalSize, WindowSize,
};
use urushi_tui_app::{
    Admission, Application, Effect, Input, KeyKind, Runtime, Subscription, Surface,
};

use crate::graphics::{
    GraphicsBackendKind, GraphicsPreference, GraphicsSelection, KittyBackend, SixelBackend,
    select_graphics_backend,
};
use crate::heading::HeadingFonts;
use crate::image::ImageCatalog;
use crate::{
    App, CellRect, Command, DesiredFrame, Document, Layout, PixelSize, RasterCache, config::Config,
    ui,
};

const SIXEL_REDRAW: &str = "raden: sixel full redraw required";
const GRAPHICS_FAILURE_PREFIX: &str = "raden graphics backend ";

pub(crate) fn run(
    path: &Path,
    document: &Document,
    config: &Config,
    preference: GraphicsPreference,
) -> Result<()> {
    if !io::stdout().is_terminal() {
        return Err(anyhow!("an interactive terminal is required on stdout"));
    }
    let heading_fonts = if config.heading_families().is_empty() {
        HeadingFonts::default()
    } else {
        HeadingFonts::load(config.heading_families()).map_err(anyhow::Error::msg)?
    };
    let images = ImageCatalog::load(document, path);
    let control = GraphicsControl::new();
    let application = ReaderApplication {
        path: path.to_path_buf(),
        document: document.clone(),
        images,
        heading_fonts: Mutex::new(Some(heading_fonts)),
        max_width: config.body_max_width(),
        graphics: Arc::clone(&control),
    };
    let terminal = NativeTerminal::open().context("could not open the controlling terminal")?;
    let backend = DirectGraphicsBackend::new(
        terminal,
        io::stdout(),
        preference,
        needs_graphics(document),
        control,
    );
    let model = Runtime::new(application)
        .backend(backend)
        .bracketed_paste(false)
        .focus_change(false)
        .keyboard_enhancement(None)
        .run()
        .context("terminal application failed")?;
    if let Some(error) = model.fatal_error {
        Err(error).context("terminal application failed")
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct GraphicsStartup {
    kind: GraphicsBackendKind,
    diagnostic: Option<String>,
    queried_cell_pixels: Option<PixelSize>,
}

#[derive(Default)]
struct GraphicsControlState {
    startup: Option<GraphicsStartup>,
    frame: Option<DesiredFrame>,
    disable_requested: bool,
}

struct GraphicsControl {
    state: Mutex<GraphicsControlState>,
}

impl GraphicsControl {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GraphicsControlState::default()),
        })
    }

    fn startup(&self) -> GraphicsStartup {
        self.state
            .lock()
            .expect("graphics control poisoned")
            .startup
            .clone()
            .unwrap_or(GraphicsStartup {
                kind: GraphicsBackendKind::Text,
                diagnostic: Some("graphics initialization was unavailable".to_owned()),
                queried_cell_pixels: None,
            })
    }

    fn set_startup(&self, startup: GraphicsStartup) {
        self.state
            .lock()
            .expect("graphics control poisoned")
            .startup = Some(startup);
    }

    fn publish(&self, frame: DesiredFrame) {
        self.state.lock().expect("graphics control poisoned").frame = Some(frame);
    }

    fn take_frame(&self) -> Option<DesiredFrame> {
        self.state
            .lock()
            .expect("graphics control poisoned")
            .frame
            .take()
    }

    fn discard_frame(&self) {
        self.state.lock().expect("graphics control poisoned").frame = None;
    }

    fn request_disable(&self) {
        let mut state = self.state.lock().expect("graphics control poisoned");
        state.disable_requested = true;
        state.frame = None;
    }

    fn take_disable_request(&self) -> bool {
        let mut state = self.state.lock().expect("graphics control poisoned");
        std::mem::take(&mut state.disable_requested)
    }
}

struct DirectGraphicsBackend {
    terminal: NativeTerminal,
    graphics_output: Stdout,
    preference: GraphicsPreference,
    needs_graphics: bool,
    control: Arc<GraphicsControl>,
    graphics: GraphicsMode,
    selected: bool,
    last_generation: Option<u64>,
    sixel_retry_generation: Option<u64>,
}

impl DirectGraphicsBackend {
    fn new(
        terminal: NativeTerminal,
        graphics_output: Stdout,
        preference: GraphicsPreference,
        needs_graphics: bool,
        control: Arc<GraphicsControl>,
    ) -> Self {
        Self {
            terminal,
            graphics_output,
            preference,
            needs_graphics,
            control,
            graphics: GraphicsMode::Text,
            selected: false,
            last_generation: None,
            sixel_retry_generation: None,
        }
    }

    fn select_graphics(&mut self, window: WindowSize) -> io::Result<()> {
        if self.selected {
            return Ok(());
        }
        let should_probe = self.preference != GraphicsPreference::Text
            && (self.needs_graphics || self.preference != GraphicsPreference::Auto);
        let capabilities = if should_probe {
            self.terminal.terminal_capabilities()?
        } else {
            TerminalCapabilities::none()
        };
        let GraphicsSelection { backend, reason } =
            select_graphics_backend(self.preference, capabilities, self.needs_graphics)
                .map_err(|error| io::Error::other(error.to_string()))?;
        self.graphics = GraphicsMode::from_kind(backend);
        self.control.set_startup(GraphicsStartup {
            kind: backend,
            diagnostic: reason,
            queried_cell_pixels: window.cell_pixels().and_then(convert_cell_pixels),
        });
        self.selected = true;
        Ok(())
    }

    fn clear_before_resize(&mut self) -> io::Result<()> {
        self.control.discard_frame();
        self.last_generation = None;
        self.sixel_retry_generation = None;
        self.graphics.clear(&mut self.graphics_output)
    }

    fn flush_frame(&mut self) -> io::Result<()> {
        if self.control.take_disable_request() {
            if matches!(self.graphics, GraphicsMode::Kitty(_)) {
                self.graphics.clear(&mut self.graphics_output)?;
            }
            self.graphics = GraphicsMode::Text;
            self.last_generation = None;
            self.sixel_retry_generation = None;
        }
        let Some(frame) = self.control.take_frame() else {
            return self.terminal.flush();
        };
        if self.last_generation == Some(frame.generation) {
            return self.terminal.flush();
        }
        self.terminal.flush()?;
        if matches!(self.graphics, GraphicsMode::Sixel(_))
            && self.sixel_retry_generation != Some(frame.generation)
        {
            self.graphics.clear(&mut self.graphics_output)?;
            self.sixel_retry_generation = Some(frame.generation);
            self.control.publish(frame);
            return Err(io::Error::other(SIXEL_REDRAW));
        }
        let backend_name = self.graphics.name();
        self.graphics
            .render(&frame, &mut self.graphics_output)
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("{GRAPHICS_FAILURE_PREFIX}{backend_name} failed: {error}"),
                )
            })?;
        self.last_generation = Some(frame.generation);
        self.sixel_retry_generation = None;
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        self.control.discard_frame();
        self.terminal.flush()?;
        self.graphics.clear(&mut self.graphics_output)?;
        self.graphics = GraphicsMode::Text;
        self.last_generation = None;
        self.sixel_retry_generation = None;
        Ok(())
    }

    fn observe_event(&mut self, event: &TerminalEvent) -> io::Result<()> {
        if matches!(event, TerminalEvent::Resize(_)) {
            self.clear_before_resize()?;
        }
        Ok(())
    }
}

impl TerminalOutput for DirectGraphicsBackend {
    fn flush(&mut self) -> io::Result<()> {
        self.flush_frame()
    }
}

impl CommandWriter for DirectGraphicsBackend {
    fn write_command(&mut self, command: TerminalCommand<'_>) -> io::Result<()> {
        if matches!(&command, TerminalCommand::SetAlternateScreen(false)) {
            self.cleanup()?;
        }
        self.terminal.write_command(command)
    }
}

impl EventSource for DirectGraphicsBackend {
    fn read_event(&mut self) -> io::Result<TerminalEvent> {
        let event = self.terminal.read_event()?;
        self.observe_event(&event)?;
        Ok(event)
    }

    fn poll_event(&mut self) -> io::Result<Option<TerminalEvent>> {
        let event = self.terminal.poll_event()?;
        if let Some(event) = &event {
            self.observe_event(event)?;
        }
        Ok(event)
    }

    fn poll_event_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> io::Result<Option<TerminalEvent>> {
        let event = self.terminal.poll_event_timeout(timeout)?;
        if let Some(event) = &event {
            self.observe_event(event)?;
        }
        Ok(event)
    }
}

impl RawModeControl for DirectGraphicsBackend {
    fn is_interactive(&self) -> bool {
        self.terminal.is_interactive()
    }

    fn enable_raw_mode(&mut self) -> io::Result<()> {
        self.terminal.enable_raw_mode()
    }

    fn disable_raw_mode(&mut self) -> io::Result<()> {
        self.terminal.disable_raw_mode()
    }
}

impl TerminalQuery for DirectGraphicsBackend {
    fn terminal_size(&mut self) -> io::Result<TerminalSize> {
        self.terminal.terminal_size()
    }

    fn cursor_position(&mut self) -> io::Result<Position> {
        self.terminal.cursor_position()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let window = self.terminal.window_size()?;
        self.select_graphics(window)?;
        Ok(window)
    }

    fn raw_mode_enabled(&mut self) -> io::Result<bool> {
        self.terminal.raw_mode_enabled()
    }

    fn terminal_capabilities(&mut self) -> io::Result<TerminalCapabilities> {
        self.terminal.terminal_capabilities()
    }
}

impl KeyboardEnhancementQuery for DirectGraphicsBackend {
    fn supports_keyboard_enhancement(&mut self) -> io::Result<bool> {
        self.terminal.supports_keyboard_enhancement()
    }
}

struct ReaderApplication {
    path: PathBuf,
    document: Document,
    images: ImageCatalog,
    heading_fonts: Mutex<Option<HeadingFonts>>,
    max_width: Option<u16>,
    graphics: Arc<GraphicsControl>,
}

struct ReaderModel {
    app: App,
    layout: Layout,
    surface: Surface,
    generation: u64,
    cell_pixels: PixelSize,
    desired_frame: DesiredFrame,
    graphics_kind: GraphicsBackendKind,
    graphics_diagnostic: Option<String>,
    diagnostic_count: usize,
    logical_renderer: Option<LogicalRenderer>,
    prepared_source: Option<PreparedSource>,
    fatal_error: Option<anyhow::Error>,
}

enum Message {
    Input(Input),
    Surface(Surface),
    Prepared(PreparedRender),
    TerminalError(io::Error),
}

impl Application for ReaderApplication {
    type Model = ReaderModel;
    type Message = Message;

    fn init(&self) -> (Self::Model, Effect<Self::Message>) {
        let startup = self.graphics.startup();
        let surface = Surface::new(1, 1);
        let body = ui::viewport_areas(surface.size, self.max_width)[1];
        let layout = Layout::with_text(&self.document, body.width, &self.images);
        let cell_pixels = startup.queried_cell_pixels.unwrap_or(PixelSize::FALLBACK);
        let (logical_renderer, prepared_source) = if startup.kind == GraphicsBackendKind::Text {
            (None, None)
        } else {
            let heading_fonts = self
                .heading_fonts
                .lock()
                .expect("heading font state poisoned")
                .take()
                .expect("an application is initialized only once");
            let (renderer, source) =
                LogicalRenderer::spawn(self.document.clone(), self.images.clone(), heading_fonts);
            (Some(renderer), Some(source))
        };
        let desired_frame = empty_frame(1, cell_pixels);
        (
            ReaderModel {
                app: App::default(),
                layout,
                surface,
                generation: 1,
                cell_pixels,
                desired_frame,
                graphics_kind: startup.kind,
                graphics_diagnostic: startup.diagnostic,
                diagnostic_count: self.images.diagnostics().len(),
                logical_renderer,
                prepared_source,
                fatal_error: None,
            },
            Effect::none(),
        )
    }

    fn update(&self, model: &mut Self::Model, message: Self::Message) -> Effect<Self::Message> {
        match message {
            Message::Input(Input::Key(key)) if key.kind != KeyKind::Release => {
                let command = Command::from_key(key);
                if command == Command::None {
                    return Effect::none();
                }
                let body = ui::viewport_areas(model.surface.size, self.max_width)[1];
                model.app.apply(command, &model.layout, body.height);
                if model.app.should_quit() {
                    return Effect::shutdown();
                }
                model.generation = model.generation.saturating_add(1);
                update_desired_frame(model, body);
                submit_logical_render(model, body);
                Effect::none()
            }
            Message::Input(_) => Effect::none(),
            Message::Surface(surface) => {
                model.surface = surface;
                model.cell_pixels = surface_cell_pixels(surface)
                    .or_else(|| self.graphics.startup().queried_cell_pixels)
                    .unwrap_or(PixelSize::FALLBACK);
                let body = ui::viewport_areas(surface.size, self.max_width)[1];
                model.layout = Layout::with_text(&self.document, body.width, &self.images);
                model.app.clamp_viewport(&model.layout, body.height);
                model.generation = model.generation.saturating_add(1);
                model.desired_frame = empty_frame(model.generation, model.cell_pixels);
                submit_logical_render(model, body);
                Effect::none()
            }
            Message::Prepared(prepared) if prepared.generation == model.generation => {
                model.layout = prepared.layout;
                model
                    .app
                    .clamp_viewport(&model.layout, prepared.body.height);
                let position = model.app.viewport_position(&model.layout);
                model.desired_frame = if position == prepared.position {
                    prepared.frame
                } else {
                    model.layout.desired_frame(
                        position.0,
                        position.1,
                        prepared.body,
                        model.generation,
                        model.cell_pixels,
                    )
                };
                Effect::none()
            }
            Message::Prepared(_) => Effect::none(),
            Message::TerminalError(error) if error.to_string() == SIXEL_REDRAW => Effect::none(),
            Message::TerminalError(error)
                if error.to_string().starts_with(GRAPHICS_FAILURE_PREFIX) =>
            {
                self.fallback_to_text(model, &error);
                Effect::none()
            }
            Message::TerminalError(error) => {
                model.fatal_error = Some(error.into());
                Effect::shutdown()
            }
        }
    }

    fn view(&self, model: &Self::Model) -> View {
        self.graphics.publish(model.desired_frame.clone());
        ui::view(
            &self.path,
            &model.app,
            &model.layout,
            &graphics_status(model),
            model.surface.size,
            self.max_width,
        )
    }

    fn subscriptions(&self, model: &Self::Model) -> Subscription<Self::Message> {
        let mut subscriptions = vec![
            Subscription::surface(Message::Surface),
            Subscription::input(Message::Input),
            Subscription::terminal_errors(Message::TerminalError),
        ];
        if let Some(source) = &model.prepared_source {
            subscriptions.push(source.subscription());
        }
        Subscription::batch(subscriptions)
    }
}

impl ReaderApplication {
    fn fallback_to_text(&self, model: &mut ReaderModel, error: &io::Error) {
        let failed = match model.graphics_kind {
            GraphicsBackendKind::Kitty => "kitty",
            GraphicsBackendKind::Sixel => "sixel",
            GraphicsBackendKind::Text => "graphics",
        };
        model.graphics_kind = GraphicsBackendKind::Text;
        model.graphics_diagnostic = Some(format!("{failed} failed: {error}"));
        model.logical_renderer = None;
        model.prepared_source = None;
        self.graphics.request_disable();
        let body = ui::viewport_areas(model.surface.size, self.max_width)[1];
        model.layout = Layout::with_text(&self.document, body.width, &self.images);
        model.app.clamp_viewport(&model.layout, body.height);
        model.generation = model.generation.saturating_add(1);
        model.desired_frame = empty_frame(model.generation, model.cell_pixels);
    }
}

fn update_desired_frame(model: &mut ReaderModel, body: CellRect) {
    if model.graphics_kind == GraphicsBackendKind::Text {
        model.desired_frame = empty_frame(model.generation, model.cell_pixels);
        return;
    }
    let (first, offset) = model.app.viewport_position(&model.layout);
    model.desired_frame =
        model
            .layout
            .desired_frame(first, offset, body, model.generation, model.cell_pixels);
}

fn submit_logical_render(model: &ReaderModel, body: CellRect) {
    let Some(renderer) = &model.logical_renderer else {
        return;
    };
    renderer.submit(RenderRequest {
        generation: model.generation,
        top_source: model.app.top_source(),
        top_row_offset: model.app.top_row_offset(),
        body,
        cell_pixels: model.cell_pixels,
    });
}

fn graphics_status(model: &ReaderModel) -> String {
    if let Some(error) = &model.graphics_diagnostic {
        return format!("{} ({error})", graphics_name(model.graphics_kind));
    }
    match (model.graphics_kind, model.diagnostic_count) {
        (GraphicsBackendKind::Kitty, count) if count > 0 => {
            format!("kitty ({count} image warning(s))")
        }
        (GraphicsBackendKind::Kitty, _) => "kitty".to_owned(),
        (GraphicsBackendKind::Sixel, count) if count > 0 => {
            format!("sixel experimental ({count} image warning(s))")
        }
        (GraphicsBackendKind::Sixel, _) => "sixel experimental".to_owned(),
        (GraphicsBackendKind::Text, count) if count > 0 => {
            format!("text ({count} image warning(s))")
        }
        (GraphicsBackendKind::Text, _) => "text".to_owned(),
    }
}

const fn graphics_name(kind: GraphicsBackendKind) -> &'static str {
    match kind {
        GraphicsBackendKind::Kitty => "kitty",
        GraphicsBackendKind::Sixel => "sixel",
        GraphicsBackendKind::Text => "text",
    }
}

fn surface_cell_pixels(surface: Surface) -> Option<PixelSize> {
    let pixels = surface.cell_pixels?;
    Some(PixelSize {
        width: u32::try_from(pixels.width()).ok()?,
        height: u32::try_from(pixels.height()).ok()?,
    })
}

fn convert_cell_pixels(pixels: urushi_terminal::PixelSize) -> Option<PixelSize> {
    Some(PixelSize {
        width: u32::try_from(pixels.width()).ok()?,
        height: u32::try_from(pixels.height()).ok()?,
    })
}

fn empty_frame(generation: u64, cell_pixels: PixelSize) -> DesiredFrame {
    DesiredFrame {
        generation,
        cell_pixels,
        placements: Vec::new(),
        assets: std::collections::HashMap::new(),
    }
}

fn needs_graphics(document: &Document) -> bool {
    !document.images().is_empty()
        || !document.headings().is_empty()
        || !document.tables().is_empty()
        || !document.math().is_empty()
}

#[derive(Clone, Copy, Debug)]
struct RenderRequest {
    generation: u64,
    top_source: usize,
    top_row_offset: u16,
    body: CellRect,
    cell_pixels: PixelSize,
}

struct PreparedRender {
    generation: u64,
    layout: Layout,
    frame: DesiredFrame,
    position: (usize, u16),
    body: CellRect,
}

#[derive(Default)]
struct LatestRequest {
    request: Option<RenderRequest>,
    shutdown: bool,
}

struct LogicalRenderer {
    mailbox: Arc<(Mutex<LatestRequest>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct PreparedSource {
    receiver: Arc<Mutex<Option<Receiver<PreparedRender>>>>,
}

impl PreparedSource {
    fn subscription(&self) -> Subscription<Message> {
        let receiver = Arc::clone(&self.receiver);
        Subscription::run_blocking_with(
            "raden-logical-renderer",
            Admission::latest(),
            move |sender| {
                let Some(receiver) = receiver
                    .lock()
                    .expect("logical renderer receiver poisoned")
                    .take()
                else {
                    return;
                };
                while let Ok(prepared) = receiver.recv() {
                    if sender.blocking_send(Message::Prepared(prepared)).is_err() {
                        break;
                    }
                }
            },
        )
    }
}

impl LogicalRenderer {
    fn spawn(
        document: Document,
        images: ImageCatalog,
        heading_fonts: HeadingFonts,
    ) -> (Self, PreparedSource) {
        let mailbox = Arc::new((Mutex::new(LatestRequest::default()), Condvar::new()));
        let worker_mailbox = Arc::clone(&mailbox);
        let (result_tx, result_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut cache = RasterCache::with_heading_fonts(heading_fonts);
            loop {
                let request = {
                    let (lock, ready) = &*worker_mailbox;
                    let mut state = lock.lock().expect("logical renderer mailbox poisoned");
                    while state.request.is_none() && !state.shutdown {
                        state = ready
                            .wait(state)
                            .expect("logical renderer mailbox poisoned");
                    }
                    if state.shutdown {
                        return;
                    }
                    state.request.take().expect("request became unavailable")
                };
                let layout = Layout::with_graphics(
                    &document,
                    request.body.width,
                    request.body.height,
                    &images,
                    &mut cache,
                    request.cell_pixels,
                    request.top_source,
                );
                let first = layout.row_for_source(request.top_source);
                let position = (
                    first,
                    request
                        .top_row_offset
                        .min(layout.rows()[first].height.saturating_sub(1)),
                );
                let frame = layout.desired_frame(
                    position.0,
                    position.1,
                    request.body,
                    request.generation,
                    request.cell_pixels,
                );
                let newer_pending = worker_mailbox
                    .0
                    .lock()
                    .expect("logical renderer mailbox poisoned")
                    .request
                    .is_some();
                if !newer_pending
                    && result_tx
                        .send(PreparedRender {
                            generation: request.generation,
                            layout,
                            frame,
                            position,
                            body: request.body,
                        })
                        .is_err()
                {
                    return;
                }
            }
        });
        (
            Self {
                mailbox,
                handle: Some(handle),
            },
            PreparedSource {
                receiver: Arc::new(Mutex::new(Some(result_rx))),
            },
        )
    }

    fn submit(&self, request: RenderRequest) {
        let (lock, ready) = &*self.mailbox;
        let mut state = lock.lock().expect("logical renderer mailbox poisoned");
        state.request = Some(request);
        ready.notify_one();
    }
}

impl Drop for LogicalRenderer {
    fn drop(&mut self) {
        let (lock, ready) = &*self.mailbox;
        let mut state = lock.lock().expect("logical renderer mailbox poisoned");
        state.shutdown = true;
        state.request = None;
        ready.notify_one();
        drop(state);
        // Rasterizers do not expose cooperative cancellation. Detach an
        // in-flight render so quitting never waits for stale work to finish.
        drop(self.handle.take());
    }
}

#[derive(Debug)]
enum GraphicsMode {
    Kitty(KittyBackend),
    Sixel(SixelBackend),
    Text,
}

impl GraphicsMode {
    fn from_kind(kind: GraphicsBackendKind) -> Self {
        match kind {
            GraphicsBackendKind::Kitty => Self::Kitty(KittyBackend::new()),
            GraphicsBackendKind::Sixel => Self::Sixel(SixelBackend::new()),
            GraphicsBackendKind::Text => Self::Text,
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::Kitty(_) => "kitty",
            Self::Sixel(_) => "sixel",
            Self::Text => "text",
        }
    }

    fn render<W: io::Write>(&mut self, frame: &DesiredFrame, writer: &mut W) -> io::Result<()> {
        match self {
            Self::Kitty(backend) => backend.render(frame, writer),
            Self::Sixel(backend) => backend.render(frame, writer),
            Self::Text => Ok(()),
        }
    }

    fn clear<W: io::Write>(&mut self, writer: &mut W) -> io::Result<()> {
        match self {
            Self::Kitty(backend) => backend.clear(writer),
            Self::Sixel(backend) => backend.clear(writer),
            Self::Text => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_only_documents_require_the_graphics_pipeline() {
        let table = Document::parse("| a | b |\n| --- | --- |\n| 1 | 2 |\n".to_owned());
        let plain = Document::parse("plain text".to_owned());

        assert!(needs_graphics(&table));
        assert!(!needs_graphics(&plain));
    }

    #[test]
    fn direct_frame_tracks_scrolling_before_a_new_raster_finishes() {
        let document = Document::parse("first\n\nsecond\n\nthird".to_owned());
        let layout = Layout::new(&document, 20);
        let mut model = ReaderModel {
            app: App::default(),
            layout,
            surface: Surface::new(20, 4),
            generation: 2,
            cell_pixels: PixelSize::FALLBACK,
            desired_frame: empty_frame(1, PixelSize::FALLBACK),
            graphics_kind: GraphicsBackendKind::Kitty,
            graphics_diagnostic: None,
            diagnostic_count: 0,
            logical_renderer: None,
            prepared_source: None,
            fatal_error: None,
        };
        model.app.apply(Command::Down, &model.layout, 2);
        update_desired_frame(
            &mut model,
            CellRect {
                x: 0,
                y: 1,
                width: 20,
                height: 2,
            },
        );

        assert_eq!(model.desired_frame.generation, 2);
    }
}
