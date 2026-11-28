//! Draws the search window with Direct2D and DirectWrite.

use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_LIGHT,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_HIT_TEST_METRICS,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{PCWSTR, Result, w};
use windows_numerics::Vector2;

use crate::dictionary::{Definition, part_name};
use crate::icons::Icon;
use crate::search::{Kind, Row};
use crate::strings::Strings;
use crate::textfield::TextField;

/// All sizes are in device independent pixels.
const BAR_HEIGHT: f32 = 64.0;
const HEADER_HEIGHT: f32 = 26.0;
const ROW_HEIGHT: f32 = 46.0;
const COMPACT_ROW_HEIGHT: f32 = 34.0;
const BOTTOM_PADDING: f32 = 8.0;
/// Space under the search bar when there are no section headings.
const TOP_PADDING: f32 = 8.0;
const TEXT_LEFT: f32 = 58.0;
const MARGIN_RIGHT: f32 = 20.0;
/// Space above and below the text of a full definition.
const PANEL_PADDING: f32 = 8.0;
const PANEL_LEFT: f32 = 22.0;
const PANEL_INDENT: f32 = 46.0;

/// How the window should look, from the settings.
#[derive(Clone, Copy)]
pub struct Look {
    pub width: f32,
    /// One line per result.
    pub compact: bool,
    /// Whether sections have headings.
    pub headers: bool,
    pub dark: bool,
    /// Whether the blurred system backdrop shows through the background.
    pub translucent: bool,
    /// Background opacity when translucent, from 0 to 1.
    pub opacity: f32,
    pub accent: (u8, u8, u8),
}

impl Look {
    fn row_height(&self) -> f32 {
        if self.compact {
            COMPACT_ROW_HEIGHT
        } else {
            ROW_HEIGHT
        }
    }

    fn text_right(&self) -> f32 {
        self.width - MARGIN_RIGHT
    }
}

/// Where each header and row goes.
pub struct Layout {
    headers: Vec<(f32, &'static str)>,
    /// The top and height of each row.
    rows: Vec<(f32, f32)>,
    pub height: f32,
}

impl Layout {
    /// `panel` is the height of a full definition row.
    pub fn new(rows: &[Row], panel: f32, look: &Look) -> Self {
        let mut headers = Vec::new();
        let mut tops = Vec::new();
        let mut y = BAR_HEIGHT;
        let mut section = "";
        if !look.headers && !rows.is_empty() {
            y += TOP_PADDING;
        }
        for row in rows {
            if look.headers && row.section != section {
                section = row.section;
                headers.push((y, section));
                y += HEADER_HEIGHT;
            }
            let height = match row.entry.kind {
                Kind::FullDefinition => panel,
                _ => look.row_height(),
            };
            tops.push((y, height));
            y += height;
        }
        if !rows.is_empty() {
            y += BOTTOM_PADDING;
        }
        Self {
            headers,
            rows: tops,
            height: y,
        }
    }

    pub fn row_at(&self, y: f32) -> Option<usize> {
        self.rows
            .iter()
            .position(|&(top, height)| y >= top && y < top + height)
    }
}

/// How tall the window would be with these rows, apart from any definition.
pub fn fixed_height(rows: usize, headers: usize, look: &Look) -> f32 {
    let headings = if look.headers {
        headers as f32 * HEADER_HEIGHT
    } else {
        TOP_PADDING
    };
    BAR_HEIGHT + rows as f32 * look.row_height() + headings + BOTTOM_PADDING
}

pub struct View<'a> {
    pub strings: &'static Strings,
    pub field: &'a TextField,
    pub rows: &'a [Row],
    pub selected: usize,
    pub caret_visible: bool,
    pub indexing: bool,
    pub look: Look,
    /// Height of the full definition row, and how far it is scrolled.
    pub panel_height: f32,
    pub panel_scroll: f32,
}

/// A paragraph of a definition, positioned within the panel.
struct Block {
    layout: IDWriteTextLayout,
    x: f32,
    y: f32,
    height: f32,
    muted: bool,
}

