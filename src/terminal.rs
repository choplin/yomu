use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use urushi::View;
use urushi_graphics::{GraphicsSelection, select_graphics};
use urushi_terminal::{TerminalQuery, backend::native::NativeTerminal};
use urushi_tui_app::{
    Application, Effect, Input, KeyCode, KeyKind, Modifiers, Runtime, Subscription, Surface,
};

use crate::image::ImageCatalog;
use crate::layout::{GraphicsPreparationState, PreparedGraphics};
use crate::{
    App, Command, Document, GraphicsPreference, Layout, PixelSize, RasterCache, config::Config, ui,
};

pub(crate) fn run(
    path: &Path,
    document: &Document,
    config: &Config,
    preference: GraphicsPreference,
    theme_name: Option<&str>,
) -> Result<()> {
    if !io::stdout().is_terminal() {
        return Err(anyhow!("an interactive terminal is required on stdout"));
    }
    let mut terminal = NativeTerminal::open().context("could not open the controlling terminal")?;
    let theme = resolve_theme(config, theme_name, &mut terminal)?;
    let graphics = resolve_graphics(preference, &mut terminal)?;
    let rasterize = graphics != GraphicsSelection::Text && theme.uses_rasterization();
    let application = ReaderApplication {
        path: path.to_path_buf(),
        document: Arc::new(document.clone()),
        images: Arc::new(ImageCatalog::load(document, path)),
        raster_cache: Arc::new(Mutex::new(RasterCache::default())),
        latest_generation: Arc::new(AtomicU64::new(0)),
        max_width: config.body_max_width(),
        search_case: config.search_case(),
        rasterize,
        graphics_label: selection_label(graphics),
        theme: Arc::new(theme),
    };
    Runtime::new(application)
        .backend(terminal)
        .graphics(preference.into())
        .bracketed_paste(false)
        .focus_change(false)
        .keyboard_enhancement(None)
        .run()
        .context("terminal application failed")?;
    Ok(())
}

fn resolve_theme(
    config: &Config,
    theme_name: Option<&str>,
    terminal: &mut impl TerminalQuery,
) -> Result<crate::theme::Theme> {
    let background = if config.needs_terminal_background(theme_name) {
        terminal
            .terminal_background()
            .context("could not query terminal background for automatic Theme selection")?
    } else {
        None
    };
    config.resolve_theme(theme_name, background)
}

fn resolve_graphics(
    preference: GraphicsPreference,
    terminal: &mut impl TerminalQuery,
) -> Result<GraphicsSelection> {
    if preference == GraphicsPreference::Text {
        return Ok(GraphicsSelection::Text);
    }
    let window = terminal
        .window_size()
        .context("could not query terminal geometry for graphics selection")?;
    let capabilities = terminal
        .terminal_capabilities()
        .context("could not query terminal graphics capabilities")?;
    select_graphics(preference.into(), capabilities, window.cell_pixels())
        .map_err(|error| anyhow!("{error}; try --graphics auto or --graphics text"))
}

struct ReaderApplication {
    path: PathBuf,
    document: Arc<Document>,
    images: Arc<ImageCatalog>,
    raster_cache: Arc<Mutex<RasterCache>>,
    latest_generation: Arc<AtomicU64>,
    max_width: Option<u16>,
    search_case: crate::app::SearchCase,
    rasterize: bool,
    graphics_label: &'static str,
    theme: Arc<crate::theme::Theme>,
}

struct ReaderModel {
    app: App,
    search_input: Option<String>,
    prepared_graphics: Option<PreparedGraphics>,
    surface: Surface,
    generation: u64,
    cell_pixels: PixelSize,
    graphics_label: &'static str,
    terminal_diagnostic: Option<String>,
    graphics_diagnostic: Option<String>,
}

enum Message {
    Input(Input),
    Surface(Surface),
    Prepared(Option<PreparedGraphicsResult>),
    TerminalError(io::Error),
}

impl Application for ReaderApplication {
    type Model = ReaderModel;
    type Message = Message;

    fn init(&self) -> (Self::Model, Effect<Self::Message>) {
        let surface = Surface::new(1, 1);
        (
            ReaderModel {
                app: App::default(),
                search_input: None,
                prepared_graphics: None,
                surface,
                generation: 0,
                cell_pixels: PixelSize::FALLBACK,
                graphics_label: self.graphics_label,
                terminal_diagnostic: None,
                graphics_diagnostic: None,
            },
            Effect::none(),
        )
    }

