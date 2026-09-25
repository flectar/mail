use crate::rich_compose::{
    BOLD, CODE, CharacterStyle, ComposeSelection, ITALIC, RichComposeDocument, STRIKE, UNDERLINE,
};
use cosmic_text::{
    Action, Attrs, Buffer, Color as CosmicColor, Cursor, Edit, Editor, Family, FontSystem, Metrics,
    Motion, PhysicalGlyph, Renderer, Selection, Shaping, Style, SwashCache, SwashContent,
    UnderlineStyle, Weight, Wrap, fontdb::Source,
};
use slint::{Color, Image, Rgba8Pixel, SharedPixelBuffer};
use std::{
    cell::OnceCell,
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    sync::Arc,
    time::{Duration, Instant},
};

const FONT_SIZE_LOGICAL: f32 = 13.0;
const LINE_HEIGHT_LOGICAL: f32 = 18.0;
pub(crate) const UI_FONT_FAMILY: &str = "Google Sans Flex";
pub(crate) const UI_FONT_DATA: &[u8] = include_bytes!(
    "../resources/fonts/google-sans-flex/GoogleSansFlex-VariableFont_GRAD,ROND,opsz,slnt,wdth,wght.ttf"
);
const TILE_HEIGHT_LOGICAL: f32 = 384.0;
const TILE_OVERSCAN: u32 = 1;
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(450);
const MULTI_CLICK_DISTANCE: f32 = 5.0;
// cosmic-text 0.19's rich-text path omits the final empty paragraph when the
// supplied text ends in a newline. Keep that paragraph alive in the layout
// with a zero-width, transparent character. This is renderer-only and never
// enters the editable document or its byte-offset mapping.
const TRAILING_LINE_SENTINEL: &str = "\u{200b}";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComposeEditorStyle {
    pub text: Color,
    pub link: Color,
    pub selection: Color,
    pub selected_text: Color,
}

#[derive(Clone)]
pub struct RenderedComposeTile {
    pub image: Image,
    pub y: f32,
    pub height: f32,
}

pub struct RenderedComposeEditor {
    pub tiles: Vec<RenderedComposeTile>,
    pub content_height: f32,
    pub caret_x: f32,
    pub caret_y: f32,
    pub caret_height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutKey {
    revision: u64,
    physical_width: u32,
    scale_bits: u32,
    text_color: u32,
    link_color: u32,
}

/// Retained cosmic-text layout and raster cache for the composer body.
///
/// The editor owns the only visible text layout. Slint's transparent TextInput
/// remains focused for native IME and clipboard behavior, but all visible text,
/// selection highlighting, hit-testing, and caret geometry come from here.
pub struct CosmicComposeEditor {
    font_system: FontSystem,
    swash_cache: SwashCache,
    editor: Editor<'static>,
    layout_key: Option<LayoutKey>,
    layout_preedit_text: String,
    layout_preedit_offset: usize,
    visual_text: String,
    rendered_selection: Option<(i32, i32)>,
    content_height_physical: f32,
    tiles: BTreeMap<u32, RenderedComposeTile>,
    pointer_down: bool,
    last_pointer_down: Option<(Instant, f32, f32)>,
    click_count: u8,
}

/// Construct the font database only when a composer action needs its surface.
/// Most sessions reach the mailbox without opening the composer.
#[derive(Default)]
pub struct LazyComposeEditor(OnceCell<CosmicComposeEditor>);

impl LazyComposeEditor {
    /// The document owns text, selection and undo history independently.
    pub fn release(&mut self) {
        self.0.take();
    }
}

impl Deref for LazyComposeEditor {
    type Target = CosmicComposeEditor;

    fn deref(&self) -> &Self::Target {
        self.0.get_or_init(CosmicComposeEditor::default)
    }
}

impl DerefMut for LazyComposeEditor {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.get_or_init(CosmicComposeEditor::default);
        self.0.get_mut().expect("compose editor was initialized")
    }
}

impl Default for CosmicComposeEditor {
    fn default() -> Self {
        let mut font_system = FontSystem::new();
        // The composer is rasterized by cosmic-text rather than Slint, so it
        // must register the same bundled UI font independently. System fonts
        // remain available as fallbacks for glyphs outside the font's coverage.
        // The bytes already live in the executable for Slint. Share them with
        // fontdb instead of copying the 4 MiB font into the heap.
        font_system
            .db_mut()
            .load_font_source(Source::Binary(Arc::new(UI_FONT_DATA)));
        let buffer = Buffer::new(
            &mut font_system,
            Metrics::new(FONT_SIZE_LOGICAL, LINE_HEIGHT_LOGICAL),
        );
        Self {
            font_system,
            swash_cache: SwashCache::new(),
            editor: Editor::new(buffer),
            layout_key: None,
            layout_preedit_text: String::new(),
            layout_preedit_offset: 0,
            visual_text: String::new(),
            rendered_selection: None,
            content_height_physical: LINE_HEIGHT_LOGICAL,
            tiles: BTreeMap::new(),
            pointer_down: false,
            last_pointer_down: None,
            click_count: 0,
        }
    }
}

