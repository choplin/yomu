use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use urushi::View;
use urushi_terminal::{TerminalQuery, backend::native::NativeTerminal};
use urushi_tui_app::{
    Application, Effect, Input, KeyCode, KeyKind, Runtime, Subscription, Surface,
};

use crate::image::ImageCatalog;
use crate::layout::PreparedGraphics;
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
    let background = if config.needs_terminal_background(theme_name) {
        terminal
            .terminal_background()
            .context("could not query terminal background for automatic Theme selection")?
    } else {
        None
    };
    let theme = config.resolve_theme(theme_name, background)?;
    let rasterize = preference != GraphicsPreference::Text && theme.uses_rasterization();
    let application = ReaderApplication {
        path: path.to_path_buf(),
        document: document.clone(),
        images: ImageCatalog::load(document, path),
        raster_cache: Arc::new(Mutex::new(RasterCache::default())),
        latest_generation: Arc::new(AtomicU64::new(0)),
        max_width: config.body_max_width(),
        rasterize,
        graphics_label: preference_label(preference),
        theme,
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

struct ReaderApplication {
    path: PathBuf,
    document: Document,
    images: ImageCatalog,
    raster_cache: Arc<Mutex<RasterCache>>,
    latest_generation: Arc<AtomicU64>,
    max_width: Option<u16>,
    rasterize: bool,
    graphics_label: &'static str,
    theme: crate::theme::Theme,
}

struct ReaderModel {
    app: App,
    prepared_graphics: Option<PreparedGraphics>,
    surface: Surface,
    generation: u64,
    cell_pixels: PixelSize,
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
                prepared_graphics: None,
                surface,
                generation: 0,
                cell_pixels: PixelSize::FALLBACK,
                terminal_diagnostic: None,
                graphics_diagnostic: None,
            },
            Effect::none(),
        )
    }

    fn update(&self, model: &mut Self::Model, message: Self::Message) -> Effect<Self::Message> {
        match message {
            Message::Input(Input::Key(key)) if key.kind != KeyKind::Release => {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Escape) {
                    return Effect::shutdown();
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
            Message::Prepared(prepared) => {
                self.apply_prepared(model, prepared);
                Effect::none()
            }
            Message::TerminalError(error) => {
                model.terminal_diagnostic = Some(error.to_string());
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
            &status,
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
    fn apply_prepared(&self, model: &mut ReaderModel, prepared: Option<PreparedGraphicsResult>) {
        let Some(prepared) = prepared.filter(|result| result.generation == model.generation) else {
            return;
        };
        model.prepared_graphics = Some(prepared.graphics);
        model.graphics_diagnostic = prepared.diagnostic;
        let layout = self.layout(model);
        let body = ui::viewport_areas(model.surface.size, self.max_width)[1];
        model.app.clamp_viewport(&layout, body.height);
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
        let document = self.document.clone();
        let images = self.images.clone();
        let raster_cache = Arc::clone(&self.raster_cache);
        let latest_generation = Arc::clone(&self.latest_generation);
        let generation = model.generation;
        latest_generation.store(generation, Ordering::Release);
        let cell_pixels = model.cell_pixels;
        let focus_source = model.app.top_source();
        let theme = self.theme.clone();
        Effect::perform_latest("raden-rasterize", move || {
            Message::Prepared(prepare_graphics_result(GraphicsPreparation {
                document: &document,
                images: &images,
                raster_cache: &raster_cache,
                latest_generation: &latest_generation,
                generation,
                width: body.width,
                height: body.height,
                cell_pixels,
                focus_source,
                theme: &theme,
            }))
        })
    }

    fn graphics_status(&self, model: &ReaderModel) -> String {
        if let Some(error) = &model.terminal_diagnostic {
            return format!("terminal warning ({error}); theme {}", self.theme.name);
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
                self.graphics_label, self.theme.name
            )
        } else {
            format!(
                "graphics {} ({}); theme {}",
                self.graphics_label,
                diagnostics.join("; "),
                self.theme.name
            )
        }
    }
}

struct PreparedGraphicsResult {
    generation: u64,
    graphics: PreparedGraphics,
    diagnostic: Option<String>,
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
    focus_source: usize,
    theme: &'a crate::theme::Theme,
}

fn prepare_graphics_result(request: GraphicsPreparation<'_>) -> Option<PreparedGraphicsResult> {
    if request.latest_generation.load(Ordering::Acquire) != request.generation {
        return None;
    }
    let mut cache = request.raster_cache.lock().expect("raster cache poisoned");
    if request.latest_generation.load(Ordering::Acquire) != request.generation {
        return None;
    }
    let graphics = PreparedGraphics::prepare(
        request.document,
        (request.width, request.height),
        request.images,
        &mut cache,
        request.cell_pixels,
        request.focus_source,
        request.theme,
    );
    let diagnostic = cache.heading_font_diagnostic();
    Some(PreparedGraphicsResult {
        generation: request.generation,
        graphics,
        diagnostic,
    })
}

fn surface_cell_pixels(surface: Surface) -> Option<PixelSize> {
    let pixels = surface.cell_pixels?;
    Some(PixelSize {
        width: u32::try_from(pixels.width()).ok()?,
        height: u32::try_from(pixels.height()).ok()?,
    })
}

const fn preference_label(preference: GraphicsPreference) -> &'static str {
    match preference {
        GraphicsPreference::Auto => "auto",
        GraphicsPreference::Kitty => "kitty",
        GraphicsPreference::Sixel => "sixel",
        GraphicsPreference::Text => "text",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            images: ImageCatalog::load(&document, Path::new("document.md")),
            document,
            raster_cache,
            latest_generation: Arc::new(AtomicU64::new(0)),
            max_width: Some(80),
            rasterize,
            graphics_label: preference_label(preference),
            theme,
        }
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
        let prepared = prepare_graphics_result(GraphicsPreparation {
            document: &application.document,
            images: &application.images,
            raster_cache: &application.raster_cache,
            latest_generation: &application.latest_generation,
            generation: 1,
            width: body.width,
            height: body.height,
            cell_pixels: model.cell_pixels,
            focus_source: model.app.top_source(),
            theme: &application.theme,
        });
        application.apply_prepared(&mut model, prepared);

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
        let graphics = PreparedGraphics::prepare(
            &application.document,
            (body.width, body.height),
            &application.images,
            &mut cache,
            model.cell_pixels,
            anchor,
            &application.theme,
        );

        application.apply_prepared(
            &mut model,
            Some(PreparedGraphicsResult {
                generation: 1,
                graphics,
                diagnostic: None,
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
}
