use std::io::{self, IsTerminal, Stdout};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;

use crate::graphics::{
    CapabilityEvidence, GraphicsBackendKind, GraphicsPreference, GraphicsSelection, KittyBackend,
    SixelBackend, select_graphics_backend,
};
use crate::heading::HeadingFonts;
use crate::image::ImageCatalog;
use crate::{
    App, CellRect, Command, DesiredFrame, Document, Layout, PixelSize, RasterCache, config::Config,
    ui,
};

const INPUT_WAIT: Duration = Duration::from_millis(8);
const INPUT_BATCH_WINDOW: Duration = Duration::from_millis(2);
const MAX_INPUT_BATCH: usize = 64;

pub(crate) fn run(
    path: &Path,
    document: &Document,
    config: &Config,
    preference: GraphicsPreference,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(
            "an interactive terminal is required (stdin and stdout must both be terminals)"
        ));
    }

    let heading_fonts = if config.heading_families().is_empty() {
        HeadingFonts::default()
    } else {
        HeadingFonts::load(config.heading_families()).map_err(anyhow::Error::msg)?
    };
    let mut session = TerminalSession::enter()?;
    let needs_graphics = needs_graphics(document);
    let should_probe = preference != GraphicsPreference::Text
        && (needs_graphics || preference != GraphicsPreference::Auto);
    let (evidence, probe_error) = if should_probe {
        match CapabilityEvidence::query_stdio() {
            Ok(evidence) => (evidence, None),
            Err(error) if preference == GraphicsPreference::Auto => (
                CapabilityEvidence::environment_fallback(),
                Some(format!("capability query failed: {error}")),
            ),
            Err(error) => {
                let restore_result = session.restore();
                let selection_error = anyhow!("could not query graphics capabilities: {error}");
                return combine_run_and_restore(Err(selection_error), restore_result);
            }
        }
    } else {
        (CapabilityEvidence::default(), None)
    };
    let GraphicsSelection {
        backend,
        reason: selection_reason,
    } = match select_graphics_backend(preference, evidence, needs_graphics) {
        Ok(selection) => selection,
        Err(error) => {
            let restore_result = session.restore();
            return combine_run_and_restore(Err(anyhow!(error)), restore_result);
        }
    };
    let initial_diagnostic = combine_diagnostics(selection_reason, probe_error);
    let run_result = run_loop(
        session.terminal_mut(),
        path,
        document,
        heading_fonts,
        backend,
        evidence.cell_pixels,
        initial_diagnostic,
    );
    let restore_result = session.restore();
    combine_run_and_restore(run_result, restore_result)
}

fn combine_run_and_restore(run_result: Result<()>, restore_result: Result<()>) -> Result<()> {
    match (run_result, restore_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(run_error), Ok(())) => Err(run_error),
        (Ok(()), Err(restore_error)) => Err(restore_error),
        (Err(run_error), Err(restore_error)) => Err(anyhow!(
            "{run_error:#}; terminal cleanup also failed: {restore_error:#}"
        )),
    }
}

fn combine_diagnostics(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first}; {second}")),
        (Some(diagnostic), None) | (None, Some(diagnostic)) => Some(diagnostic),
        (None, None) => None,
    }
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    path: &Path,
    document: &Document,
    heading_fonts: HeadingFonts,
    backend: GraphicsBackendKind,
    queried_cell_pixels: Option<PixelSize>,
    initial_diagnostic: Option<String>,
) -> Result<()> {
    let mut graphics = GraphicsMode::from_kind(backend);
    let images = ImageCatalog::load(document, path);
    let diagnostic_count = images.diagnostics().len();
    let mut logical_renderer = graphics
        .uses_rasters()
        .then(|| LogicalRenderer::spawn(document.clone(), images.clone(), heading_fonts));
    let context = DriveContext {
        path,
        document,
        images: &images,
        diagnostic_count,
        queried_cell_pixels,
        initial_diagnostic,
    };
    let run_result = drive_loop(terminal, context, &mut logical_renderer, &mut graphics);
    drop(logical_renderer);
    let clear_result = graphics.clear(&mut io::stdout()).with_context(|| {
        format!(
            "could not clear {} graphics during shutdown",
            graphics.name()
        )
    });
    combine_run_and_graphics_cleanup(run_result, clear_result)
}