impl CosmicComposeEditor {
    pub fn reset(&mut self) {
        self.layout_key = None;
        self.layout_preedit_text.clear();
        self.layout_preedit_offset = 0;
        self.visual_text.clear();
        self.rendered_selection = None;
        self.tiles.clear();
        self.pointer_down = false;
        self.last_pointer_down = None;
        self.click_count = 0;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        document: &RichComposeDocument,
        selection: ComposeSelection,
        logical_width: f32,
        viewport_height: f32,
        scroll_y: f32,
        scale_factor: f32,
        style: ComposeEditorStyle,
        preedit_text: &str,
    ) -> RenderedComposeEditor {
        let scale_factor = scale_factor.max(1.0);
        self.ensure_layout(
            document,
            selection,
            logical_width,
            scale_factor,
            style,
            preedit_text,
        );
        self.set_selection(selection);
        self.editor.shape_as_needed(&mut self.font_system, false);

        let (caret_x, caret_y, caret_height) = self.caret_geometry(scale_factor);
        let wanted = desired_tile_range(
            scroll_y,
            viewport_height,
            self.content_height_physical / scale_factor,
        );
        self.tiles
            .retain(|index, _| wanted.clone().any(|wanted| wanted == *index));

        let physical_width = self
            .layout_key
            .map_or(1, |layout| layout.physical_width.max(1));
        let tile_height_physical = (TILE_HEIGHT_LOGICAL * scale_factor).ceil().max(1.0) as u32;
        let content_height_physical = self.content_height_physical.ceil().max(1.0) as u32;
        for index in wanted {
            if self.tiles.contains_key(&index) {
                continue;
            }
            let tile_start = index.saturating_mul(tile_height_physical);
            if tile_start >= content_height_physical {
                continue;
            }
            let tile_height = tile_height_physical.min(content_height_physical - tile_start);
            let image = self.render_tile(physical_width, tile_start, tile_height, style);
            self.tiles.insert(
                index,
                RenderedComposeTile {
                    image,
                    y: tile_start as f32 / scale_factor,
                    height: tile_height as f32 / scale_factor,
                },
            );
        }

        RenderedComposeEditor {
            tiles: self.tiles.values().cloned().collect(),
            content_height: self.content_height_physical / scale_factor,
            caret_x,
            caret_y,
            caret_height,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_pointer(
        &mut self,
        document: &RichComposeDocument,
        selection: ComposeSelection,
        logical_width: f32,
        scale_factor: f32,
        style: ComposeEditorStyle,
        preedit_text: &str,
        x: f32,
        y: f32,
        kind: &str,
        shift: bool,
    ) -> Option<ComposeSelection> {
        let scale_factor = scale_factor.max(1.0);
        self.ensure_layout(
            document,
            selection,
            logical_width,
            scale_factor,
            style,
            preedit_text,
        );
        self.set_selection(selection);
        self.editor.shape_as_needed(&mut self.font_system, false);

        let x_physical = (x.max(0.0) * scale_factor).round() as i32;
        let y_physical = (y.max(0.0) * scale_factor).round() as i32;
        match kind {
            "down" => {
                self.pointer_down = true;
                self.click_count = self.next_click_count(x, y);
                let action = if shift {
                    Action::Drag {
                        x: x_physical,
                        y: y_physical,
                    }
                } else if self.click_count >= 3 {
                    Action::TripleClick {
                        x: x_physical,
                        y: y_physical,
                    }
                } else if self.click_count == 2 {
                    Action::DoubleClick {
                        x: x_physical,
                        y: y_physical,
                    }
                } else {
                    Action::Click {
                        x: x_physical,
                        y: y_physical,
                    }
                };
                self.editor.action(&mut self.font_system, action);
            }
            "move" if self.pointer_down => {
                self.editor.action(
                    &mut self.font_system,
                    Action::Drag {
                        x: x_physical,
                        y: y_physical,
                    },
                );
            }
            "up" | "cancel" => {
                self.pointer_down = false;
                return None;
            }
            _ => return None,
        }

        Some(self.selection_from_editor())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_navigation(
        &mut self,
        document: &RichComposeDocument,
        selection: ComposeSelection,
        logical_width: f32,
        scale_factor: f32,
        style: ComposeEditorStyle,
        preedit_text: &str,
        key: &str,
        control: bool,
        shift: bool,
        alt: bool,
        meta: bool,
    ) -> Option<ComposeSelection> {
        let motion = navigation_motion(key, control, alt, meta)?;
        let scale_factor = scale_factor.max(1.0);
        self.ensure_layout(
            document,
            selection,
            logical_width,
            scale_factor,
            style,
            preedit_text,
        );
        self.set_selection(selection);
        self.editor.shape_as_needed(&mut self.font_system, false);

        if shift {
            if self.editor.selection() == Selection::None {
                self.editor
                    .set_selection(Selection::Normal(self.editor.cursor()));
            }
        } else if let Some((start, end)) = self.editor.selection_bounds() {
            let collapse_to_start = matches!(
                motion,
                Motion::Left
                    | Motion::LeftWord
                    | Motion::Previous
                    | Motion::PreviousWord
                    | Motion::Up
                    | Motion::Home
                    | Motion::SoftHome
                    | Motion::ParagraphStart
                    | Motion::BufferStart
                    | Motion::PageUp
            );
            self.editor
                .set_cursor(if collapse_to_start { start } else { end });
            self.editor.set_selection(Selection::None);
            self.tiles.clear();
            return Some(self.selection_from_editor());
        } else {
            self.editor.set_selection(Selection::None);
        }

        self.editor
            .action(&mut self.font_system, Action::Motion(motion));
        if !shift {
            self.editor.set_selection(Selection::None);
        }
        Some(self.selection_from_editor())
    }

    fn ensure_layout(
        &mut self,
        document: &RichComposeDocument,
        selection: ComposeSelection,
        logical_width: f32,
        scale_factor: f32,
        style: ComposeEditorStyle,
        preedit_text: &str,
    ) {
        let physical_width = (logical_width.max(1.0) * scale_factor).ceil().max(1.0) as u32;
        let key = LayoutKey {
            revision: document.revision(),
            physical_width,
            scale_bits: scale_factor.to_bits(),
            text_color: style.text.as_argb_encoded(),
            link_color: style.link.as_argb_encoded(),
        };
        let preedit_offset = valid_offset(document.text(), selection.end);
        if self.layout_key == Some(key)
            && self.layout_preedit_text == preedit_text
            && (preedit_text.is_empty() || self.layout_preedit_offset == preedit_offset)
        {
            return;
        }

        let metrics = Metrics::new(
            FONT_SIZE_LOGICAL * scale_factor,
            LINE_HEIGHT_LOGICAL * scale_factor,
        );
        let default_attrs = Attrs::new()
            .family(Family::Name(UI_FONT_FAMILY))
            .color(cosmic_color(style.text));
        let mut spans = rich_spans(
            document,
            preedit_offset,
            preedit_text,
            &default_attrs,
            cosmic_color(style.link),
        );
        self.visual_text.clear();
        self.visual_text
            .push_str(&document.text()[..preedit_offset]);
        self.visual_text.push_str(preedit_text);
        self.visual_text
            .push_str(&document.text()[preedit_offset..]);
        if self.visual_text.ends_with('\n') {
            spans.push((
                TRAILING_LINE_SENTINEL.to_owned(),
                default_attrs.clone().color(CosmicColor::rgba(0, 0, 0, 0)),
            ));
        }

        self.editor.with_buffer_mut(|buffer| {
            buffer.set_metrics_and_size(metrics, Some(physical_width as f32), None);
            buffer.set_wrap(Wrap::WordOrGlyph);
            buffer.set_rich_text(
                spans
                    .iter()
                    .map(|(text, attrs)| (text.as_str(), attrs.clone())),
                &default_attrs,
                Shaping::Advanced,
                None,
            );
        });
        self.editor.shape_as_needed(&mut self.font_system, false);
        self.content_height_physical = self.editor.with_buffer(|buffer| {
            buffer
                .layout_runs()
                .map(|run| run.line_top + run.line_height)
                .fold(metrics.line_height, f32::max)
        });
        self.layout_key = Some(key);
        self.layout_preedit_text.clear();
        self.layout_preedit_text.push_str(preedit_text);
        self.layout_preedit_offset = preedit_offset;
        self.rendered_selection = None;
        self.tiles.clear();
    }

    fn set_selection(&mut self, selection: ComposeSelection) {
        if self.rendered_selection == Some((selection.start, selection.end)) {
            return;
        }
        let had_visible_selection = self
            .rendered_selection
            .is_some_and(|(start, end)| start != end);
        let has_visible_selection = selection.start != selection.end;
        let (anchor_offset, cursor_offset) = if self.layout_preedit_text.is_empty() {
            (selection.start, selection.end)
        } else {
            let cursor = self
                .layout_preedit_offset
                .saturating_add(self.layout_preedit_text.len());
            let cursor = i32::try_from(cursor).unwrap_or(i32::MAX);
            (cursor, cursor)
        };
        let anchor = global_to_cursor(&self.visual_text, anchor_offset);
        let cursor = global_to_cursor(&self.visual_text, cursor_offset);
        self.editor.set_cursor(cursor);
        self.editor.set_selection(if anchor == cursor {
            Selection::None
        } else {
            Selection::Normal(anchor)
        });
        self.rendered_selection = Some((selection.start, selection.end));
        if had_visible_selection || has_visible_selection {
            self.tiles.clear();
        }
    }

    fn selection_from_editor(&self) -> ComposeSelection {
        let cursor = self
            .visual_to_document_offset(cursor_to_global(&self.visual_text, self.editor.cursor()));
        match self.editor.selection() {
            Selection::None => ComposeSelection {
                start: cursor,
                end: cursor,
            },
            Selection::Normal(anchor) => ComposeSelection {
                start: self.visual_to_document_offset(cursor_to_global(&self.visual_text, anchor)),
                end: cursor,
            },
            Selection::Line(_) | Selection::Word(_) => self.editor.selection_bounds().map_or(
                ComposeSelection {
                    start: cursor,
                    end: cursor,
                },
                |(start, end)| ComposeSelection {
                    start: self
                        .visual_to_document_offset(cursor_to_global(&self.visual_text, start)),
                    end: self.visual_to_document_offset(cursor_to_global(&self.visual_text, end)),
                },
            ),
        }
    }

    fn visual_to_document_offset(&self, offset: i32) -> i32 {
        if self.layout_preedit_text.is_empty() {
            return offset;
        }
        let offset = usize::try_from(offset).unwrap_or_default();
        let preedit_end = self
            .layout_preedit_offset
            .saturating_add(self.layout_preedit_text.len());
        let document_offset = if offset <= self.layout_preedit_offset {
            offset
        } else if offset <= preedit_end {
            self.layout_preedit_offset
        } else {
            offset.saturating_sub(self.layout_preedit_text.len())
        };
        i32::try_from(document_offset).unwrap_or(i32::MAX)
    }

    fn caret_geometry(&self, scale_factor: f32) -> (f32, f32, f32) {
        let Some((x, y)) = self.editor.cursor_position() else {
            return (0.0, 0.0, LINE_HEIGHT_LOGICAL);
        };
        let cursor = self.editor.cursor();
        let line_height = self.editor.with_buffer(|buffer| {
            buffer
                .layout_runs()
                .find(|run| {
                    run.line_i == cursor.line
                        && run.cursor_position(&cursor).is_some()
                        && (run.line_top - y as f32).abs() < 0.5
                })
                .map_or(LINE_HEIGHT_LOGICAL * scale_factor, |run| run.line_height)
        });
        (
            x as f32 / scale_factor,
            y as f32 / scale_factor,
            line_height / scale_factor,
        )
    }

    fn render_tile(
        &mut self,
        width: u32,
        tile_start: u32,
        tile_height: u32,
        style: ComposeEditorStyle,
    ) -> Image {
        let mut canvas = TileCanvas::new(width, tile_height, tile_start);
        let editor = &self.editor;
        let mut renderer = TileRenderer {
            font_system: &mut self.font_system,
            swash_cache: &mut self.swash_cache,
            canvas: &mut canvas,
        };
        editor.render(
            &mut renderer,
            cosmic_color(style.text),
            CosmicColor::rgba(0, 0, 0, 0),
            cosmic_color(style.selection),
            cosmic_color(style.selected_text),
        );
        canvas.into_image()
    }

    fn next_click_count(&mut self, x: f32, y: f32) -> u8 {
        let now = Instant::now();
        let repeated = self.last_pointer_down.is_some_and(|(previous, px, py)| {
            now.duration_since(previous) <= DOUBLE_CLICK_INTERVAL
                && (x - px).abs() <= MULTI_CLICK_DISTANCE
                && (y - py).abs() <= MULTI_CLICK_DISTANCE
        });
        self.last_pointer_down = Some((now, x, y));
        if repeated {
            self.click_count.saturating_add(1).min(3)
        } else {
            1
        }
    }
}

fn rich_spans(
    document: &RichComposeDocument,
    preedit_offset: usize,
    preedit_text: &str,
    default_attrs: &Attrs<'static>,
    link_color: CosmicColor,
) -> Vec<(String, Attrs<'static>)> {
    let runs = document.style_runs();
    let mut spans = Vec::with_capacity(runs.len().saturating_add(2).max(1));
    let mut inserted_preedit = preedit_text.is_empty();
    let preedit_attrs = character_attrs(default_attrs, document.typing_style(), link_color)
        .underline(UnderlineStyle::Single);

    for run in &runs {
        let attrs = character_attrs(default_attrs, &run.style, link_color);
        if !inserted_preedit && preedit_offset <= run.range.start {
            push_span(&mut spans, preedit_text, preedit_attrs.clone());
            inserted_preedit = true;
        }
        if !inserted_preedit && preedit_offset < run.range.end {
            push_span(
                &mut spans,
                &document.text()[run.range.start..preedit_offset],
                attrs.clone(),
            );
            push_span(&mut spans, preedit_text, preedit_attrs.clone());
            push_span(
                &mut spans,
                &document.text()[preedit_offset..run.range.end],
                attrs,
            );
            inserted_preedit = true;
        } else {
            push_span(&mut spans, &document.text()[run.range.clone()], attrs);
        }
    }
    if !inserted_preedit {
        push_span(&mut spans, preedit_text, preedit_attrs);
    }
    if spans.is_empty() {
        spans.push((String::new(), default_attrs.clone()));
    }
    spans
}

fn push_span(spans: &mut Vec<(String, Attrs<'static>)>, text: &str, attrs: Attrs<'static>) {
    if !text.is_empty() {
        spans.push((text.to_owned(), attrs));
    }
}

fn character_attrs(
    default_attrs: &Attrs<'static>,
    style: &CharacterStyle,
    link_color: CosmicColor,
) -> Attrs<'static> {
    let mut attrs = default_attrs.clone();
    if style.marks & BOLD != 0 {
        attrs = attrs.weight(Weight::BOLD);
    }
    if style.marks & ITALIC != 0 {
        attrs = attrs.style(Style::Italic);
    }
    if style.marks & UNDERLINE != 0 {
        attrs = attrs.underline(UnderlineStyle::Single);
    }
    if style.marks & STRIKE != 0 {
        attrs = attrs.strikethrough();
    }
    if style.marks & CODE != 0 {
        attrs = attrs.family(Family::Monospace);
    }
    if style.link.is_some() {
        attrs = attrs.color(link_color);
    }
    attrs
}

fn navigation_motion(key: &str, control: bool, alt: bool, meta: bool) -> Option<Motion> {
    let key = key.chars().next()?;
    let word = control || alt;
    match key {
        '\u{f700}' if meta => Some(Motion::BufferStart),
        '\u{f701}' if meta => Some(Motion::BufferEnd),
        '\u{f700}' if control => Some(Motion::ParagraphStart),
        '\u{f701}' if control => Some(Motion::ParagraphEnd),
        '\u{f700}' => Some(Motion::Up),
        '\u{f701}' => Some(Motion::Down),
        '\u{f702}' if meta => Some(Motion::Home),
        '\u{f703}' if meta => Some(Motion::End),
        '\u{f702}' if word => Some(Motion::LeftWord),
        '\u{f703}' if word => Some(Motion::RightWord),
        '\u{f702}' => Some(Motion::Left),
        '\u{f703}' => Some(Motion::Right),
        '\u{f729}' if control || meta => Some(Motion::BufferStart),
        '\u{f72b}' if control || meta => Some(Motion::BufferEnd),
        '\u{f729}' => Some(Motion::Home),
        '\u{f72b}' => Some(Motion::End),
        '\u{f72c}' => Some(Motion::PageUp),
        '\u{f72d}' => Some(Motion::PageDown),
        _ => None,
    }
}

fn global_to_cursor(text: &str, offset: i32) -> Cursor {
    let mut remaining = valid_offset(text, offset);
    for (line, value) in text.split('\n').enumerate() {
        if remaining <= value.len() {
            return Cursor::new(line, remaining);
        }
        remaining = remaining.saturating_sub(value.len() + 1);
    }
    let line = text.matches('\n').count();
    let index = text.rsplit('\n').next().map_or(0, str::len);
    Cursor::new(line, index)
}

fn cursor_to_global(text: &str, cursor: Cursor) -> i32 {
    let mut offset = 0usize;
    for (line, value) in text.split('\n').enumerate() {
        if line == cursor.line {
            offset += cursor.index.min(value.len());
            return i32::try_from(offset).unwrap_or(i32::MAX);
        }
        offset = offset.saturating_add(value.len() + 1);
    }
    i32::try_from(text.len()).unwrap_or(i32::MAX)
}

fn valid_offset(text: &str, offset: i32) -> usize {
    let mut offset = usize::try_from(offset).unwrap_or_default().min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn desired_tile_range(
    scroll_y: f32,
    viewport_height: f32,
    content_height: f32,
) -> std::ops::Range<u32> {
    let tile_count = (content_height.max(1.0) / TILE_HEIGHT_LOGICAL)
        .ceil()
        .max(1.0) as u32;
    let first_visible = (scroll_y.max(0.0) / TILE_HEIGHT_LOGICAL).floor() as u32;
    let last_visible =
        ((scroll_y.max(0.0) + viewport_height.max(1.0)) / TILE_HEIGHT_LOGICAL).floor() as u32;
    let start = first_visible.saturating_sub(TILE_OVERSCAN);
    let end = last_visible
        .saturating_add(TILE_OVERSCAN)
        .saturating_add(1)
        .min(tile_count);
    start..end.max(start + 1).min(tile_count)
}

fn cosmic_color(color: Color) -> CosmicColor {
    CosmicColor::rgba(color.red(), color.green(), color.blue(), color.alpha())
}

struct TileCanvas {
    width: u32,
    height: u32,
    y_offset: u32,
    pixels: Vec<u8>,
}

impl TileCanvas {
    fn new(width: u32, height: u32, y_offset: u32) -> Self {
        let len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        Self {
            width,
            height,
            y_offset,
            pixels: vec![0; len],
        }
    }

    fn blend(&mut self, x: i32, y: i32, color: CosmicColor) {
        if x < 0 || y < self.y_offset as i32 {
            return;
        }
        let x = x as u32;
        let y = y as u32 - self.y_offset;
        if x >= self.width || y >= self.height || color.a() == 0 {
            return;
        }
        let index = ((y as usize * self.width as usize) + x as usize) * 4;
        blend_premultiplied(&mut self.pixels[index..index + 4], color);
    }

    fn rectangle(&mut self, x: i32, y: i32, width: u32, height: u32, color: CosmicColor) {
        if width == 0 || height == 0 || color.a() == 0 {
            return;
        }
        let left = x.max(0) as u32;
        let right = x.saturating_add(width as i32).max(0) as u32;
        let top = y.max(self.y_offset as i32) as u32;
        let bottom = y
            .saturating_add(height as i32)
            .min(self.y_offset.saturating_add(self.height) as i32)
            .max(0) as u32;
        for py in top..bottom {
            for px in left..right.min(self.width) {
                self.blend(px as i32, py as i32, color);
            }
        }
    }

    fn into_image(self) -> Image {
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &self.pixels,
            self.width,
            self.height,
        );
        Image::from_rgba8_premultiplied(pixels)
    }
}

struct TileRenderer<'a> {
    font_system: &'a mut FontSystem,
    swash_cache: &'a mut SwashCache,
    canvas: &'a mut TileCanvas,
}

impl Renderer for TileRenderer<'_> {
    fn rectangle(&mut self, x: i32, y: i32, width: u32, height: u32, color: CosmicColor) {
        self.canvas.rectangle(x, y, width, height, color);
    }

    fn glyph(&mut self, glyph: PhysicalGlyph, color: CosmicColor) {
        let tile_top = self.canvas.y_offset as i32;
        let tile_bottom = self.canvas.y_offset.saturating_add(self.canvas.height) as i32;
        if glyph.y < tile_top.saturating_sub(256) || glyph.y > tile_bottom.saturating_add(256) {
            return;
        }
        let Some(image) = self
            .swash_cache
            .get_image(self.font_system, glyph.cache_key)
            .as_ref()
        else {
            return;
        };
        let left = glyph.x + image.placement.left;
        let top = glyph.y - image.placement.top;
        let width = image.placement.width as usize;
        let height = image.placement.height as usize;
        match image.content {
            SwashContent::Mask => {
                for row in 0..height {
                    for column in 0..width {
                        let mask = image.data[row * width + column];
                        let alpha = multiply_alpha(mask, color.a());
                        self.canvas.blend(
                            left + column as i32,
                            top + row as i32,
                            CosmicColor::rgba(color.r(), color.g(), color.b(), alpha),
                        );
                    }
                }
            }
            SwashContent::Color => {
                for row in 0..height {
                    for column in 0..width {
                        let index = (row * width + column) * 4;
                        self.canvas.blend(
                            left + column as i32,
                            top + row as i32,
                            CosmicColor::rgba(
                                image.data[index],
                                image.data[index + 1],
                                image.data[index + 2],
                                image.data[index + 3],
                            ),
                        );
                    }
                }
            }
            SwashContent::SubpixelMask => {
                for row in 0..height {
                    for column in 0..width {
                        // Swash stores subpixel masks as 32-bit RGBA. Use the
                        // strongest RGB coverage as grayscale antialiasing;
                        // the alpha byte is padding for this content type.
                        let index = (row * width + column) * 4;
                        let mask = image.data[index]
                            .max(image.data[index + 1])
                            .max(image.data[index + 2]);
                        let alpha = multiply_alpha(mask, color.a());
                        self.canvas.blend(
                            left + column as i32,
                            top + row as i32,
                            CosmicColor::rgba(color.r(), color.g(), color.b(), alpha),
                        );
                    }
                }
            }
        }
    }
}

fn multiply_alpha(first: u8, second: u8) -> u8 {
    ((u16::from(first) * u16::from(second) + 127) / 255) as u8
}

fn blend_premultiplied(destination: &mut [u8], source: CosmicColor) {
    let alpha = u32::from(source.a());
    let inverse = 255 - alpha;
    let premultiply = |channel: u8| (u32::from(channel) * alpha + 127) / 255;
    destination[0] =
        (premultiply(source.r()) + u32::from(destination[0]) * inverse / 255).min(255) as u8;
    destination[1] =
        (premultiply(source.g()) + u32::from(destination[1]) * inverse / 255).min(255) as u8;
    destination[2] =
        (premultiply(source.b()) + u32::from(destination[2]) * inverse / 255).min(255) as u8;
    destination[3] = (alpha + u32::from(destination[3]) * inverse / 255).min(255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_offsets_round_trip_across_unicode_lines() {
        let text = "héllo\n世界";
        for offset in [0, 1, 3, 6, 7, text.len() as i32] {
            let cursor = global_to_cursor(text, offset);
            assert_eq!(
                cursor_to_global(text, cursor),
                valid_offset(text, offset) as i32
            );
        }
    }

    #[test]
    fn lazy_editor_releases_fonts_without_discarding_document_history() {
        let mut editor = LazyComposeEditor::default();
        assert!(editor.0.get().is_none());
        editor.release();
        assert!(editor.0.get().is_none());
        let mut document = RichComposeDocument::default();
        document.synchronize("draft", 5, 5);
        document.insert_text(" preserved");
        editor.reset();
        assert!(editor.0.get().is_some());
        editor.release();
        assert!(editor.0.get().is_none());
        assert_eq!(document.text(), "draft preserved");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "draft");
        editor.reset();
        assert!(editor.0.get().is_some());
    }

    #[test]
    fn bundled_ui_font_is_registered_for_the_compose_renderer() {
        let surface = CosmicComposeEditor::default();
        let has_ui_font = surface.font_system.db().faces().any(|face| {
            face.families
                .iter()
                .any(|(family, _)| family == UI_FONT_FAMILY)
        });

        assert!(has_ui_font, "{UI_FONT_FAMILY} is missing from cosmic-text");
    }

    #[test]
    fn visible_tile_range_includes_overscan_and_stays_bounded() {
        assert_eq!(desired_tile_range(0.0, 300.0, 2_000.0), 0..2);
        assert_eq!(desired_tile_range(900.0, 300.0, 2_000.0), 1..5);
        assert_eq!(desired_tile_range(1_900.0, 300.0, 2_000.0), 3..6);
    }

    #[test]
    fn source_over_blending_stores_premultiplied_rgba() {
        let mut pixel = [0, 0, 0, 0];
        blend_premultiplied(&mut pixel, CosmicColor::rgba(200, 100, 50, 128));
        assert_eq!(pixel, [100, 50, 25, 128]);
    }

    #[test]
    fn combined_styles_render_without_changing_caret_text_offsets() {
        let mut document = RichComposeDocument::default();
        document.synchronize("first second", 0, 12);
        document.format("bold", "first second", 0, 12);
        document.format("underline", "first second", 0, 12);
        document.format("strike", "first second", 0, 12);
        document.update_selection(12, 12);

        let mut surface = CosmicComposeEditor::default();
        let rendered = surface.render(
            &document,
            document.selection(),
            320.0,
            120.0,
            0.0,
            1.0,
            ComposeEditorStyle {
                text: Color::from_rgb_u8(240, 240, 240),
                link: Color::from_rgb_u8(90, 150, 255),
                selection: Color::from_argb_u8(140, 60, 120, 220),
                selected_text: Color::from_rgb_u8(255, 255, 255),
            },
            "",
        );

        assert_eq!(document.text(), "first second");
        assert!(!rendered.tiles.is_empty());
        assert!(rendered.content_height >= LINE_HEIGHT_LOGICAL);
        assert!(rendered.caret_x > 0.0);
        assert!(rendered.caret_y < LINE_HEIGHT_LOGICAL);
        assert!(rendered.caret_y + rendered.caret_height <= rendered.content_height + 1.0);

        let default_attrs = Attrs::new().family(Family::Name(UI_FONT_FAMILY));
        let spans = rich_spans(
            &document,
            document.text().len(),
            "",
            &default_attrs,
            CosmicColor::rgb(90, 150, 255),
        );
        assert_eq!(spans[0].1.text_decoration.underline, UnderlineStyle::Single);
        assert!(spans[0].1.text_decoration.strikethrough);
    }

    #[test]
    fn ime_preedit_is_laid_out_at_the_document_cursor() {
        let mut document = RichComposeDocument::default();
        document.synchronize("alpha omega", 6, 6);
        let mut surface = CosmicComposeEditor::default();
        let rendered = surface.render(
            &document,
            document.selection(),
            320.0,
            120.0,
            0.0,
            1.0,
            ComposeEditorStyle {
                text: Color::from_rgb_u8(240, 240, 240),
                link: Color::from_rgb_u8(90, 150, 255),
                selection: Color::from_argb_u8(140, 60, 120, 220),
                selected_text: Color::from_rgb_u8(255, 255, 255),
            },
            "世界",
        );

        assert_eq!(surface.visual_text, "alpha 世界omega");
        assert_eq!(
            surface.selection_from_editor(),
            ComposeSelection { start: 6, end: 6 }
        );
        assert!(rendered.caret_x > 0.0);
        assert!(!rendered.tiles.is_empty());
    }

    #[test]
    fn line_breaks_render_at_every_caret_position() {
        let style = ComposeEditorStyle {
            text: Color::from_rgb_u8(240, 240, 240),
            link: Color::from_rgb_u8(90, 150, 255),
            selection: Color::from_argb_u8(140, 60, 120, 220),
            selected_text: Color::from_rgb_u8(255, 255, 255),
        };

        for text in ["\n", "first\n", "\nsecond", "first\nsecond"] {
            for offset in 0..=text.len() {
                if !text.is_char_boundary(offset) {
                    continue;
                }
                let mut document = RichComposeDocument::default();
                document.synchronize(text, offset as i32, offset as i32);
                let mut surface = CosmicComposeEditor::default();
                let rendered = surface.render(
                    &document,
                    document.selection(),
                    320.0,
                    120.0,
                    0.0,
                    1.0,
                    style,
                    "",
                );

                assert!(
                    rendered.content_height >= LINE_HEIGHT_LOGICAL,
                    "failed to lay out {text:?} at byte offset {offset}",
                );
                assert!(
                    !rendered.tiles.is_empty(),
                    "failed to render {text:?} at byte offset {offset}",
                );
                assert_eq!(
                    surface.selection_from_editor(),
                    ComposeSelection {
                        start: offset as i32,
                        end: offset as i32,
                    },
                    "caret mapping changed for {text:?} at byte offset {offset}",
                );
            }
        }
    }

    #[test]
    fn trailing_line_sentinel_is_not_part_of_the_editable_document() {
        let mut document = RichComposeDocument::default();
        document.synchronize("message\n", 8, 8);
        let mut surface = CosmicComposeEditor::default();
        surface.render(
            &document,
            document.selection(),
            320.0,
            120.0,
            0.0,
            1.0,
            ComposeEditorStyle {
                text: Color::from_rgb_u8(240, 240, 240),
                link: Color::from_rgb_u8(90, 150, 255),
                selection: Color::from_argb_u8(140, 60, 120, 220),
                selected_text: Color::from_rgb_u8(255, 255, 255),
            },
            "",
        );

        assert_eq!(document.text(), "message\n");
        assert_eq!(surface.visual_text, document.text());
        assert_eq!(
            surface.selection_from_editor(),
            ComposeSelection { start: 8, end: 8 },
        );
    }
}