struct Palette {
    background: D2D1_COLOR_F,
    text: D2D1_COLOR_F,
    muted: D2D1_COLOR_F,
    line: D2D1_COLOR_F,
    accent: D2D1_COLOR_F,
    on_accent: D2D1_COLOR_F,
    selection: D2D1_COLOR_F,
}

fn color(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

impl Palette {
    fn new(look: &Look) -> Self {
        let alpha = if look.translucent { look.opacity } else { 1.0 };
        let (r, g, b) = look.accent;
        let accent = color(r, g, b, 1.0);
        let on_accent = color(255, 255, 255, 1.0);
        if look.dark {
            Self {
                background: color(30, 30, 32, alpha),
                text: color(242, 242, 245, 1.0),
                muted: color(165, 165, 172, 1.0),
                line: color(255, 255, 255, 0.12),
                accent,
                on_accent,
                selection: color(r, g, b, 0.6),
            }
        } else {
            Self {
                background: color(246, 246, 248, alpha),
                text: color(28, 28, 30, 1.0),
                muted: color(98, 98, 106, 1.0),
                line: color(0, 0, 0, 0.12),
                accent,
                on_accent,
                selection: color(r, g, b, 0.28),
            }
        }
    }
}

struct Formats {
    query: IDWriteTextFormat,
    status: IDWriteTextFormat,
    header: IDWriteTextFormat,
    name: IDWriteTextFormat,
    detail: IDWriteTextFormat,
    /// The detail beside the name in compact rows.
    aside: IDWriteTextFormat,
    glyph: IDWriteTextFormat,
    search_glyph: IDWriteTextFormat,
    word: IDWriteTextFormat,
    part: IDWriteTextFormat,
    sense: IDWriteTextFormat,
    similar: IDWriteTextFormat,
    example: IDWriteTextFormat,
}

struct Target {
    surface: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    /// Icons uploaded to this surface.
    bitmaps: HashMap<String, ID2D1Bitmap>,
}

pub struct Renderer {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    formats: Formats,
    target: Option<Target>,
    icons: HashMap<String, Icon>,
    /// The laid out text of the current definition.
    blocks: Vec<Block>,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let text = |family: PCWSTR, size: f32, weight: DWRITE_FONT_WEIGHT| {
                make_format(&dwrite, family, size, weight)
            };
            let formats = Formats {
                query: text(w!("Segoe UI"), 24.0, DWRITE_FONT_WEIGHT_LIGHT)?,
                status: text(w!("Segoe UI"), 12.0, DWRITE_FONT_WEIGHT_NORMAL)?,
                header: text(w!("Segoe UI"), 11.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?,
                name: text(w!("Segoe UI"), 14.5, DWRITE_FONT_WEIGHT_NORMAL)?,
                detail: text(w!("Segoe UI"), 11.5, DWRITE_FONT_WEIGHT_NORMAL)?,
                aside: text(w!("Segoe UI"), 11.5, DWRITE_FONT_WEIGHT_NORMAL)?,
                glyph: text(w!("Segoe Fluent Icons"), 20.0, DWRITE_FONT_WEIGHT_NORMAL)?,
                search_glyph: text(w!("Segoe Fluent Icons"), 22.0, DWRITE_FONT_WEIGHT_NORMAL)?,
                word: make_paragraph(&dwrite, 20.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
                part: make_paragraph(&dwrite, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
                similar: make_paragraph(&dwrite, 11.5, DWRITE_FONT_WEIGHT_NORMAL, false)?,
                sense: make_paragraph(&dwrite, 13.5, DWRITE_FONT_WEIGHT_NORMAL, false)?,
                example: make_paragraph(&dwrite, 12.5, DWRITE_FONT_WEIGHT_NORMAL, true)?,
            };
            formats
                .status
                .SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            formats
                .aside
                .SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            formats
                .glyph
                .SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            formats
                .search_glyph
                .SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            Ok(Self {
                factory,
                dwrite,
                formats,
                target: None,
                icons: HashMap::new(),
                blocks: Vec::new(),
            })
        }
    }

    /// Lays out the definition to be shown in full in a window `width` wide.
    pub fn set_definition(
        &mut self,
        definition: Option<&Definition>,
        width: f32,
        strings: &Strings,
    ) {
        self.blocks = definition
            .and_then(|d| self.lay_out(d, width - MARGIN_RIGHT, strings).ok())
            .unwrap_or_default();
    }

    /// Height needed to show the whole definition.
    pub fn definition_height(&self) -> f32 {
        self.blocks
            .last()
            .map_or(0.0, |b| b.y + b.height + 2.0 * PANEL_PADDING)
    }

    fn lay_out(
        &self,
        definition: &Definition,
        right: f32,
        strings: &Strings,
    ) -> Result<Vec<Block>> {
        let mut blocks = Vec::new();
        let mut y = 0.0;
        // `advance` is false for a number that sits beside the next paragraph.
        let mut add = |text: &str,
                       format: &IDWriteTextFormat,
                       x: f32,
                       gap: f32,
                       muted: bool,
                       advance: bool|
         -> Result<()> {
            let text: Vec<u16> = text.encode_utf16().collect();
            unsafe {
                let layout = self
                    .dwrite
                    .CreateTextLayout(&text, format, right - x, 10_000.0)?;
                let mut metrics = DWRITE_TEXT_METRICS::default();
                layout.GetMetrics(&mut metrics)?;
                y += gap;
                blocks.push(Block {
                    layout,
                    x,
                    y,
                    height: metrics.height,
                    muted,
                });
                if advance {
                    y += metrics.height;
                }
            }
            Ok(())
        };

        let formats = &self.formats;
        add(
            &definition.word,
            &formats.word,
            PANEL_LEFT,
            0.0,
            false,
            true,
        )?;
        for group in &definition.groups {
            let part = part_name(group.part, strings);
            let title = if group.word == definition.word {
                part.to_string()
            } else {
                format!("{part}  \u{00B7}  {}", group.word)
            };
            add(&title, &formats.part, PANEL_LEFT, 12.0, true, true)?;
            for (number, sense) in group.senses.iter().enumerate() {
                let number = format!("{}.", number + 1);
                add(&number, &formats.sense, PANEL_LEFT, 6.0, true, false)?;
                add(
                    &sense.meaning,
                    &formats.sense,
                    PANEL_INDENT,
                    0.0,
                    false,
                    true,
                )?;
                for example in sense.examples.iter().take(2) {
                    let quoted = format!("\u{201C}{example}\u{201D}");
                    add(&quoted, &formats.example, PANEL_INDENT, 1.0, true, true)?;
                }
                if !sense.synonyms.is_empty() {
                    let similar = strings
                        .similar
                        .replace("{list}", &sense.synonyms.join(", "));
                    add(&similar, &formats.similar, PANEL_INDENT, 2.0, true, true)?;
                }
            }
        }
        Ok(blocks)
    }

    pub fn add_icon(&mut self, key: String, icon: Icon) {
        self.icons.insert(key, icon);
    }

    pub fn clear_icons(&mut self) {
        self.icons.clear();
        if let Some(target) = &mut self.target {
            target.bitmaps.clear();
        }
    }

    /// Lets go of the drawing surface and what the graphics device holds for
    /// it; the next paint makes them again. Icons already read are kept.
    pub fn release(&mut self) {
        self.target = None;
    }

    pub fn paint(&mut self, window: HWND, scale: f32, view: &View) {
        // A lost graphics device makes drawing fail; rebuild and try once more.
        for _ in 0..2 {
            if self.try_paint(window, scale, view).is_ok() {
                return;
            }
            self.target = None;
        }
    }

    fn try_paint(&mut self, window: HWND, scale: f32, view: &View) -> Result<()> {
        let mut client = RECT::default();
        unsafe { GetClientRect(window, &mut client)? };
        let size = D2D_SIZE_U {
            width: (client.right - client.left).max(1) as u32,
            height: (client.bottom - client.top).max(1) as u32,
        };
        if self.target.is_none() {
            self.target = Some(self.create_target(window, size)?);
        }
        let Some(target) = &mut self.target else {
            return Ok(());
        };
        unsafe {
            let surface = &target.surface;
            let current = surface.GetPixelSize();
            if current.width != size.width || current.height != size.height {
                surface.Resize(&size)?;
            }
            surface.SetDpi(96.0 * scale, 96.0 * scale);
            surface.BeginDraw();
            let painter = Painter {
                target,
                dwrite: &self.dwrite,
                formats: &self.formats,
                icons: &self.icons,
                blocks: &self.blocks,
                palette: Palette::new(&view.look),
                look: view.look,
            };
            let drawn = painter.draw(view);
            let ended = target.surface.EndDraw(None, None);
            drawn.and(ended)
        }
    }

    fn create_target(&self, window: HWND, size: D2D_SIZE_U) -> Result<Target> {
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            ..Default::default()
        };
        let window_properties = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: window,
            pixelSize: size,
            ..Default::default()
        };
        unsafe {
            let surface = self
                .factory
                .CreateHwndRenderTarget(&properties, &window_properties)?;
            // ClearType cannot blend onto a translucent background.
            surface.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = surface.CreateSolidColorBrush(&color(0, 0, 0, 1.0), None)?;
            Ok(Target {
                surface,
                brush,
                bitmaps: HashMap::new(),
            })
        }
    }
}