    fn update(&self, model: &mut Self::Model, message: Self::Message) -> Effect<Self::Message> {
        match message {
            Message::Input(Input::Key(key)) if key.kind != KeyKind::Release => {
                if let Some(query) = &mut model.search_input {
                    match key.code {
                        KeyCode::Escape => model.search_input = None,
                        KeyCode::Enter => {
                            let query = model.search_input.take().unwrap_or_default();
                            if !query.is_empty() {
                                model
                                    .app
                                    .search(self.document.source(), query, self.search_case);
                                return self.after_navigation(model);
                            }
                        }
                        KeyCode::Backspace => {
                            query.pop();
                        }
                        KeyCode::Char(character)
                            if !key.modifiers.contains(Modifiers::CONTROL)
                                && !key.modifiers.contains(Modifiers::ALT)
                                && !key.modifiers.contains(Modifiers::SUPER)
                                && !key.modifiers.contains(Modifiers::HYPER)
                                && !key.modifiers.contains(Modifiers::META) =>
                        {
                            query.push(character);
                        }
                        _ => {}
                    }
                    return Effect::none();
                }
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Escape) {
                    return Effect::shutdown();
                }
                match key.code {
                    KeyCode::Char('/') => {
                        model.search_input = Some(String::new());
                        return Effect::none();
                    }
                    KeyCode::Char('n') => {
                        model.app.next_match();
                        return self.after_navigation(model);
                    }
                    KeyCode::Char('N') => {
                        model.app.previous_match();
                        return self.after_navigation(model);
                    }
                    _ => {}
                }
                let command = Command::from_key(key);
                if command == Command::None {
                    return Effect::none();
                }
                let body = ui::viewport_areas(model.surface.size, self.max_width)[1];
                let layout = self.layout(model);
                model.app.apply(command, &layout, body.height);
                model.generation = model.generation.saturating_add(1);
                self.prepare_graphics(model, body)
            }
            Message::Input(_) => Effect::none(),
            Message::Surface(surface) => {
                model.surface = surface;
                model.cell_pixels = surface_cell_pixels(surface).unwrap_or(PixelSize::FALLBACK);
                let body = ui::viewport_areas(surface.size, self.max_width)[1];
                model.prepared_graphics = None;
                model.generation = model.generation.saturating_add(1);
                self.prepare_graphics(model, body)
            }
            Message::Prepared(prepared) => self.apply_prepared(model, prepared),
            Message::TerminalError(error) => {
                model.terminal_diagnostic = Some(error.to_string());
                model.graphics_label = selection_label(GraphicsSelection::Text);
                Effect::none()
            }
        }
    }

    fn view(&self, model: &Self::Model) -> View {
        let status = self.graphics_status(model);
        let layout = self.layout(model);
        ui::view(
            &self.path,
            &model.app,
            &layout,
            (&status, model.search_input.as_deref()),
            model.surface.size,
            self.max_width,
            &self.theme,
        )
    }

    fn subscriptions(&self, _model: &Self::Model) -> Subscription<Self::Message> {
        Subscription::batch([
            Subscription::surface(Message::Surface),
            Subscription::input(Message::Input),
            Subscription::terminal_errors(Message::TerminalError),
        ])
    }
}

impl ReaderApplication {
    fn after_navigation(&self, model: &mut ReaderModel) -> Effect<Message> {
        let body = ui::viewport_areas(model.surface.size, self.max_width)[1];
        let layout = self.layout(model);
        model.app.clamp_viewport(&layout, body.height);
        model.generation = model.generation.saturating_add(1);
        self.prepare_graphics(model, body)
    }

