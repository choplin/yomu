use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use urushi::View;
use urushi_terminal::backend::native::NativeTerminal;
use urushi_tui_app::{
    Application, Effect, Input, KeyCode, KeyKind, Runtime, Subscription, Surface,
};

use crate::heading::HeadingFonts;
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
) -> Result<()> {
    if !io::stdout().is_terminal() {
        return Err(anyhow!("an interactive terminal is required on stdout"));
    }
    let heading_fonts = if config.heading_families().is_empty() {
        HeadingFonts::default()
    } else {
        HeadingFonts::load(config.heading_families()).map_err(anyhow::Error::msg)?
    };
    let application = ReaderApplication {
        path: path.to_path_buf(),
        document: document.clone(),
        images: ImageCatalog::load(document, path),
        raster_cache: Arc::new(Mutex::new(RasterCache::with_heading_fonts(heading_fonts))),
        latest_generation: Arc::new(AtomicU64::new(0)),
        max_width: config.body_max_width(),
        rasterize: preference != GraphicsPreference::Text,
        graphics_label: preference_label(preference),
    };
    let terminal = NativeTerminal::open().context("could not open the controlling terminal")?;
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
}

struct ReaderModel {
    app: App,
    prepared_graphics: Option<PreparedGraphics>,
    surface: Surface,
    generation: u64,
    cell_pixels: PixelSize,
    terminal_diagnostic: Option<String>,
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
            Message::Prepared(Some(prepared)) if prepared.generation == model.generation => {
                model.prepared_graphics = Some(prepared.graphics);
                let layout = self.layout(model);
                let body = ui::viewport_areas(model.surface.size, self.max_width)[1];
                model.app.clamp_viewport(&layout, body.height);
                Effect::none()
            }
            Message::Prepared(_) => Effect::none(),
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
    fn layout(&self, model: &ReaderModel) -> Layout {
        let body = ui::viewport_areas(model.surface.size, self.max_width)[1];
        model.prepared_graphics.as_ref().map_or_else(
            || Layout::with_text(&self.document, body.width, &self.images),
            |graphics| Layout::with_prepared(&self.document, body.width, &self.images, graphics),
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
        Effect::perform_latest("raden-rasterize", move || {
            if latest_generation.load(Ordering::Acquire) != generation {
                return Message::Prepared(None);
            }
            let mut cache = raster_cache.lock().expect("raster cache poisoned");
            if latest_generation.load(Ordering::Acquire) != generation {
                return Message::Prepared(None);
            }
            let graphics = PreparedGraphics::prepare(
                &document,
                body.width,
                body.height,
                &images,
                &mut cache,
                cell_pixels,
                focus_source,
            );
            Message::Prepared(Some(PreparedGraphicsResult {
                generation,
                graphics,
            }))
        })
    }

    fn graphics_status(&self, model: &ReaderModel) -> String {
        if let Some(error) = &model.terminal_diagnostic {
            return format!("terminal warning ({error})");
        }
        let warnings = self.images.diagnostics().len();
        if warnings == 0 {
            format!("graphics {}", self.graphics_label)
        } else {
            format!(
                "graphics {} ({warnings} image warning(s))",
                self.graphics_label
            )
        }
    }
}

struct PreparedGraphicsResult {
    generation: u64,
    graphics: PreparedGraphics,
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