fn make_format(
    dwrite: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = dwrite.CreateTextFormat(
            family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let trimming = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            ..Default::default()
        };
        let ellipsis = dwrite.CreateEllipsisTrimmingSign(&format)?;
        format.SetTrimming(&trimming, &ellipsis)?;
        Ok(format)
    }
}

/// A format for wrapped text in the definition panel.
fn make_paragraph(
    dwrite: &IDWriteFactory,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    italic: bool,
) -> Result<IDWriteTextFormat> {
    let style = if italic {
        DWRITE_FONT_STYLE_ITALIC
    } else {
        DWRITE_FONT_STYLE_NORMAL
    };
    unsafe {
        dwrite.CreateTextFormat(
            w!("Segoe UI"),
            None,
            weight,
            style,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-us"),
        )
    }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

/// Segoe Fluent Icons code points, used when there is no shell icon.
fn glyph(kind: Kind) -> &'static str {
    match kind {
        Kind::App => "\u{E71D}",
        Kind::Setting => "\u{E713}",
        Kind::Folder => "\u{E8B7}",
        Kind::File => "\u{E7C3}",
        Kind::Calculator => "\u{E8EF}",
        Kind::Url => "\u{E774}",
        Kind::Web => "\u{E721}",
        Kind::Definition | Kind::FullDefinition => "\u{E82D}",
        Kind::Notice => "\u{E946}",
    }
}