fn needs_graphics(document: &Document) -> bool {
    !document.images().is_empty()
        || !document.headings().is_empty()
        || !document.tables().is_empty()
        || !document.math().is_empty()
}

struct DriveContext<'a> {
    path: &'a Path,
    document: &'a Document,
    images: &'a ImageCatalog,
    diagnostic_count: usize,
    queried_cell_pixels: Option<PixelSize>,
    initial_diagnostic: Option<String>,
}

fn drive_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    context: DriveContext<'_>,
    logical_renderer: &mut Option<LogicalRenderer>,
    graphics: &mut GraphicsMode,
) -> Result<()> {
    let DriveContext {
        path,
        document,
        images,
        diagnostic_count,
        queried_cell_pixels,
        initial_diagnostic,
    } = context;
    let mut app = App::default();
    let mut generation = 1u64;
    let mut graphics_diagnostic = initial_diagnostic;
    let initial_size = terminal.size().context("could not read terminal size")?;
    let mut layout = Layout::with_text(document, initial_size.width, images);
    submit_logical_render(
        logical_renderer,
        generation,
        &app,
        initial_size,
        queried_cell_pixels,
    );
    let mut dirty = true;
    let mut pending_frame = None;
    while !app.should_quit() {
        if let Some(prepared) = logical_renderer
            .as_ref()
            .and_then(LogicalRenderer::take_latest)
            && prepared.generation == generation
        {
            layout = prepared.layout;
            pending_frame = Some(prepared.frame);
            dirty = true;
        }

        if dirty {
            if graphics.requires_full_redraw() {
                terminal
                    .clear()
                    .context("could not clear the alternate screen for sixel redraw")?;
            }
            let graphics_status = graphics.status(graphics_diagnostic.as_deref(), diagnostic_count);
            terminal
                .draw(|frame| ui::render(frame, path, &app, &layout, &graphics_status))
                .context("could not draw the Markdown document")?;
            dirty = false;
            if let Some(frame) = pending_frame.take() {
                let backend_name = graphics.name();
                let render_error = graphics.render(&frame, &mut io::stdout()).err();
                if let Some(error) = render_error {
                    let size = terminal
                        .size()
                        .context("could not read terminal size after graphics failure")?;
                    let failure = GraphicsFailureContext {
                        path,
                        app: &app,
                        document,
                        images,
                        width: size.width,
                        diagnostic_count,
                        backend_name,
                        render_error: &error,
                    };
                    let fallback = handle_graphics_failure(
                        terminal,
                        graphics,
                        logical_renderer,
                        &mut io::stdout(),
                        failure,
                    )?;
                    graphics_diagnostic = Some(fallback.diagnostic);
                    layout = fallback.layout;
                    pending_frame = None;
                    dirty = false;
                }
            }
        }

        let events = read_input_batch(INPUT_WAIT)?;
        if events.is_empty() {
            continue;
        }
        let size = terminal.size().context("could not read terminal size")?;
        let batch = apply_input_batch(&mut app, &mut layout, document, images, size, events);
        if app.should_quit() || !batch.changed {
            continue;
        }
        if batch.resized {
            graphics.clear(&mut io::stdout()).with_context(|| {
                format!("could not clear {} before resize redraw", graphics.name())
            })?;
        }
        generation = generation.saturating_add(1);
        let size = batch.size;
        let [_, body, _] = ui::viewport_areas(Rect::new(0, 0, size.width, size.height));
        pending_frame = frame_before_logical_render(
            batch.resized,
            &app,
            &layout,
            CellRect::from(body),
            generation,
            cell_pixel_size(size.width, size.height, queried_cell_pixels),
        );
        submit_logical_render(
            logical_renderer,
            generation,
            &app,
            size,
            queried_cell_pixels,
        );
        dirty = true;
    }
    Ok(())
}