    fn apply_prepared(
        &self,
        model: &mut ReaderModel,
        prepared: Option<PreparedGraphicsResult>,
    ) -> Effect<Message> {
        let Some(prepared) = prepared.filter(|result| result.generation == model.generation) else {
            return Effect::none();
        };
        model
            .prepared_graphics
            .get_or_insert_default()
            .merge(prepared.graphics);
        let complete = prepared.state.is_complete();
        if complete {
            model
                .prepared_graphics
                .get_or_insert_default()
                .retain_targets(&prepared.state);
        }
        model.graphics_diagnostic = prepared.diagnostic;
        let layout = self.layout(model);
        let body = ui::viewport_areas(model.surface.size, self.max_width)[1];
        model.app.clamp_viewport(&layout, body.height);
        if complete {
            Effect::none()
        } else {
            self.prepare_graphics_step(model, body, prepared.state)
        }
    }

    fn layout(&self, model: &ReaderModel) -> Layout {
        let body = ui::viewport_areas(model.surface.size, self.max_width)[1];
        model.prepared_graphics.as_ref().map_or_else(
            || Layout::with_text_themed(&self.document, body.width, &self.images, &self.theme),
            |graphics| {
                Layout::with_prepared_themed(
                    &self.document,
                    body.width,
                    &self.images,
                    graphics,
                    &self.theme,
                )
            },
        )
    }

    fn prepare_graphics(&self, model: &ReaderModel, body: crate::CellRect) -> Effect<Message> {
        if !self.rasterize {
            return Effect::none();
        }
        let document = Arc::clone(&self.document);
        let images = Arc::clone(&self.images);
        let raster_cache = Arc::clone(&self.raster_cache);
        let latest_generation = Arc::clone(&self.latest_generation);
        let generation = model.generation;
        latest_generation.store(generation, Ordering::Release);
        let focus_source = model.app.top_source();
        let theme = Arc::clone(&self.theme);
        let state = GraphicsPreparationState::new(&document, focus_source, &theme);
        self.prepare_graphics_step_with_context(
            model,
            body,
            document,
            images,
            raster_cache,
            latest_generation,
            theme,
            state,
        )
    }

    fn prepare_graphics_step(
        &self,
        model: &ReaderModel,
        body: crate::CellRect,
        state: GraphicsPreparationState,
    ) -> Effect<Message> {
        self.prepare_graphics_step_with_context(
            model,
            body,
            Arc::clone(&self.document),
            Arc::clone(&self.images),
            Arc::clone(&self.raster_cache),
            Arc::clone(&self.latest_generation),
            Arc::clone(&self.theme),
            state,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_graphics_step_with_context(
        &self,
        model: &ReaderModel,
        body: crate::CellRect,
        document: Arc<Document>,
        images: Arc<ImageCatalog>,
        raster_cache: Arc<Mutex<RasterCache>>,
        latest_generation: Arc<AtomicU64>,
        theme: Arc<crate::theme::Theme>,
        state: GraphicsPreparationState,
    ) -> Effect<Message> {
        let generation = model.generation;
        let cell_pixels = model.cell_pixels;
        Effect::perform_latest("yomu-rasterize", move || {
            Message::Prepared(prepare_graphics_result(GraphicsPreparation {
                document: &document,
                images: &images,
                raster_cache: &raster_cache,
                latest_generation: &latest_generation,
                generation,
                width: body.width,
                height: body.height,
                cell_pixels,
                theme: &theme,
                state,
            }))
        })
    }

    fn graphics_status(&self, model: &ReaderModel) -> String {
        if let Some(error) = &model.terminal_diagnostic {
            return format!(
                "graphics {}; terminal warning ({error}); theme {}",
                model.graphics_label, self.theme.name
            );
        }
        let warnings = self.images.diagnostics().len();
        let mut diagnostics = Vec::new();
        if warnings > 0 {
            diagnostics.push(format!("{warnings} image warning(s)"));
        }
        if let Some(diagnostic) = &model.graphics_diagnostic {
            diagnostics.push(diagnostic.clone());
        }
        if diagnostics.is_empty() {
            format!(
                "graphics {}; theme {}",
                model.graphics_label, self.theme.name
            )
        } else {
            format!(
                "graphics {} ({}); theme {}",
                model.graphics_label,
                diagnostics.join("; "),
                self.theme.name
            )
        }
    }
}

#[derive(Clone)]
struct PreparedGraphicsResult {
    generation: u64,
    graphics: PreparedGraphics,
    diagnostic: Option<String>,
    state: GraphicsPreparationState,
}

struct GraphicsPreparation<'a> {
    document: &'a Document,
    images: &'a ImageCatalog,
    raster_cache: &'a Mutex<RasterCache>,
    latest_generation: &'a AtomicU64,
    generation: u64,
    width: u16,
    height: u16,
    cell_pixels: PixelSize,
    theme: &'a crate::theme::Theme,
    state: GraphicsPreparationState,
}

fn prepare_graphics_result(request: GraphicsPreparation<'_>) -> Option<PreparedGraphicsResult> {
    if request.latest_generation.load(Ordering::Acquire) != request.generation {
        return None;
    }
    let mut cache = request.raster_cache.lock().expect("raster cache poisoned");
    if request.latest_generation.load(Ordering::Acquire) != request.generation {
        return None;
    }
    let mut state = request.state;
    let graphics = PreparedGraphics::prepare_next_while(
        request.document,
        (request.width, request.height),
        request.images,
        &mut cache,
        request.cell_pixels,
        request.theme,
        &mut state,
        || request.latest_generation.load(Ordering::Acquire) == request.generation,
    );
    if request.latest_generation.load(Ordering::Acquire) != request.generation {
        return None;
    }
    let diagnostic = cache.heading_font_diagnostic();
    Some(PreparedGraphicsResult {
        generation: request.generation,
        graphics,
        diagnostic,
        state,
    })
}

fn surface_cell_pixels(surface: Surface) -> Option<PixelSize> {
    let pixels = surface.cell_pixels?;
    Some(PixelSize {
        width: u32::try_from(pixels.width()).ok()?,
        height: u32::try_from(pixels.height()).ok()?,
    })
}

const fn selection_label(selection: GraphicsSelection) -> &'static str {
    match selection {
        GraphicsSelection::Kitty => "kitty",
        GraphicsSelection::Sixel => "sixel experimental",
        GraphicsSelection::Text => "text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BackgroundTerminal {
        response: Option<urushi_terminal::TerminalBackground>,
        capabilities: urushi_terminal::TerminalCapabilities,
        window_pixels: Option<urushi_terminal::PixelSize>,
        query_count: usize,
    }