/// Borrowed pieces needed during one frame.
struct Painter<'a> {
    target: &'a mut Target,
    dwrite: &'a IDWriteFactory,
    formats: &'a Formats,
    icons: &'a HashMap<String, Icon>,
    blocks: &'a [Block],
    palette: Palette,
    look: Look,
}

impl Painter<'_> {
    fn text(&self, text: &str, format: &IDWriteTextFormat, area: D2D_RECT_F, color: D2D1_COLOR_F) {
        let text: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.target.brush.SetColor(&color);
            self.target.surface.DrawText(
                &text,
                format,
                &area,
                &self.target.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    fn fill(&self, area: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
        unsafe {
            self.target.brush.SetColor(&color);
            let rounded = D2D1_ROUNDED_RECT {
                rect: area,
                radiusX: radius,
                radiusY: radius,
            };
            self.target
                .surface
                .FillRoundedRectangle(&rounded, &self.target.brush);
        }
    }

    fn draw(mut self, view: &View) -> Result<()> {
        unsafe { self.target.surface.Clear(Some(&self.palette.background)) };
        self.draw_search_bar(view)?;

        if view.rows.is_empty() {
            return Ok(());
        }
        let width = self.look.width;
        self.fill(
            rect(0.0, BAR_HEIGHT - 1.0, width, BAR_HEIGHT),
            0.0,
            self.palette.line,
        );
        let layout = Layout::new(view.rows, view.panel_height, &self.look);
        for &(top, title) in &layout.headers {
            let area = rect(18.0, top + 3.0, self.look.text_right(), top + HEADER_HEIGHT);
            self.text(title, &self.formats.header, area, self.palette.muted);
        }
        for (index, (row, &(top, height))) in view.rows.iter().zip(&layout.rows).enumerate() {
            let selected = index == view.selected;
            if row.entry.kind == Kind::FullDefinition {
                self.draw_definition(top, height, view.panel_scroll, selected);
                continue;
            }
            // A message cannot be opened, so it is never highlighted.
            let (main, secondary) = if selected && row.entry.kind != Kind::Notice {
                self.fill(
                    rect(8.0, top, width - 8.0, top + height),
                    8.0,
                    self.palette.accent,
                );
                (self.palette.on_accent, self.palette.on_accent)
            } else {
                (self.palette.text, self.palette.muted)
            };
            let icon_area = if self.look.compact {
                rect(21.0, top + 5.0, 45.0, top + 29.0)
            } else {
                rect(18.0, top + 8.0, 48.0, top + 38.0)
            };
            if !self.draw_icon(&row.entry.target, icon_area)? {
                self.text(glyph(row.entry.kind), &self.formats.glyph, icon_area, main);
            }
            let right = self.look.text_right();
            let name = &row.entry.name;
            let detail = row.entry.detail(view.strings);
            if self.look.compact {
                // The detail takes whatever the name leaves free.
                let span = right - TEXT_LEFT;
                let used = self.text_width(name, &self.formats.name)?;
                let split = TEXT_LEFT + used.min(span * 0.62);
                let name_area = rect(TEXT_LEFT, top, split + 1.0, top + height);
                self.text(name, &self.formats.name, name_area, main);
                let detail_area = rect(split + 16.0, top + 1.0, right, top + height);
                self.text(&detail, &self.formats.aside, detail_area, secondary);
            } else {
                let name_area = rect(TEXT_LEFT, top + 3.0, right, top + 26.0);
                self.text(name, &self.formats.name, name_area, main);
                let detail_area = rect(TEXT_LEFT, top + 24.0, right, top + 42.0);
                self.text(&detail, &self.formats.detail, detail_area, secondary);
            }
        }
        Ok(())
    }

    fn text_width(&self, text: &str, format: &IDWriteTextFormat) -> Result<f32> {
        let text: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let layout = self
                .dwrite
                .CreateTextLayout(&text, format, 10_000.0, ROW_HEIGHT)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics)?;
            Ok(metrics.width.ceil())
        }
    }

    fn draw_definition(&self, top: f32, height: f32, scroll: f32, selected: bool) {
        if selected {
            self.fill(
                rect(8.0, top + 4.0, 11.0, top + height - 4.0),
                1.5,
                self.palette.accent,
            );
        }
        unsafe {
            self.target.surface.PushAxisAlignedClip(
                &rect(0.0, top, self.look.width, top + height),
                Default::default(),
            );
        }
        let origin = top + PANEL_PADDING - scroll;
        for block in self.blocks {
            let y = origin + block.y;
            if y + block.height < top || y > top + height {
                continue;
            }
            let color = if block.muted {
                self.palette.muted
            } else {
                self.palette.text
            };
            unsafe {
                self.target.brush.SetColor(&color);
                self.target.surface.DrawTextLayout(
                    Vector2 { X: block.x, Y: y },
                    &block.layout,
                    &self.target.brush,
                    Default::default(),
                );
            }
        }
        unsafe { self.target.surface.PopAxisAlignedClip() };

        // A scroll bar, when there is more than fits.
        let total = self.blocks.last().map_or(0.0, |b| b.y + b.height) + 2.0 * PANEL_PADDING;
        if total > height + 0.5 {
            let thumb = (height * height / total).max(24.0);
            let offset = (height - thumb) * (scroll / (total - height)).clamp(0.0, 1.0);
            self.fill(
                rect(
                    self.look.width - 9.0,
                    top + offset,
                    self.look.width - 5.0,
                    top + offset + thumb,
                ),
                2.0,
                self.palette.line,
            );
        }
    }

    fn draw_search_bar(&self, view: &View) -> Result<()> {
        self.text(
            "\u{E721}",
            &self.formats.search_glyph,
            rect(14.0, 0.0, 50.0, BAR_HEIGHT),
            self.palette.muted,
        );
        let mut right = self.look.text_right();
        if view.indexing {
            self.text(
                view.strings.indexing,
                &self.formats.status,
                rect(right - 80.0, 0.0, right, BAR_HEIGHT),
                self.palette.muted,
            );
            right -= 90.0;
        }

        let text: Vec<u16> = view.field.text().encode_utf16().collect();
        unsafe {
            // Wide enough that the query is never trimmed; it scrolls instead.
            let layout =
                self.dwrite
                    .CreateTextLayout(&text, &self.formats.query, 100_000.0, BAR_HEIGHT)?;
            let x_of = |position: u32| -> Result<f32> {
                let (mut x, mut y) = (0.0, 0.0);
                let mut metrics = DWRITE_HIT_TEST_METRICS::default();
                layout.HitTestTextPosition(position, false, &mut x, &mut y, &mut metrics)?;
                Ok(x)
            };
            let caret = x_of(view.field.caret_utf16())?;
            let scroll = (caret - (right - TEXT_LEFT - 2.0)).max(0.0);
            let origin = TEXT_LEFT - scroll;

            self.target.surface.PushAxisAlignedClip(
                &rect(TEXT_LEFT - 1.0, 0.0, right, BAR_HEIGHT),
                Default::default(),
            );
            let selection = match view.field.selection_utf16() {
                Some((start, end)) => x_of(start).and_then(|a| Ok((a, x_of(end)?))).map(Some),
                None => Ok(None),
            };
            if let Ok(Some((start, end))) = selection {
                self.fill(
                    rect(origin + start, 16.0, origin + end, BAR_HEIGHT - 16.0),
                    3.0,
                    self.palette.selection,
                );
            }
            if view.field.is_empty() {
                self.text(
                    view.strings.placeholder,
                    &self.formats.query,
                    rect(TEXT_LEFT + 4.0, 0.0, right, BAR_HEIGHT),
                    self.palette.muted,
                );
            } else {
                self.target.brush.SetColor(&self.palette.text);
                self.target.surface.DrawTextLayout(
                    Vector2 { X: origin, Y: 0.0 },
                    &layout,
                    &self.target.brush,
                    Default::default(),
                );
            }
            if view.caret_visible {
                self.fill(
                    rect(
                        origin + caret,
                        17.0,
                        origin + caret + 1.5,
                        BAR_HEIGHT - 17.0,
                    ),
                    0.0,
                    self.palette.text,
                );
            }
            self.target.surface.PopAxisAlignedClip();
            selection.map(|_| ())
        }
    }

    /// Draws the shell icon for `key` if it has been loaded.
    fn draw_icon(&mut self, key: &str, area: D2D_RECT_F) -> Result<bool> {
        if !self.target.bitmaps.contains_key(key) {
            let Some(icon) = self.icons.get(key) else {
                return Ok(false);
            };
            let properties = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let size = D2D_SIZE_U {
                width: icon.width,
                height: icon.height,
            };
            let bitmap = unsafe {
                self.target.surface.CreateBitmap(
                    size,
                    Some(icon.pixels.as_ptr() as *const _),
                    icon.width * 4,
                    &properties,
                )?
            };
            self.target.bitmaps.insert(key.to_string(), bitmap);
        }
        let bitmap = &self.target.bitmaps[key];
        unsafe {
            self.target.surface.DrawBitmap(
                bitmap,
                Some(&area),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
        Ok(true)
    }
}