fn frame_before_logical_render(
    resized: bool,
    app: &App,
    layout: &Layout,
    body: CellRect,
    generation: u64,
    cell_pixels: PixelSize,
) -> Option<DesiredFrame> {
    if resized {
        // The current layout is the text projection rebuilt at the new width.
        // Wait for the logical renderer rather than re-placing old raster
        // assets with new cell metrics after the backend was cleared.
        return None;
    }
    let (first, first_row_offset) = app.viewport_position(layout);
    Some(layout.desired_frame(first, first_row_offset, body, generation, cell_pixels))
}

fn read_input_batch(wait: Duration) -> Result<Vec<Event>> {
    if !event::poll(wait).context("could not poll terminal input")? {
        return Ok(Vec::new());
    }
    let deadline = Instant::now() + INPUT_BATCH_WINDOW;
    let mut events = Vec::with_capacity(MAX_INPUT_BATCH);
    loop {
        events.push(event::read().context("could not read terminal input")?);
        if events.len() >= MAX_INPUT_BATCH
            || Instant::now() >= deadline
            || !event::poll(Duration::ZERO).context("could not poll terminal input")?
        {
            return Ok(events);
        }
    }
}

fn apply_input_batch(
    app: &mut App,
    layout: &mut Layout,
    document: &Document,
    images: &ImageCatalog,
    mut size: ratatui::layout::Size,
    events: impl IntoIterator<Item = Event>,
) -> InputBatchResult {
    let mut result = InputBatchResult {
        size,
        changed: false,
        resized: false,
    };
    for input in events {
        match input {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                let command = Command::from_key(key);
                if command == Command::None {
                    continue;
                }
                let [_, body, _] = ui::viewport_areas(Rect::new(0, 0, size.width, size.height));
                app.apply(command, layout, body.height);
                result.changed = true;
                if app.should_quit() {
                    break;
                }
            }
            Event::Resize(width, height) => {
                size = ratatui::layout::Size { width, height };
                *layout = Layout::with_text(document, width, images);
                result.size = size;
                result.changed = true;
                result.resized = true;
            }
            _ => {}
        }
    }
    result
}

struct GraphicsFailureContext<'a> {
    path: &'a Path,
    app: &'a App,
    document: &'a Document,
    images: &'a ImageCatalog,
    width: u16,
    diagnostic_count: usize,
    backend_name: &'static str,
    render_error: &'a io::Error,
}

fn handle_graphics_failure<B: Backend>(
    terminal: &mut Terminal<B>,
    graphics: &mut GraphicsMode,
    logical_renderer: &mut Option<LogicalRenderer>,
    writer: &mut impl io::Write,
    context: GraphicsFailureContext<'_>,
) -> Result<TextFallback> {
    let cleanup_error = graphics.clear(writer).err();
    *graphics = GraphicsMode::Text;
    *logical_renderer = None;
    let layout = Layout::with_text(context.document, context.width, context.images);
    let diagnostic = graphics_failure_diagnostic(
        context.backend_name,
        context.render_error,
        cleanup_error.as_ref(),
    );
    terminal
        .clear()
        .map_err(|error| anyhow!("could not clear before text fallback: {error}"))?;
    let status = graphics.status(Some(&diagnostic), context.diagnostic_count);
    terminal
        .draw(|frame| ui::render(frame, context.path, context.app, &layout, &status))
        .map_err(|error| anyhow!("could not redraw text fallback: {error}"))?;
    Ok(TextFallback { layout, diagnostic })
}

#[derive(Debug)]
struct TextFallback {
    layout: Layout,
    diagnostic: String,
}