    impl BackgroundTerminal {
        const fn new(response: Option<urushi_terminal::TerminalBackground>) -> Self {
            Self {
                response,
                capabilities: urushi_terminal::TerminalCapabilities::none(),
                window_pixels: None,
                query_count: 0,
            }
        }
    }

    impl TerminalQuery for BackgroundTerminal {
        fn terminal_size(&mut self) -> io::Result<urushi_terminal::TerminalSize> {
            Ok(urushi_terminal::TerminalSize::new(80, 24))
        }

        fn cursor_position(&mut self) -> io::Result<urushi_terminal::Position> {
            Ok(urushi_terminal::Position::new(0, 0))
        }

        fn window_size(&mut self) -> io::Result<urushi_terminal::WindowSize> {
            Ok(urushi_terminal::WindowSize::new(
                urushi_terminal::TerminalSize::new(80, 24),
                self.window_pixels,
            ))
        }

        fn raw_mode_enabled(&mut self) -> io::Result<bool> {
            Ok(false)
        }

        fn terminal_background(
            &mut self,
        ) -> io::Result<Option<urushi_terminal::TerminalBackground>> {
            self.query_count += 1;
            Ok(self.response)
        }

        fn terminal_capabilities(&mut self) -> io::Result<urushi_terminal::TerminalCapabilities> {
            Ok(self.capabilities)
        }
    }

    fn application(source: &str, preference: GraphicsPreference) -> ReaderApplication {
        let mut theme = crate::theme::Theme::builtin_dark();
        let crate::theme::HeadingStyle::Rasterized(heading) = &mut theme.document.headings.h1
        else {
            panic!("built-in H1 should be rasterized");
        };
        heading.font_families = vec!["Definitely Missing Heading Font".to_owned()];
        application_with_theme(source, preference, theme)
    }

