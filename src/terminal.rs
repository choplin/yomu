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
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::graphics::{KittyBackend, kitty_environment_hint};
use crate::heading::HeadingFonts;
use crate::image::ImageCatalog;
use crate::{
    App, CellRect, Command, DesiredFrame, Document, Layout, PixelSize, RasterCache, config::Config,
    ui,
};

const INPUT_WAIT: Duration = Duration::from_millis(8);
const INPUT_BATCH_WINDOW: Duration = Duration::from_millis(2);
const MAX_INPUT_BATCH: usize = 64;

pub(crate) fn run(path: &Path, document: &Document, config: &Config) -> Result<()> {
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
    let run_result = run_loop(session.terminal_mut(), path, document, heading_fonts);
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

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    path: &Path,
    document: &Document,
    heading_fonts: HeadingFonts,
) -> Result<()> {
    let use_kitty = kitty_environment_hint() && needs_graphics(document);
    let images = ImageCatalog::load(document, path);
    let diagnostic_count = images.diagnostics().len();
    let mut logical_renderer =
        use_kitty.then(|| LogicalRenderer::spawn(document.clone(), images.clone(), heading_fonts));
    let mut graphics = if use_kitty {
        GraphicsMode::Kitty(KittyBackend::new())
    } else {
        GraphicsMode::Text
    };
    let run_result = drive_loop(
        terminal,
        path,
        document,
        &images,
        diagnostic_count,
        &mut logical_renderer,
        &mut graphics,
    );
    drop(logical_renderer);
    let clear_result = match &mut graphics {
        GraphicsMode::Kitty(backend) => backend
            .clear(&mut io::stdout())
            .context("could not clear Kitty images during shutdown"),
        GraphicsMode::Text => Ok(()),
    };
    combine_run_and_graphics_cleanup(run_result, clear_result)
}

fn needs_graphics(document: &Document) -> bool {
    !document.images().is_empty()
        || !document.headings().is_empty()
        || !document.tables().is_empty()
}

fn drive_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    path: &Path,
    document: &Document,
    images: &ImageCatalog,
    diagnostic_count: usize,
    logical_renderer: &mut Option<LogicalRenderer>,
    graphics: &mut GraphicsMode,
) -> Result<()> {
    let mut app = App::default();
    let mut generation = 1u64;
    let mut graphics_error = None;
    let initial_size = terminal.size().context("could not read terminal size")?;
    let mut layout = Layout::with_text(document, initial_size.width, images);
    submit_logical_render(logical_renderer, generation, &app, initial_size);
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
            let graphics_status = graphics.status(graphics_error.as_deref(), diagnostic_count);
            terminal
                .draw(|frame| ui::render(frame, path, &app, &layout, &graphics_status))
                .context("could not draw the Markdown document")?;
            dirty = false;
            if let Some(frame) = pending_frame.take() {
                let render_error = match graphics {
                    GraphicsMode::Kitty(backend) => backend.render(&frame, &mut io::stdout()).err(),
                    GraphicsMode::Text => None,
                };
                if let Some(error) = render_error {
                    if let GraphicsMode::Kitty(backend) = graphics {
                        backend.clear(&mut io::stdout()).with_context(|| {
                            format!(
                                "Kitty rendering failed: {error}; immediate cleanup also failed"
                            )
                        })?;
                    }
                    graphics_error = Some(format!("Kitty failed: {error}"));
                    let size = terminal
                        .size()
                        .context("could not read terminal size after graphics failure")?;
                    layout =
                        fall_back_to_text(graphics, logical_renderer, document, size.width, images);
                    pending_frame = None;
                    terminal
                        .clear()
                        .context("could not redraw after graphics failure")?;
                    dirty = true;
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
        if batch.resized
            && let GraphicsMode::Kitty(backend) = graphics
        {
            backend
                .clear(&mut io::stdout())
                .context("could not clear Kitty images before resize redraw")?;
        }
        generation = generation.saturating_add(1);
        let size = batch.size;
        let [_, body, _] = ui::viewport_areas(Rect::new(0, 0, size.width, size.height));
        let (first, first_row_offset) = app.viewport_position(&layout);
        pending_frame = Some(layout.desired_frame(
            first,
            first_row_offset,
            CellRect::from(body),
            generation,
            cell_pixel_size(size.width, size.height),
        ));
        submit_logical_render(logical_renderer, generation, &app, size);
        dirty = true;
    }
    Ok(())
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

fn fall_back_to_text(
    graphics: &mut GraphicsMode,
    logical_renderer: &mut Option<LogicalRenderer>,
    document: &Document,
    width: u16,
    images: &ImageCatalog,
) -> Layout {
    *graphics = GraphicsMode::Text;
    *logical_renderer = None;
    Layout::with_text(document, width, images)
}

fn submit_logical_render(
    renderer: &Option<LogicalRenderer>,
    generation: u64,
    app: &App,
    size: ratatui::layout::Size,
) {
    let Some(renderer) = renderer else { return };
    let [_, body, _] = ui::viewport_areas(Rect::new(0, 0, size.width, size.height));
    renderer.submit(RenderRequest {
        generation,
        top_source: app.top_source(),
        top_row_offset: app.top_row_offset(),
        size,
        body: CellRect::from(body),
        cell_pixels: cell_pixel_size(size.width, size.height),
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
    Text,
}

impl GraphicsMode {
    fn status(&self, error: Option<&str>, diagnostic_count: usize) -> String {
        if let Some(error) = error {
            return format!("text ({error})");
        }
        match self {
            Self::Kitty(_) if diagnostic_count > 0 => {
                format!("kitty ({diagnostic_count} image warning(s))")
            }
            Self::Kitty(_) => "kitty".to_owned(),
            Self::Text if diagnostic_count > 0 => {
                format!("text ({diagnostic_count} image warning(s))")
            }
            Self::Text => "text".to_owned(),
        }
    }
}

fn cell_pixel_size(columns: u16, rows: u16) -> PixelSize {
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

        let layout = fall_back_to_text(&mut graphics, &mut renderer, &document, 80, &images);
        let rendered = layout
            .rows()
            .iter()
            .flat_map(|row| row.fragments.iter())
            .map(|fragment| fragment.text.as_str())
            .collect::<String>();

        assert!(matches!(graphics, GraphicsMode::Text));
        assert!(renderer.is_none());
        assert!(rendered.contains("Readable heading"));
    }
}