fn submit_logical_render(
    renderer: &Option<LogicalRenderer>,
    generation: u64,
    app: &App,
    size: ratatui::layout::Size,
    queried_cell_pixels: Option<PixelSize>,
) {
    let Some(renderer) = renderer else { return };
    let [_, body, _] = ui::viewport_areas(Rect::new(0, 0, size.width, size.height));
    renderer.submit(RenderRequest {
        generation,
        top_source: app.top_source(),
        top_row_offset: app.top_row_offset(),
        size,
        body: CellRect::from(body),
        cell_pixels: cell_pixel_size(size.width, size.height, queried_cell_pixels),
    });
}

#[derive(Clone, Copy, Debug)]
struct RenderRequest {
    generation: u64,
    top_source: usize,
    top_row_offset: u16,
    size: ratatui::layout::Size,
    body: CellRect,
    cell_pixels: PixelSize,
}

#[derive(Debug)]
struct PreparedRender {
    generation: u64,
    layout: Layout,
    frame: DesiredFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InputBatchResult {
    size: ratatui::layout::Size,
    changed: bool,
    resized: bool,
}

#[derive(Debug, Default)]
struct LatestRequest {
    request: Option<RenderRequest>,
    shutdown: bool,
}

#[derive(Debug)]
struct LogicalRenderer {
    mailbox: Arc<(Mutex<LatestRequest>, Condvar)>,
    result: Arc<Mutex<Option<PreparedRender>>>,
    handle: Option<JoinHandle<()>>,
}

impl LogicalRenderer {
    fn spawn(document: Document, images: ImageCatalog, heading_fonts: HeadingFonts) -> Self {
        let mailbox = Arc::new((Mutex::new(LatestRequest::default()), Condvar::new()));
        let worker_mailbox = Arc::clone(&mailbox);
        let result = Arc::new(Mutex::new(None));
        let worker_result = Arc::clone(&result);
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
                    request.size.width,
                    request.body.height,
                    &images,
                    &mut cache,
                    request.cell_pixels,
                    request.top_source,
                );
                let first = layout.row_for_source(request.top_source);
                let frame = layout.desired_frame(
                    first,
                    request.top_row_offset,
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
                if !newer_pending {
                    *worker_result
                        .lock()
                        .expect("logical renderer result poisoned") = Some(PreparedRender {
                        generation: request.generation,
                        layout,
                        frame,
                    });
                }
            }
        });
        Self {
            mailbox,
            result,
            handle: Some(handle),
        }
    }

    fn submit(&self, request: RenderRequest) {
        let (lock, ready) = &*self.mailbox;
        let mut state = lock.lock().expect("logical renderer mailbox poisoned");
        state.replace(request);
        ready.notify_one();
    }

    fn take_latest(&self) -> Option<PreparedRender> {
        self.result
            .lock()
            .expect("logical renderer result poisoned")
            .take()
    }
}

impl LatestRequest {
    fn replace(&mut self, request: RenderRequest) {
        self.request = Some(request);
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
        // Decoders do not expose cooperative cancellation. Detach an in-flight
        // decode so quitting never waits for stale logical work to finish.
        drop(self.handle.take());
    }
}