    fn application_with_theme(
        source: &str,
        preference: GraphicsPreference,
        theme: crate::theme::Theme,
    ) -> ReaderApplication {
        let document = Document::parse(source.to_owned());
        let rasterize = preference != GraphicsPreference::Text && theme.uses_rasterization();
        let raster_cache = Arc::new(Mutex::new(RasterCache::default()));
        ReaderApplication {
            path: PathBuf::from("document.md"),
            images: Arc::new(ImageCatalog::load(&document, Path::new("document.md"))),
            document: Arc::new(document),
            raster_cache,
            latest_generation: Arc::new(AtomicU64::new(0)),
            max_width: Some(80),
            search_case: crate::app::SearchCase::Smart,
            rasterize,
            graphics_label: match preference {
                GraphicsPreference::Kitty => selection_label(GraphicsSelection::Kitty),
                GraphicsPreference::Sixel => selection_label(GraphicsSelection::Sixel),
                GraphicsPreference::Auto | GraphicsPreference::Text => {
                    selection_label(GraphicsSelection::Text)
                }
            },
            theme: Arc::new(theme),
        }
    }

    #[test]
    fn automatic_graphics_reports_the_selected_backend() {
        use urushi_terminal::TerminalGraphicsProtocols;

        let cases = [
            (
                TerminalGraphicsProtocols::KITTY.union(TerminalGraphicsProtocols::SIXEL),
                Some(urushi_terminal::PixelSize::new(800, 480)),
                GraphicsSelection::Kitty,
            ),
            (
                TerminalGraphicsProtocols::SIXEL,
                Some(urushi_terminal::PixelSize::new(800, 480)),
                GraphicsSelection::Sixel,
            ),
            (
                TerminalGraphicsProtocols::empty(),
                None,
                GraphicsSelection::Text,
            ),
        ];

        for (protocols, pixels, expected) in cases {
            let mut terminal = BackgroundTerminal::new(None);
            terminal.capabilities =
                urushi_terminal::TerminalCapabilities::none().with_graphics_protocols(protocols);
            terminal.window_pixels = pixels;

            assert_eq!(
                resolve_graphics(GraphicsPreference::Auto, &mut terminal)
                    .expect("automatic graphics selection should succeed"),
                expected
            );
        }
    }

    #[test]
    fn unavailable_explicit_graphics_suggests_safe_alternatives() {
        let mut terminal = BackgroundTerminal::new(None);

        let error = resolve_graphics(GraphicsPreference::Sixel, &mut terminal)
            .expect_err("unsupported explicit Sixel should fail");

        assert!(
            error
                .to_string()
                .contains("try --graphics auto or --graphics text")
        );
    }

    #[test]
    fn automatic_theme_queries_once_and_uses_urushi_classification() {
        for (background, expected) in [
            (
                urushi_terminal::TerminalBackground::new(u16::MAX, u16::MAX, u16::MAX),
                "builtin:light",
            ),
            (
                urushi_terminal::TerminalBackground::new(0, 0, 0),
                "builtin:dark",
            ),
        ] {
            let mut terminal = BackgroundTerminal::new(Some(background));
            let theme = resolve_theme(&Config::default(), None, &mut terminal)
                .expect("automatic Theme should resolve");

            assert_eq!(theme.name, expected);
            assert_eq!(terminal.query_count, 1);
        }
    }

    #[test]
    fn unavailable_background_evidence_uses_dark_fallback() {
        // Urushi maps missing, timed-out, and malformed OSC 11 replies to None.
        let mut terminal = BackgroundTerminal::new(None);
        let theme = resolve_theme(&Config::default(), None, &mut terminal)
            .expect("unavailable evidence should not block startup");

        assert_eq!(theme.name, "builtin:dark");
        assert_eq!(terminal.query_count, 1);
    }

    #[test]
    fn explicit_theme_does_not_query_the_terminal_background() {
        let mut terminal =
            BackgroundTerminal::new(Some(urushi_terminal::TerminalBackground::new(0, 0, 0)));
        let theme = resolve_theme(&Config::default(), Some("builtin:light"), &mut terminal)
            .expect("explicit Theme should resolve");

        assert_eq!(theme.name, "builtin:light");
        assert_eq!(terminal.query_count, 0);
    }

    #[test]
    fn text_only_theme_skips_background_rasterization() {
        let theme = crate::theme::resolve(
            "builtin:plain",
            &crate::theme::ThemeSettings::default(),
            None,
            None,
        )
        .expect("built-in plain Theme should resolve");
        let application = application_with_theme("# Heading", GraphicsPreference::Auto, theme);

        assert!(!application.rasterize);
    }