impl From<Rect> for CellRect {
    fn from(rect: Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

fn combine_run_and_graphics_cleanup(
    run_result: Result<()>,
    cleanup_result: Result<()>,
) -> Result<()> {
    match (run_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(run_error), Ok(())) => Err(run_error),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error),
        (Err(run_error), Err(cleanup_error)) => Err(anyhow!(
            "{run_error:#}; graphics cleanup also failed: {cleanup_error:#}"
        )),
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

    fn uses_rasters(&self) -> bool {
        !matches!(self, Self::Text)
    }

    fn requires_full_redraw(&self) -> bool {
        matches!(self, Self::Sixel(_))
    }

    fn name(&self) -> &'static str {
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

    fn status(&self, error: Option<&str>, diagnostic_count: usize) -> String {
        if let Some(error) = error {
            return format!("{} ({error})", self.name());
        }
        match self {
            Self::Kitty(_) if diagnostic_count > 0 => {
                format!("kitty ({diagnostic_count} image warning(s))")
            }
            Self::Kitty(_) => "kitty".to_owned(),
            Self::Sixel(_) if diagnostic_count > 0 => {
                format!("sixel experimental ({diagnostic_count} image warning(s))")
            }
            Self::Sixel(_) => "sixel experimental".to_owned(),
            Self::Text if diagnostic_count > 0 => {
                format!("text ({diagnostic_count} image warning(s))")
            }
            Self::Text => "text".to_owned(),
        }
    }
}

fn graphics_failure_diagnostic(
    backend: &str,
    render_error: &io::Error,
    cleanup_error: Option<&io::Error>,
) -> String {
    match cleanup_error {
        Some(cleanup_error) => {
            format!("{backend} failed: {render_error}; cleanup also failed: {cleanup_error}")
        }
        None => format!("{backend} failed: {render_error}"),
    }
}

fn cell_pixel_size(columns: u16, rows: u16, queried_cell_pixels: Option<PixelSize>) -> PixelSize {
    if let Some(cell_pixels) = queried_cell_pixels {
        return cell_pixels;
    }
    crossterm::terminal::window_size()
        .ok()
        .and_then(|window| {
            let width = u32::from(window.width).checked_div(u32::from(columns.max(1)))?;
            let height = u32::from(window.height).checked_div(u32::from(rows.max(1)))?;
            (width > 0 && height > 0).then_some(PixelSize { width, height })
        })
        .unwrap_or(PixelSize::FALLBACK)
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    cursor_needs_restoring: bool,
    alternate_screen_active: bool,
    raw_mode_active: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("could not enable terminal raw mode")?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            return match cleanup_setup(&mut stdout) {
                Ok(()) => Err(error).context("could not enter alternate screen"),
                Err(cleanup_error) => Err(error).context(format!(
                    "could not enter alternate screen; cleanup also failed: {cleanup_error:#}"
                )),
            };
        }

        let backend = CrosstermBackend::new(stdout);
        let terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let mut stdout = io::stdout();
                return match cleanup_setup(&mut stdout) {
                    Ok(()) => Err(error).context("could not initialize terminal"),
                    Err(cleanup_error) => Err(error).context(format!(
                        "could not initialize terminal; cleanup also failed: {cleanup_error:#}"
                    )),
                };
            }
        };

        Ok(Self {
            terminal,
            cursor_needs_restoring: true,
            alternate_screen_active: true,
            raw_mode_active: true,
        })
    }

    fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    fn restore(&mut self) -> Result<()> {
        let mut failures = Vec::new();

        if self.cursor_needs_restoring {
            match self.terminal.show_cursor() {
                Ok(()) => self.cursor_needs_restoring = false,
                Err(error) => failures.push(format!("could not restore terminal cursor: {error}")),
            }
        }

        if self.alternate_screen_active {
            match execute!(self.terminal.backend_mut(), LeaveAlternateScreen) {
                Ok(()) => self.alternate_screen_active = false,
                Err(error) => failures.push(format!("could not leave alternate screen: {error}")),
            }
        }

        if self.raw_mode_active {
            match disable_raw_mode() {
                Ok(()) => self.raw_mode_active = false,
                Err(error) => {
                    failures.push(format!("could not disable terminal raw mode: {error}"))
                }
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(failures.join("; ")))
        }
    }
}