    #[test]
    fn initial_model_uses_text_until_background_graphics_arrive() {
        let application = application("# Heading\n\nBody", GraphicsPreference::Auto);
        let (model, _) = application.init();
        let layout = application.layout(&model);

        assert!(model.prepared_graphics.is_none());
        assert!(layout.graphics.is_empty());
        assert_eq!(model.app.top_source(), 0);
    }

    #[test]
    fn search_prompt_accepts_text_and_navigates_matches() {
        let application = application_with_theme(
            "first needle\n\nsecond needle",
            GraphicsPreference::Text,
            crate::theme::Theme::builtin_dark(),
        );
        let (mut model, _) = application.init();
        let key = |code| Message::Input(Input::Key(urushi_tui_app::KeyEvent::new(code)));

        let _ = application.update(&mut model, key(KeyCode::Char('/')));
        for character in "needle".chars() {
            let _ = application.update(&mut model, key(KeyCode::Char(character)));
        }
        assert_eq!(model.search_input.as_deref(), Some("needle"));

        let _ = application.update(&mut model, key(KeyCode::Enter));
        assert_eq!(model.search_input, None);
        assert_eq!(model.app.search_status(), Some(("needle", Some((1, 2)))));

        let _ = application.update(&mut model, key(KeyCode::Char('n')));
        assert_eq!(model.app.search_status(), Some(("needle", Some((2, 2)))));
    }

    #[test]
    fn escape_cancels_search_input_without_replacing_the_previous_search() {
        let application = application_with_theme(
            "needle",
            GraphicsPreference::Text,
            crate::theme::Theme::builtin_dark(),
        );
        let (mut model, _) = application.init();
        let key = |code| Message::Input(Input::Key(urushi_tui_app::KeyEvent::new(code)));

        let _ = application.update(&mut model, key(KeyCode::Char('/')));
        for character in "needle".chars() {
            let _ = application.update(&mut model, key(KeyCode::Char(character)));
        }
        let _ = application.update(&mut model, key(KeyCode::Enter));
        let _ = application.update(&mut model, key(KeyCode::Char('/')));
        let _ = application.update(&mut model, key(KeyCode::Char('x')));
        let _ = application.update(&mut model, key(KeyCode::Escape));

        assert_eq!(model.search_input, None);
        assert_eq!(model.app.search_status(), Some(("needle", Some((1, 1)))));
    }

    #[test]
    fn navigation_keeps_existing_graphics_until_replacements_arrive() {
        let application = application(
            "# Heading\n\nfirst paragraph\n\nsecond paragraph\n\nthird paragraph",
            GraphicsPreference::Auto,
        );
        let (mut model, _) = application.init();
        model.surface = Surface::new(40, 10);
        model.cell_pixels = PixelSize {
            width: 8,
            height: 16,
        };
        let body = ui::viewport_areas(model.surface.size, application.max_width)[1];
        let graphics = PreparedGraphics::prepare(
            &application.document,
            (body.width, body.height),
            &application.images,
            &mut RasterCache::default(),
            model.cell_pixels,
            model.app.top_source(),
            &application.theme,
        );
        model.prepared_graphics = Some(graphics);
        assert!(!application.layout(&model).graphics.is_empty());

        let _ = application.update(
            &mut model,
            Message::Input(Input::Key(urushi_tui_app::KeyEvent::new(KeyCode::Down))),
        );

        assert!(model.prepared_graphics.is_some());
        assert!(!application.layout(&model).graphics.is_empty());
    }

    #[test]
    fn text_mode_keeps_configured_fonts_deferred() {
        let application = application("# Heading", GraphicsPreference::Text);
        let (model, _) = application.init();

        assert!(!application.rasterize);
        assert!(model.prepared_graphics.is_none());
        assert!(
            !application
                .raster_cache
                .lock()
                .expect("raster cache should lock")
                .has_loaded_heading_fonts(&["Definitely Missing Heading Font".to_owned()])
        );
    }

    #[test]
    fn stale_background_result_is_not_applied() {
        let application = application("# Heading", GraphicsPreference::Auto);
        let (mut model, _) = application.init();
        model.generation = 2;

        application.apply_prepared(
            &mut model,
            Some(PreparedGraphicsResult {
                generation: 1,
                graphics: PreparedGraphics::default(),
                diagnostic: Some("stale".to_owned()),
                state: GraphicsPreparationState::new(&application.document, 0, &application.theme),
            }),
        );

        assert!(model.prepared_graphics.is_none());
        assert!(model.graphics_diagnostic.is_none());
    }

    #[test]
    fn background_preparation_transitions_text_to_graphics_with_fallback_diagnostic() {
        let application = application("# Heading", GraphicsPreference::Auto);
        let (mut model, _) = application.init();
        model.surface = Surface::new(40, 10);
        model.cell_pixels = PixelSize {
            width: 8,
            height: 16,
        };
        model.generation = 1;
        application.latest_generation.store(1, Ordering::Release);
        let body = ui::viewport_areas(model.surface.size, application.max_width)[1];

        assert!(application.layout(&model).graphics.is_empty());
        let state = GraphicsPreparationState::new(
            &application.document,
            model.app.top_source(),
            &application.theme,
        );
        let first = prepare_graphics_result(GraphicsPreparation {
            document: &application.document,
            images: &application.images,
            raster_cache: &application.raster_cache,
            latest_generation: &application.latest_generation,
            generation: 1,
            width: body.width,
            height: body.height,
            cell_pixels: model.cell_pixels,
            theme: &application.theme,
            state,
        })
        .expect("current generation should produce a heading");
        assert!(first.state.is_complete());
        let _ = application.apply_prepared(&mut model, Some(first));

        assert!(!application.layout(&model).graphics.is_empty());
        assert!(
            model
                .graphics_diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("using embedded Noto Sans JP"))
        );
    }

    #[test]
    fn current_background_result_preserves_heading_source_anchor_and_row_offset() {
        let source = "# A long heading that wraps across several terminal rows before becoming a graphic\n\nfirst trailing paragraph\n\nsecond trailing paragraph\n\nthird trailing paragraph\n\nfourth trailing paragraph";
        let application = application(source, GraphicsPreference::Auto);
        let (mut model, _) = application.init();
        model.surface = Surface::new(18, 6);
        model.cell_pixels = PixelSize {
            width: 8,
            height: 16,
        };
        model.generation = 1;
        let text_layout = application.layout(&model);
        for _ in 0..2 {
            model.app.apply(Command::Down, &text_layout, 4);
        }
        let anchor = model.app.top_source();
        let row_offset = model.app.top_row_offset();
        assert!(anchor > application.document.headings()[0].source.start);
        assert!(anchor < application.document.headings()[0].source.end);
        let body = ui::viewport_areas(model.surface.size, application.max_width)[1];
        let mut cache = RasterCache::default();
        let mut state =
            GraphicsPreparationState::new(&application.document, anchor, &application.theme);
        let mut graphics = PreparedGraphics::default();
        while !state.is_complete() {
            graphics.merge(PreparedGraphics::prepare_next(
                &application.document,
                (body.width, body.height),
                &application.images,
                &mut cache,
                model.cell_pixels,
                &application.theme,
                &mut state,
            ));
        }

        let _ = application.apply_prepared(
            &mut model,
            Some(PreparedGraphicsResult {
                generation: 1,
                graphics,
                diagnostic: None,
                state,
            }),
        );

        assert_eq!(model.app.top_source(), anchor);
        assert_eq!(model.app.top_row_offset(), row_offset);
        assert!(model.prepared_graphics.is_some());
    }

    #[test]
    fn font_fallback_diagnostic_keeps_existing_image_warning_count() {
        let application = application("![missing](missing.png)", GraphicsPreference::Auto);
        let (mut model, _) = application.init();
        model.graphics_diagnostic = Some("configured heading fonts unavailable".to_owned());

        let status = application.graphics_status(&model);

        assert!(status.contains("1 image warning(s)"));
        assert!(status.contains("configured heading fonts unavailable"));
    }

    #[test]
    fn terminal_graphics_failure_reports_text_fallback() {
        let application = application("Body", GraphicsPreference::Kitty);
        let (mut model, _) = application.init();

        let _ = application.update(
            &mut model,
            Message::TerminalError(io::Error::other("graphics write failed")),
        );
        let status = application.graphics_status(&model);

        assert!(status.contains("graphics text"));
        assert!(status.contains("terminal warning (graphics write failed)"));
    }
}