fn cleanup_setup(stdout: &mut Stdout) -> Result<()> {
    let mut failures = Vec::new();
    if let Err(error) = execute!(stdout, LeaveAlternateScreen) {
        failures.push(format!("could not leave alternate screen: {error}"));
    }
    if let Err(error) = disable_raw_mode() {
        failures.push(format!("could not disable terminal raw mode: {error}"));
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(failures.join("; ")))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("raden: terminal cleanup failed: {error:#}");
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
    fn explicit_sixel_override_is_experimental_and_uses_rasters() {
        let graphics = GraphicsMode::from_kind(GraphicsBackendKind::Sixel);

        assert!(matches!(graphics, GraphicsMode::Sixel(_)));
        assert!(graphics.uses_rasters());
        assert!(graphics.requires_full_redraw());
        assert_eq!(graphics.status(None, 0), "sixel experimental");
    }

    #[test]
    fn text_selection_does_not_start_an_explicit_graphics_backend() {
        let graphics = GraphicsMode::from_kind(GraphicsBackendKind::Text);

        assert!(matches!(graphics, GraphicsMode::Text));
    }

    #[test]
    fn status_keeps_the_active_backend_when_reporting_probe_failure() {
        let graphics = GraphicsMode::from_kind(GraphicsBackendKind::Kitty);

        assert_eq!(
            graphics.status(Some("capability query failed: injected"), 0),
            "kitty (capability query failed: injected)"
        );
    }

    #[test]
    fn resize_waits_for_new_rasters_instead_of_replacing_stale_assets() {
        let document = Document::parse("$x^2$".to_owned());
        let layout = Layout::new(&document, 80);
        let body = CellRect {
            x: 0,
            y: 1,
            width: 80,
            height: 20,
        };

        assert!(
            frame_before_logical_render(
                true,
                &App::default(),
                &layout,
                body,
                2,
                PixelSize {
                    width: 10,
                    height: 20,
                },
            )
            .is_none()
        );
    }

    #[test]
    fn graphics_cleanup_failure_is_reported_without_hiding_render_failure() {
        let render = io::Error::other("render broke");
        let cleanup = io::Error::other("clear broke");

        let diagnostic = graphics_failure_diagnostic("sixel", &render, Some(&cleanup));

        assert!(diagnostic.contains("sixel failed: render broke"));
        assert!(diagnostic.contains("cleanup also failed: clear broke"));
    }

    #[test]
    fn reports_both_run_and_cleanup_failures() {
        let error = combine_run_and_restore(
            Err(anyhow!("input failed")),
            Err(anyhow!("raw mode stayed active")),
        )
        .expect_err("both failures must be reported");
        let message = format!("{error:#}");

        assert!(message.contains("input failed"));
        assert!(message.contains("raw mode stayed active"));
    }

    #[test]
    fn reports_both_run_and_graphics_cleanup_failures() {
        let error = combine_run_and_graphics_cleanup(
            Err(anyhow!("input failed")),
            Err(anyhow!("Kitty delete failed")),
        )
        .expect_err("both failures must be reported");
        let message = format!("{error:#}");

        assert!(message.contains("input failed"));
        assert!(message.contains("Kitty delete failed"));
    }

    #[test]
    fn logical_mailbox_keeps_only_the_latest_generation() {
        let request = |generation| RenderRequest {
            generation,
            top_source: generation as usize,
            top_row_offset: 0,
            size: ratatui::layout::Size {
                width: 80,
                height: 24,
            },
            body: CellRect {
                x: 0,
                y: 1,
                width: 80,
                height: 22,
            },
            cell_pixels: PixelSize::FALLBACK,
        };
        let mut mailbox = LatestRequest::default();

        mailbox.replace(request(1));
        mailbox.replace(request(2));
        mailbox.replace(request(3));

        let pending = mailbox
            .request
            .expect("latest request should remain pending");
        assert_eq!(pending.generation, 3);
        assert_eq!(pending.top_source, 3);
    }

    #[test]
    fn repeated_keys_are_applied_in_one_input_batch_without_losing_distance() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let source = (0..100)
            .map(|index| format!("line {index}\n\n"))
            .collect::<String>();
        let document = Document::parse(source);
        let images = ImageCatalog::load(&document, Path::new("doc.md"));
        let mut batched_layout = Layout::with_text(&document, 80, &images);
        let mut batched = App::default();
        let repeat = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        let events = std::iter::repeat_n(repeat, 20).collect::<Vec<_>>();
        let size = ratatui::layout::Size {
            width: 80,
            height: 24,
        };

        let result = apply_input_batch(
            &mut batched,
            &mut batched_layout,
            &document,
            &images,
            size,
            events,
        );
        let mut sequential = App::default();
        for _ in 0..20 {
            sequential.apply(Command::Down, &batched_layout, 22);
        }

        assert_eq!(
            result,
            InputBatchResult {
                size,
                changed: true,
                resized: false,
            }
        );
        assert_eq!(batched.top_source(), sequential.top_source());
        assert_eq!(batched.top_row_offset(), sequential.top_row_offset());
    }

    #[test]
    fn graphics_failure_rebuilds_large_headings_as_text() {
        let document = Document::parse("# Readable heading\n".to_owned());
        let images = ImageCatalog::load(&document, Path::new("doc.md"));
        let mut graphics = GraphicsMode::Kitty(KittyBackend::new());
        let mut renderer = None;
        let mut cleanup = Vec::new();
        let app = App::default();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24))
            .expect("test terminal should be created");
        let render_error = io::Error::other("render broke");
        let fallback = handle_graphics_failure(
            &mut terminal,
            &mut graphics,
            &mut renderer,
            &mut cleanup,
            GraphicsFailureContext {
                path: Path::new("doc.md"),
                app: &app,
                document: &document,
                images: &images,
                width: 80,
                diagnostic_count: 0,
                backend_name: "kitty",
                render_error: &render_error,
            },
        )
        .expect("text fallback should be redrawn");
        let rendered = fallback
            .layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        assert!(matches!(graphics, GraphicsMode::Text));
        assert!(renderer.is_none());
        assert!(fallback.diagnostic.contains("kitty failed: render broke"));
        assert!(rendered.contains("Readable heading"));
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(screen.contains("Readable heading"));
    }

    #[test]
    fn sixel_failure_clears_before_resetting_and_relayout_to_text() {
        let document = Document::parse(
            "before the heading\n\n# Readable heading\n\nafter the heading\n".to_owned(),
        );
        let images = ImageCatalog::load(&document, Path::new("doc.md"));
        let graphics_layout = Layout::with_text(&document, 24, &images);
        let mut app = App::default();
        app.apply(Command::Down, &graphics_layout, 10);
        let source_anchor = app.top_source();
        let mut graphics = GraphicsMode::Sixel(SixelBackend::new());
        let mut renderer = None;
        let mut cleanup = Vec::new();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(24, 12))
            .expect("test terminal should be created");
        let render_error = io::Error::other("render broke");
        let fallback = handle_graphics_failure(
            &mut terminal,
            &mut graphics,
            &mut renderer,
            &mut cleanup,
            GraphicsFailureContext {
                path: Path::new("doc.md"),
                app: &app,
                document: &document,
                images: &images,
                width: 24,
                diagnostic_count: 0,
                backend_name: "sixel",
                render_error: &render_error,
            },
        )
        .expect("text fallback should be redrawn");

        assert_eq!(cleanup, b"\x1b[2J\x1b[H");
        assert!(matches!(graphics, GraphicsMode::Text));
        assert!(renderer.is_none());
        assert_eq!(app.top_source(), source_anchor);
        assert_eq!(
            fallback.layout.rows()[fallback.layout.row_for_source(source_anchor)]
                .source
                .start,
            source_anchor
        );
        assert!(fallback.diagnostic.contains("sixel failed: render broke"));
        assert!(
            fallback
                .layout
                .rows()
                .iter()
                .flat_map(|row| &row.fragments)
                .any(|fragment| fragment.text.contains("Readable heading"))
        );
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(screen.contains("Readable heading"));
    }
}
