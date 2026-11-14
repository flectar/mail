use crate::rich_compose::{
    Alignment, BOLD, CODE, CharacterStyle, ComposeSelection, Followed, FontFamily,
    IMAGE_PLACEHOLDER, ITALIC, InlineImage, Rgb, RichComposeDocument, STRIKE, TextSize, UNDERLINE,
};
use cosmic_text::{
    Action, Align, Attrs, AttrsList, BidiParagraphs, Buffer, BufferLine, Color as CosmicColor,
    Cursor, Edit, Editor, Family, FontSystem, LineEnding, Metrics, Motion, PhysicalGlyph, Renderer,
    Scroll, Selection, Shaping, Style, SwashCache, SwashContent, UnderlineStyle, Weight, Wrap,
    fontdb::Source,
};
use slint::{Color, Image, Rgba8Pixel, SharedPixelBuffer};
use std::{
    cell::OnceCell,
    collections::{BTreeMap, HashMap},
    ops::{Deref, DerefMut, Range},
    sync::Arc,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const FONT_SIZE_LOGICAL: f32 = 13.0;
const LINE_HEIGHT_LOGICAL: f32 = 18.0;
pub(crate) const UI_FONT_FAMILY: &str = "Google Sans Flex";
pub(crate) const UI_FONT_DATA: &[u8] = include_bytes!(
    "../resources/fonts/google-sans-flex/GoogleSansFlex-VariableFont_GRAD,ROND,opsz,slnt,wdth,wght.ttf"
);
const TILE_HEIGHT_LOGICAL: f32 = 384.0;
const TILE_OVERSCAN: u32 = 1;
/// How soon a second click or tap counts as a double one.
pub(crate) const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(450);
const MULTI_CLICK_DISTANCE: f32 = 5.0;
// cosmic-text 0.19's rich-text path omits the final empty paragraph when the
// supplied text ends in a newline. Keep that paragraph alive in the layout
// with a zero-width, transparent character. This is renderer-only and never
// enters the editable document or its byte-offset mapping.
const TRAILING_LINE_SENTINEL: &str = "\u{200b}";
/// What an image's place holds in the layout: a zero-width space as wide and tall as the image,
/// which then wraps, lines up and takes the caret as text does. It's as long as the image
/// placeholder, so places in the text stay the same.
const IMAGE_SPACE: &str = "\u{200b}";
const _: () = assert!(IMAGE_SPACE.len() == IMAGE_PLACEHOLDER.len_utf8());

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComposeEditorStyle {
    pub text: Color,
    pub link: Color,
    pub selection: Color,
    pub selected_text: Color,
    /// The wavy line under misspelled words.
    pub misspelling: Color,
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
    /// Where the selected image shows, when the selection is one image: its left, top,
    /// width and height.
    pub image_box: Option<[f32; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LayoutKey {
    revision: u64,
    physical_width: u32,
    scale_bits: u32,
    text_color: u32,
    link_color: u32,
    /// How an empty last line lines up, which follows the caret.
    last_line_align: Alignment,
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
    paint_colors: Option<(u32, u32)>,
    layout_preedit_text: String,
    layout_preedit_offset: usize,
    visual_text: String,
    rendered_selection: Option<(i32, i32)>,
    content_height_physical: f32,
    tiles: BTreeMap<u32, RenderedComposeTile>,
    /// Misspelled words as byte ranges of the document at `misspellings_revision`, and the
    /// lines drawn under them as the cached tiles show them.
    misspellings: Vec<Range<usize>>,
    misspellings_revision: u64,
    /// A misspelled word found at the caret: the word being typed, marked once the caret
    /// leaves it.
    held_misspelling: Option<Range<usize>>,
    squiggles: Vec<Squiggle>,
    squiggle_color: u32,
    squiggles_stale: bool,
    pointer_down: bool,
    last_pointer_down: Option<(Instant, f32, f32)>,
    click_count: u8,
    /// The images the layout shows, by the slot their space's metadata names.
    layout_images: Vec<Arc<InlineImage>>,
    /// Images decoded at the size they're shown, premultiplied RGBA, by image and size: only
    /// the size each shows at now.
    image_pixels: HashMap<(u64, u32, u32), Arc<[u8]>>,
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
        // OpenHarmony ships no Fontconfig, so fontdb's system scan finds
        // nothing; load the platform font directory explicitly. Without it
        // the composer falls back to the bundled UI font alone, which has no
        // CJK coverage (typed Chinese renders as missing glyphs).
        if let Some(fonts) = crate::platform::email_fonts() {
            font_system.db_mut().load_fonts_dir(fonts.dir);
        }
        choose_generic_families(font_system.db_mut());
        let buffer = Buffer::new(
            &mut font_system,
            Metrics::new(FONT_SIZE_LOGICAL, LINE_HEIGHT_LOGICAL),
        );
        Self {
            font_system,
            swash_cache: SwashCache::new(),
            editor: Editor::new(buffer),
            layout_key: None,
            paint_colors: None,
            layout_preedit_text: String::new(),
            layout_preedit_offset: 0,
            visual_text: String::new(),
            rendered_selection: None,
            content_height_physical: LINE_HEIGHT_LOGICAL,
            tiles: BTreeMap::new(),
            misspellings: Vec::new(),
            misspellings_revision: 0,
            held_misspelling: None,
            squiggles: Vec::new(),
            squiggle_color: 0,
            squiggles_stale: false,
            pointer_down: false,
            last_pointer_down: None,
            click_count: 0,
            layout_images: Vec::new(),
            image_pixels: HashMap::new(),
        }
    }
}

impl CosmicComposeEditor {
    pub fn reset(&mut self) {
        self.layout_key = None;
        self.paint_colors = None;
        self.layout_preedit_text.clear();
        self.layout_preedit_offset = 0;
        self.visual_text.clear();
        self.rendered_selection = None;
        self.tiles.clear();
        self.misspellings.clear();
        self.held_misspelling = None;
        self.squiggles.clear();
        self.squiggles_stale = false;
        self.pointer_down = false;
        self.last_pointer_down = None;
        self.click_count = 0;
    }

    /// Underline `ranges`, the misspelled words of the document as it was at `revision`.
    /// They follow later edits; a word an edit changes loses its line until it is checked
    /// again. A newly found word at the caret is the word being typed, and waits for the caret
    /// to leave it.
    pub fn set_misspellings(
        &mut self,
        document: &RichComposeDocument,
        revision: u64,
        ranges: Vec<Range<usize>>,
    ) {
        self.follow_misspellings(document);
        let caret = collapsed_caret(document.selection());
        let mut misspellings = Vec::with_capacity(ranges.len());
        let mut held = None;
        for range in ranges {
            let Some(range) = follow_word(document, revision, range) else {
                continue;
            };
            let shown = self.misspellings.contains(&range);
            if !shown && caret.is_some_and(|caret| range.start <= caret && caret <= range.end) {
                held = Some(range);
            } else {
                misspellings.push(range);
            }
        }
        if misspellings != self.misspellings {
            self.misspellings = misspellings;
            self.squiggles_stale = true;
        }
        self.held_misspelling = held;
    }

    /// Stop underlining `word` anywhere in the document, as one the user accepted.
    pub fn accept_word(&mut self, document: &RichComposeDocument, word: &str) {
        self.follow_misspellings(document);
        let text = document.text();
        let is_word = |range: &Range<usize>| text.get(range.clone()) == Some(word);
        let before = self.misspellings.len();
        self.misspellings.retain(|range| !is_word(range));
        if self.misspellings.len() != before {
            self.squiggles_stale = true;
        }
        if self.held_misspelling.as_ref().is_some_and(is_word) {
            self.held_misspelling = None;
        }
    }

    /// The underlined word the caret is in or at the edge of.
    pub fn misspelling_at_caret(&mut self, document: &RichComposeDocument) -> Option<Range<usize>> {
        self.follow_misspellings(document);
        let caret = usize::try_from(document.selection().end).ok()?;
        self.misspellings
            .iter()
            .find(|range| range.start <= caret && caret <= range.end)
            .cloned()
    }

    /// The selection of the image at a point of the editor, in logical pixels, as last
    /// rendered.
    pub fn image_selection_at(
        &self,
        x: f32,
        y: f32,
        scale_factor: f32,
    ) -> Option<ComposeSelection> {
        let scale_factor = scale_factor.max(1.0);
        let (line, image) = self.image_at(x * scale_factor, y * scale_factor)?;
        let offset = |index| {
            self.visual_to_document_offset(cursor_to_global(
                &self.visual_text,
                Cursor::new(line, index),
            ))
        };
        Some(ComposeSelection {
            start: offset(image.start),
            end: offset(image.end),
        })
    }

    /// The underlined word at a point of the editor, in logical pixels, as last rendered.
    pub fn misspelling_at(&self, x: f32, y: f32, scale_factor: f32) -> Option<Range<usize>> {
        self.squiggle_at(x, y, scale_factor)
            .map(|squiggle| squiggle.start..squiggle.end)
    }

    /// The bottom of the line of the underlined word at a point of the editor, in logical
    /// pixels, where a menu for the word opens without covering it.
    pub fn misspelling_line_bottom(&self, x: f32, y: f32, scale_factor: f32) -> Option<f32> {
        self.squiggle_at(x, y, scale_factor)
            .map(|squiggle| squiggle.bottom / scale_factor.max(1.0))
    }

    fn squiggle_at(&self, x: f32, y: f32, scale_factor: f32) -> Option<&Squiggle> {
        let (x, y) = (x * scale_factor.max(1.0), y * scale_factor.max(1.0));
        self.squiggles.iter().find(|squiggle| {
            (squiggle.left..=squiggle.right).contains(&x)
                && (squiggle.top..=squiggle.bottom).contains(&y)
        })
    }

    /// Move the underlined words to where the edits since they were found left them, and mark
    /// a held word once the caret is off it.
    fn follow_misspellings(&mut self, document: &RichComposeDocument) {
        let revision = document.revision();
        if revision != self.misspellings_revision {
            if !self.misspellings.is_empty() {
                self.squiggles_stale = true;
            }
            self.misspellings = std::mem::take(&mut self.misspellings)
                .into_iter()
                .filter_map(|range| follow_word(document, self.misspellings_revision, range))
                .collect();
            self.held_misspelling = self
                .held_misspelling
                .take()
                .and_then(|range| follow_word(document, self.misspellings_revision, range));
            self.misspellings_revision = revision;
        }
        let caret = collapsed_caret(document.selection());
        if let Some(held) = self
            .held_misspelling
            .take_if(|held| !caret.is_some_and(|caret| held.start <= caret && caret <= held.end))
        {
            let at = self
                .misspellings
                .partition_point(|range| range.start < held.start);
            self.misspellings.insert(at, held);
            self.squiggles_stale = true;
        }
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
        let timing_start = crate::renderer::render_timings_enabled().then(Instant::now);
        let scale_factor = scale_factor.max(1.0);
        let relaid_out = self.ensure_layout(
            document,
            selection,
            logical_width,
            scale_factor,
            style,
            preedit_text,
        );
        let paint_colors = (
            style.selection.as_argb_encoded(),
            style.selected_text.as_argb_encoded(),
        );
        if self.paint_colors != Some(paint_colors) {
            let selection_painted = self
                .rendered_selection
                .is_some_and(|(start, end)| start != end)
                || selection.start != selection.end;
            if selection_painted {
                self.tiles.clear();
            }
            self.paint_colors = Some(paint_colors);
        }
        self.set_selection(selection);
        self.editor.shape_as_needed(&mut self.font_system, false);
        self.follow_misspellings(document);
        if relaid_out || self.squiggles_stale {
            self.squiggles_stale = false;
            let squiggles = self.squiggle_geometry(document, preedit_text);
            if squiggles != self.squiggles {
                self.squiggles = squiggles;
                self.tiles.clear();
            }
        }
        let squiggle_color = style.misspelling.as_argb_encoded();
        if squiggle_color != self.squiggle_color {
            if !self.squiggles.is_empty() {
                self.tiles.clear();
            }
            self.squiggle_color = squiggle_color;
        }

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
        let mut new_tiles = 0;
        let mut paint_time = Duration::ZERO;
        for index in wanted {
            if self.tiles.contains_key(&index) {
                continue;
            }
            let tile_start = index.saturating_mul(tile_height_physical);
            if tile_start >= content_height_physical {
                continue;
            }
            let tile_height = tile_height_physical.min(content_height_physical - tile_start);
            let paint_start = timing_start.map(|_| Instant::now());
            let image = self.render_tile(physical_width, tile_start, tile_height, style);
            if let Some(paint_start) = paint_start {
                paint_time += paint_start.elapsed();
            }
            new_tiles += 1;
            self.tiles.insert(
                index,
                RenderedComposeTile {
                    image,
                    y: tile_start as f32 / scale_factor,
                    height: tile_height as f32 / scale_factor,
                },
            );
        }

        let image_box = document
            .selected_image()
            .and_then(|_| self.image_box(selection, scale_factor));
        let rendered = RenderedComposeEditor {
            tiles: self.tiles.values().cloned().collect(),
            content_height: self.content_height_physical / scale_factor,
            caret_x,
            caret_y,
            caret_height,
            image_box,
        };
        if let Some(timing_start) = timing_start {
            let tile_bytes = self
                .tiles
                .values()
                .map(|tile| {
                    let size = tile.image.size();
                    u64::from(size.width) * u64::from(size.height) * 4
                })
                .sum::<u64>();
            eprintln!(
                "compose cpu frame: total={:.2}ms layout={} new_tiles={new_tiles} paint={:.2}ms retained_tiles={} retained_tile_bytes={tile_bytes}",
                timing_start.elapsed().as_secs_f64() * 1000.0,
                u8::from(relaid_out),
                paint_time.as_secs_f64() * 1000.0,
                self.tiles.len(),
            );
        }
        rendered
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
                // A click on an image selects it, as other editors select pictures.
                if let (false, 1, Some((line, image))) = (
                    shift,
                    self.click_count,
                    self.image_at(x_physical as f32, y_physical as f32),
                ) {
                    self.editor
                        .set_selection(Selection::Normal(Cursor::new(line, image.start)));
                    self.editor.set_cursor(Cursor::new(line, image.end));
                    return Some(self.selection_from_editor());
                }
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
            // Left and Right only collapse a selection. The other moves go on
            // from the edge they head toward, as text fields do: End from a
            // selection reaches the end of the line.
            if matches!(
                motion,
                Motion::Left | Motion::Right | Motion::Previous | Motion::Next
            ) {
                return Some(self.selection_from_editor());
            }
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
    ) -> bool {
        let physical_width = (logical_width.max(1.0) * scale_factor).ceil().max(1.0) as u32;
        let text = document.text();
        // An empty last line has no characters to hold its alignment: it lines up as typing
        // there would, in the caret's alignment while the caret is on it.
        let last_line_align = if valid_offset(text, selection.end) == text.len() {
            document.typing_style().align
        } else {
            text.len()
                .checked_sub(1)
                .map_or_else(Alignment::default, |end| document.paragraph_alignment(end))
        };
        let key = LayoutKey {
            revision: document.revision(),
            physical_width,
            scale_bits: scale_factor.to_bits(),
            text_color: style.text.as_argb_encoded(),
            link_color: style.link.as_argb_encoded(),
            last_line_align,
        };
        let preedit_offset = valid_offset(text, selection.end);
        if self.layout_key == Some(key)
            && self.layout_preedit_text == preedit_text
            && (preedit_text.is_empty() || self.layout_preedit_offset == preedit_offset)
        {
            return false;
        }

        let metrics = Metrics::new(
            FONT_SIZE_LOGICAL * scale_factor,
            LINE_HEIGHT_LOGICAL * scale_factor,
        );
        let default_attrs = Attrs::new()
            .family(Family::Name(UI_FONT_FAMILY))
            .color(cosmic_color(style.text));
        let look = TextLook {
            dark: luminance(style.text) > 0.5,
            metrics,
            link: cosmic_color(style.link),
            width: physical_width as f32,
        };
        let mut images = Vec::new();
        let mut spans = rich_spans(
            document,
            preedit_offset,
            preedit_text,
            &default_attrs,
            look,
            &mut images,
        );
        self.visual_text.clear();
        self.visual_text
            .push_str(&document.text()[..preedit_offset]);
        self.visual_text.push_str(preedit_text);
        self.visual_text
            .push_str(&document.text()[preedit_offset..]);
        if self.visual_text.ends_with('\n') {
            spans.push((
                TRAILING_LINE_SENTINEL,
                default_attrs.clone().color(CosmicColor::rgba(0, 0, 0, 0)),
            ));
        }

        // Each paragraph lines up as the document says, found from where it starts in the text
        // laid out, which holds any IME composition.
        let align_at = |visual_start: usize| {
            let start = if visual_start <= preedit_offset {
                visual_start
            } else {
                visual_start
                    .saturating_sub(preedit_text.len())
                    .max(preedit_offset)
            }
            .min(text.len());
            let align = if start == text.len() && (text.is_empty() || text.ends_with('\n')) {
                last_line_align
            } else {
                let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
                document.paragraph_alignment(line_start)
            };
            match align {
                Alignment::Left => None,
                Alignment::Center => Some(Align::Center),
                Alignment::Right => Some(Align::Right),
            }
        };
        let lines = rich_lines(&spans, &default_attrs, align_at);
        let font_system = &mut self.font_system;
        self.editor.with_buffer_mut(|buffer| {
            buffer.set_metrics_and_size(metrics, Some(physical_width as f32), None);
            buffer.set_wrap(Wrap::WordOrGlyph);
            // Shaping costs grow with the whole draft, including a reply's
            // quoted text, so only the paragraphs an edit changed are replaced.
            let replaced = replace_changed_lines(buffer, lines);
            buffer.set_scroll(Scroll::default());
            // cosmic-text does not count a new line as out of date, so its
            // lazy pass would skip it: lay the replaced lines out here.
            for line in replaced {
                buffer.line_layout(font_system, line);
            }
            buffer.set_redraw(true);
        });
        self.editor.shape_as_needed(&mut self.font_system, false);
        self.content_height_physical = self.editor.with_buffer(|buffer| {
            buffer
                .layout_runs()
                .map(|run| run.line_top + run.line_height)
                .fold(metrics.line_height, f32::max)
        });
        self.layout_key = Some(key);
        self.image_pixels
            .retain(|(id, _, _), _| images.iter().any(|image| image.id == *id));
        self.layout_images = images;
        self.layout_preedit_text.clear();
        self.layout_preedit_text.push_str(preedit_text);
        self.layout_preedit_offset = preedit_offset;
        self.rendered_selection = None;
        self.tiles.clear();
        true
    }

    fn set_selection(&mut self, selection: ComposeSelection) {
        if self.rendered_selection == Some((selection.start, selection.end)) {
            return;
        }
        let previous = self
            .rendered_selection
            .and_then(|offsets| self.selection_cursor_bounds(offsets));
        let current = self.selection_cursor_bounds((selection.start, selection.end));
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
        if previous.is_some() || current.is_some() {
            self.invalidate_selection_tiles([previous, current]);
        }
    }

    fn selection_cursor_bounds(&self, (start, end): (i32, i32)) -> Option<(Cursor, Cursor)> {
        if start == end || !self.layout_preedit_text.is_empty() {
            return None;
        }
        let start = global_to_cursor(&self.visual_text, start);
        let end = global_to_cursor(&self.visual_text, end);
        (start != end).then_some((start.min(end), start.max(end)))
    }

    fn invalidate_selection_tiles(&mut self, bounds: [Option<(Cursor, Cursor)>; 2]) {
        let Some(layout) = self.layout_key else {
            self.tiles.clear();
            return;
        };
        let tile_height = (TILE_HEIGHT_LOGICAL * f32::from_bits(layout.scale_bits)).ceil();
        let mut extents: [Option<(f32, f32)>; 2] = [None, None];
        let last_line = bounds
            .into_iter()
            .flatten()
            .map(|(_, end)| end.line)
            .max()
            .unwrap_or(0);
        self.editor.with_buffer(|buffer| {
            for run in buffer.layout_runs() {
                if run.line_i > last_line {
                    break;
                }
                for (bound, extent) in bounds.into_iter().zip(extents.iter_mut()) {
                    let Some((start, end)) = bound else {
                        continue;
                    };
                    if run.line_i < start.line || run.line_i > end.line {
                        continue;
                    }
                    // A logical paragraph can wrap over many raster tiles.
                    // Follow Cosmic Text's selected-glyph predicate so runs
                    // outside the selected byte range retain their pixels.
                    let selected_glyph = run.glyphs.iter().any(|glyph| {
                        (start.line != run.line_i || glyph.end > start.index)
                            && (end.line != run.line_i || glyph.start < end.index)
                    });
                    let selected_empty_line = run.glyphs.is_empty() && end.line > run.line_i;
                    if !selected_glyph && !selected_empty_line {
                        continue;
                    }
                    let top = run.line_top;
                    let bottom = run.line_top + run.line_height;
                    *extent = Some(match *extent {
                        Some((first, last)) => (first.min(top), last.max(bottom)),
                        None => (top, bottom),
                    });
                }
            }
        });
        if tile_height <= 0.0
            || !tile_height.is_finite()
            || bounds
                .into_iter()
                .zip(extents.iter())
                .any(|(bound, extent)| bound.is_some() && extent.is_none())
            || extents
                .iter()
                .flatten()
                .any(|(top, bottom)| !top.is_finite() || !bottom.is_finite())
        {
            self.tiles.clear();
            return;
        }
        let ranges = extents.map(|extent| {
            extent.map(|(top, bottom)| {
                let first = ((top.max(0.0) / tile_height).floor() as u32).saturating_sub(1);
                let last = ((bottom.max(0.0) / tile_height).floor() as u32).saturating_add(1);
                first..=last
            })
        });
        self.tiles
            .retain(|index, _| !ranges.iter().flatten().any(|range| range.contains(index)));
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
        let (x, line_height) = self.editor.with_buffer(|buffer| {
            let run = buffer.layout_runs().find(|run| {
                run.line_i == cursor.line
                    && run.cursor_position(&cursor).is_some()
                    && (run.line_top - y as f32).abs() < 0.5
            });
            let Some(run) = run else {
                return (x as f32, LINE_HEIGHT_LOGICAL * scale_factor);
            };
            // cosmic-text puts the caret at the start of a line with nothing on it, whatever
            // its alignment.
            let width = buffer.size().0.unwrap_or_default();
            let x = match buffer.lines[cursor.line].align() {
                Some(Align::Center) if run.glyphs.is_empty() => width / 2.0,
                Some(Align::Right) if run.glyphs.is_empty() => width,
                _ => x as f32,
            };
            (x, run.line_height)
        });
        (
            x / scale_factor,
            y as f32 / scale_factor,
            line_height / scale_factor,
        )
    }

    #[cfg(test)]
    fn glyph_left(&self, offset: usize) -> f32 {
        self.editor.with_buffer(|buffer| {
            buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter())
                .find(|glyph| glyph.start >= offset)
                .map_or(f32::MAX, |glyph| glyph.x)
        })
    }

    /// Where the lines under misspelled words go in the current layout, one per visual line a
    /// word is on. None while the IME composes, since the laid-out text then differs.
    fn squiggle_geometry(
        &self,
        document: &RichComposeDocument,
        preedit_text: &str,
    ) -> Vec<Squiggle> {
        if self.misspellings.is_empty() || !preedit_text.is_empty() {
            return Vec::new();
        }
        let text = document.text();
        let mut squiggles = Vec::new();
        self.editor.with_buffer(|buffer| {
            for range in &self.misspellings {
                let (Ok(start), Ok(end)) = (i32::try_from(range.start), i32::try_from(range.end))
                else {
                    continue;
                };
                let (start, end) = (global_to_cursor(text, start), global_to_cursor(text, end));
                if start.line != end.line || start.index >= end.index {
                    continue;
                }
                for run in buffer.layout_runs().filter(|run| run.line_i == start.line) {
                    let (left, right) = run
                        .glyphs
                        .iter()
                        .filter(|glyph| glyph.start < end.index && glyph.end > start.index)
                        .fold((f32::MAX, f32::MIN), |(left, right), glyph| {
                            (left.min(glyph.x), right.max(glyph.x + glyph.w))
                        });
                    if left < right {
                        squiggles.push(Squiggle {
                            left,
                            right,
                            baseline: run.line_y,
                            top: run.line_top,
                            bottom: run.line_top + run.line_height,
                            start: range.start,
                            end: range.end,
                        });
                    }
                }
            }
        });
        squiggles
    }

    fn render_tile(
        &mut self,
        width: u32,
        tile_start: u32,
        tile_height: u32,
        style: ComposeEditorStyle,
    ) -> Image {
        let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(width, tile_height);
        {
            let mut canvas =
                TileCanvas::new(width, tile_height, tile_start, pixels.make_mut_bytes());
            let editor = &self.editor;
            // Images and highlights go behind the text and the selection.
            let mut images = Vec::new();
            editor.with_buffer(|buffer| {
                let tile_end = tile_start.saturating_add(tile_height) as f32;
                for run in buffer.layout_runs() {
                    if run.line_top > tile_end {
                        break;
                    }
                    if run.line_top + run.line_height < tile_start as f32 {
                        continue;
                    }
                    for glyph in run.glyphs.iter() {
                        if glyph.metadata & IMAGE != 0 {
                            images.push((
                                glyph.metadata & 0xff_ff_ff,
                                glyph.x,
                                glyph.w,
                                run.line_top + run.line_height,
                            ));
                            continue;
                        }
                        if glyph.metadata & HIGHLIGHTED == 0 {
                            continue;
                        }
                        let left = glyph.x.floor();
                        canvas.rectangle(
                            left as i32,
                            run.line_top.floor() as i32,
                            (glyph.x + glyph.w - left).ceil() as u32,
                            run.line_height.ceil() as u32,
                            cosmic_rgb((glyph.metadata & 0xff_ff_ff) as Rgb),
                        );
                    }
                }
            });
            for (slot, x, width, bottom) in images {
                let Some(image) = self.layout_images.get(slot) else {
                    continue;
                };
                // Bottom up on its line, as images sit on the text's line in a message.
                let width = width.round().max(1.0) as u32;
                let height = (width as f32 * image.height as f32 / image.width.max(1) as f32)
                    .round()
                    .max(1.0) as u32;
                let key = (image.id, width, height);
                let pixels = match self.image_pixels.get(&key) {
                    Some(pixels) => pixels.clone(),
                    None => {
                        // Only the size it shows at now is kept, as the window or the image
                        // changes size.
                        self.image_pixels.retain(|(id, _, _), _| *id != image.id);
                        let pixels = decode_image(image, width, height);
                        self.image_pixels.insert(key, pixels.clone());
                        pixels
                    }
                };
                canvas.image(
                    x.round() as i32,
                    (bottom - height as f32).round() as i32,
                    width,
                    height,
                    &pixels,
                );
            }
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
            let scale = self
                .layout_key
                .map_or(1.0, |key| f32::from_bits(key.scale_bits));
            for squiggle in &self.squiggles {
                draw_squiggle(
                    &mut canvas,
                    squiggle,
                    scale,
                    cosmic_color(style.misspelling),
                );
            }
        }
        Image::from_rgba8_premultiplied(pixels)
    }

    /// Where the image that starts the selection shows, in logical pixels, as the tiles paint
    /// it: on the bottom of its line.
    fn image_box(&self, selection: ComposeSelection, scale_factor: f32) -> Option<[f32; 4]> {
        if !self.layout_preedit_text.is_empty() {
            return None;
        }
        let cursor = global_to_cursor(&self.visual_text, selection.start.min(selection.end));
        self.editor.with_buffer(|buffer| {
            buffer
                .layout_runs()
                .filter(|run| run.line_i == cursor.line)
                .find_map(|run| {
                    let glyph = run
                        .glyphs
                        .iter()
                        .find(|glyph| glyph.metadata & IMAGE != 0 && glyph.start == cursor.index)?;
                    let image = self.layout_images.get(glyph.metadata & 0xff_ff_ff)?;
                    let width = glyph.w.round().max(1.0);
                    let height = (width * image.height as f32 / image.width.max(1) as f32)
                        .round()
                        .max(1.0);
                    let top = (run.line_top + run.line_height - height).round();
                    Some([
                        glyph.x.round() / scale_factor,
                        top / scale_factor,
                        width / scale_factor,
                        height / scale_factor,
                    ])
                })
        })
    }

    /// The image under a point of the layout, in physical pixels: its line, and its place in
    /// the line.
    fn image_at(&self, x: f32, y: f32) -> Option<(usize, Range<usize>)> {
        self.editor.with_buffer(|buffer| {
            buffer
                .layout_runs()
                .filter(|run| y >= run.line_top && y < run.line_top + run.line_height)
                .find_map(|run| {
                    run.glyphs
                        .iter()
                        .find(|glyph| {
                            glyph.metadata & IMAGE != 0 && x >= glyph.x && x < glyph.x + glyph.w
                        })
                        .map(|glyph| (run.line_i, glyph.start..glyph.end))
                })
        })
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

/// Fonts for serif and monospace text in the editor, best first: those the message names for
/// its readers, then ones systems ship that look like them.
const SERIF_FONTS: [&str; 7] = [
    "Times New Roman",
    "Times",
    "Liberation Serif",
    "Tinos",
    "Noto Serif",
    "DejaVu Serif",
    "Georgia",
];
const MONOSPACE_FONTS: [&str; 9] = [
    "Courier New",
    "Courier",
    "Liberation Mono",
    "Cousine",
    "Menlo",
    "Consolas",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Droid Sans Mono",
];

/// Points the serif and monospace families at the first of their fonts installed. cosmic-text
/// picks fonts of some Linux systems, which Windows and macOS don't have, and a family without
/// its font falls back to another kind of type.
fn choose_generic_families(db: &mut cosmic_text::fontdb::Database) {
    let installed = |fonts: &[&'static str]| {
        fonts.iter().copied().find(|font| {
            db.faces().any(|face| {
                face.families
                    .iter()
                    .any(|(family, _)| family.eq_ignore_ascii_case(font))
            })
        })
    };
    let (serif, monospace) = (installed(&SERIF_FONTS), installed(&MONOSPACE_FONTS));
    if let Some(serif) = serif {
        db.set_serif_family(serif);
    }
    if let Some(monospace) = monospace {
        db.set_monospace_family(monospace);
    }
}

fn rich_spans<'a>(
    document: &'a RichComposeDocument,
    preedit_offset: usize,
    preedit_text: &'a str,
    default_attrs: &Attrs<'static>,
    look: TextLook,
    images: &mut Vec<Arc<InlineImage>>,
) -> Vec<(&'a str, Attrs<'static>)> {
    let runs = document.style_runs();
    let mut spans = Vec::with_capacity(runs.len().saturating_add(2).max(1));
    let mut inserted_preedit = preedit_text.is_empty();
    let preedit_attrs = character_attrs(default_attrs, document.typing_style(), look)
        .underline(UnderlineStyle::Single);

    for run in runs {
        if let Some(image) = &run.style.image {
            // Each placeholder becomes the image's space. The IME composes between them.
            let slot = images.len();
            images.push(image.clone());
            let attrs = image_attrs(default_attrs, image, look, slot);
            for (offset, _) in document.text()[run.range.clone()].char_indices() {
                if !inserted_preedit && preedit_offset <= run.range.start + offset {
                    push_span(&mut spans, preedit_text, preedit_attrs.clone());
                    inserted_preedit = true;
                }
                push_span(&mut spans, IMAGE_SPACE, attrs.clone());
            }
            continue;
        }
        let attrs = character_attrs(default_attrs, &run.style, look);
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
        spans.push(("", default_attrs.clone()));
    }
    spans
}

/// A paragraph to lay out: its text, styles and alignment.
type RichLine = (String, AttrsList, Option<Align>);

/// Splits styled spans into paragraphs the way `Buffer::set_rich_text` does, each aligned as
/// `align_at` says for where it starts.
fn rich_lines(
    spans: &[(&str, Attrs<'static>)],
    default_attrs: &Attrs<'static>,
    align_at: impl Fn(usize) -> Option<Align>,
) -> Vec<RichLine> {
    let mut text = String::with_capacity(spans.iter().map(|(span, _)| span.len()).sum());
    let mut ranges = Vec::with_capacity(spans.len());
    for (span, attrs) in spans {
        let start = text.len();
        text.push_str(span);
        ranges.push((start..text.len(), attrs));
    }
    let base = text.as_ptr() as usize;
    let mut lines = Vec::new();
    let mut first_span = 0;
    for paragraph in BidiParagraphs::new(&text) {
        let start = paragraph.as_ptr() as usize - base;
        let end = start + paragraph.len();
        while ranges
            .get(first_span)
            .is_some_and(|(range, _)| range.end <= start)
        {
            first_span += 1;
        }
        let mut attrs_list = AttrsList::new(default_attrs);
        for (range, attrs) in ranges[first_span..]
            .iter()
            .take_while(|(range, _)| range.start < end)
        {
            let span_start = range.start.max(start);
            let span_end = range.end.min(end);
            if span_start < span_end && **attrs != attrs_list.defaults() {
                attrs_list.add_span(span_start - start..span_end - start, attrs);
            }
        }
        lines.push((paragraph.to_owned(), attrs_list, align_at(start)));
    }
    if lines.is_empty() {
        lines.push((String::new(), AttrsList::new(default_attrs), align_at(0)));
    }
    lines
}

/// Keeps the shaped lines an edit left alone. Only the changed middle is
/// replaced, so typing reshapes one paragraph instead of the whole draft.
/// Returns the indexes of the new lines.
fn replace_changed_lines(buffer: &mut Buffer, lines: Vec<RichLine>) -> std::ops::Range<usize> {
    let unchanged = |line: &BufferLine, (text, attrs, align): &RichLine| {
        line.text() == text
            && line.attrs_list() == attrs
            && line.ending() == LineEnding::default()
            && line.align() == *align
    };
    let prefix = buffer
        .lines
        .iter()
        .zip(&lines)
        .take_while(|(line, wanted)| unchanged(line, wanted))
        .count();
    let available = buffer.lines.len().min(lines.len()) - prefix;
    let suffix = buffer
        .lines
        .iter()
        .rev()
        .zip(lines.iter().rev())
        .take(available)
        .take_while(|(line, wanted)| unchanged(line, wanted))
        .count();
    let old_end = buffer.lines.len() - suffix;
    let new_end = lines.len() - suffix;
    let replacement = lines
        .into_iter()
        .take(new_end)
        .skip(prefix)
        .map(|(text, attrs, align)| {
            let mut line = BufferLine::new(text, LineEnding::default(), attrs, Shaping::Advanced);
            line.set_align(align);
            line
        });
    buffer.lines.splice(prefix..old_end, replacement);
    prefix..new_end
}

fn push_span<'a>(spans: &mut Vec<(&'a str, Attrs<'static>)>, text: &'a str, attrs: Attrs<'static>) {
    if !text.is_empty() {
        spans.push((text, attrs));
    }
}

/// What styled text looks like in the editor: the theme's darkness, the size of normal text
/// and the color of links.
#[derive(Clone, Copy)]
struct TextLook {
    dark: bool,
    metrics: Metrics,
    link: CosmicColor,
    /// The width text lays out in, which images shrink to fit.
    width: f32,
}

/// Marks a glyph's metadata as highlighted, with the highlight's color in its low bits.
const HIGHLIGHTED: usize = 1 << 24;
/// Marks a glyph's metadata as an image's space, with its slot in the layout's images in its low
/// bits.
const IMAGE: usize = 1 << 25;

/// An image's size in the layout, in physical pixels: as shown in the message, and no wider
/// than the text.
fn image_layout_size(image: &InlineImage, look: TextLook) -> (f32, f32) {
    let scale = look.metrics.font_size / FONT_SIZE_LOGICAL;
    let (width, height) = image.shown_size();
    let width = width as f32 * scale;
    let height = height as f32 * scale;
    let fit = (look.width / width).min(1.0);
    (
        (width * fit).max(1.0).floor(),
        (height * fit).max(1.0).round(),
    )
}

/// The attributes of an image's space: transparent, as wide as the image by its letter
/// spacing, and with a line as tall as the image.
fn image_attrs(
    default_attrs: &Attrs<'static>,
    image: &InlineImage,
    look: TextLook,
    slot: usize,
) -> Attrs<'static> {
    let (width, height) = image_layout_size(image, look);
    default_attrs
        .clone()
        .color(CosmicColor::rgba(0, 0, 0, 0))
        .letter_spacing(width / look.metrics.font_size)
        .metrics(Metrics::new(look.metrics.font_size, height))
        .metadata(IMAGE | slot)
}

fn character_attrs(
    default_attrs: &Attrs<'static>,
    style: &CharacterStyle,
    look: TextLook,
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
    match style.family {
        FontFamily::Default => {}
        FontFamily::Serif => attrs = attrs.family(Family::Serif),
        FontFamily::Monospace => attrs = attrs.family(Family::Monospace),
    }
    if style.marks & CODE != 0 {
        attrs = attrs.family(Family::Monospace);
    }
    if style.size != TextSize::Normal {
        let scale = style.size.scale();
        attrs = attrs.metrics(Metrics::new(
            look.metrics.font_size * scale,
            look.metrics.line_height * scale,
        ));
    }
    if style.link.is_some() {
        attrs = attrs.color(look.link);
    }
    match (style.highlight, style.color) {
        // Text on a highlight keeps to the highlight's contrast in either theme.
        (Some(highlight), color) => {
            attrs = attrs
                .metadata(HIGHLIGHTED | highlight as usize)
                .color(cosmic_rgb(color.unwrap_or(text_on(highlight))));
        }
        (None, Some(color)) => attrs = attrs.color(cosmic_rgb(shown_color(color, look.dark))),
        (None, None) => {}
    }
    attrs
}

fn cosmic_rgb(color: Rgb) -> CosmicColor {
    CosmicColor::rgb((color >> 16) as u8, (color >> 8) as u8, color as u8)
}

/// Relative luminance, as WCAG defines it.
fn rgb_luminance(color: Rgb) -> f32 {
    let channel = |shift: u32| {
        let value = ((color >> shift) & 0xff) as f32 / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

fn luminance(color: Color) -> f32 {
    rgb_luminance(
        (u32::from(color.red()) << 16) | (u32::from(color.green()) << 8) | u32::from(color.blue()),
    )
}

/// Near-black or white, whichever reads on `highlight`.
fn text_on(highlight: Rgb) -> Rgb {
    if rgb_luminance(highlight) > 0.18 {
        0x20_21_24
    } else {
        0xff_ff_ff
    }
}

/// A text color as the editor shows it: as chosen, or lightened toward white on a dark theme
/// when it would be too dark to read there. The message keeps the chosen color.
fn shown_color(color: Rgb, dark: bool) -> Rgb {
    if !dark || rgb_luminance(color) >= 0.18 {
        return color;
    }
    let lighten = |shift: u32| {
        let value = (color >> shift) & 0xff;
        (value + (0xff - value) / 2) << shift
    };
    lighten(16) | lighten(8) | lighten(0)
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

/// The wavy line under a misspelled word on one visual line, in physical pixels, with the
/// line's extent and the word's byte range for finding the word under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Squiggle {
    left: f32,
    right: f32,
    baseline: f32,
    top: f32,
    bottom: f32,
    start: usize,
    end: usize,
}

/// The caret's byte offset, unless text is selected.
fn collapsed_caret(selection: ComposeSelection) -> Option<usize> {
    (selection.start == selection.end)
        .then(|| usize::try_from(selection.end).ok())
        .flatten()
}

/// Where a word found in the document at `revision` is now, if it is still that whole word.
fn follow_word(
    document: &RichComposeDocument,
    revision: u64,
    range: Range<usize>,
) -> Option<Range<usize>> {
    match document.follow(revision, range) {
        Followed::Kept(range) | Followed::Bordered(range)
            if is_whole_word(document.text(), &range) =>
        {
            Some(range)
        }
        _ => None,
    }
}

/// Whether `range` of `text` is one whole word, as spell checking splits text into words. An
/// edit nearby can join a word to its neighbour, as typing a letter after "teh." does.
fn is_whole_word(text: &str, range: &Range<usize>) -> bool {
    if range.start >= range.end
        || range.end > text.len()
        || !text.is_char_boundary(range.start)
        || !text.is_char_boundary(range.end)
    {
        return false;
    }
    // Word breaks depend on a few characters around them, so a window splits the same.
    let mut from = range.start.saturating_sub(32);
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (range.end + 32).min(text.len());
    while !text.is_char_boundary(to) {
        to += 1;
    }
    text[from..to]
        .unicode_word_indices()
        .any(|(start, word)| from + start == range.start && word.len() == range.len())
}

/// A wave of `scale` pixels' amplitude and line width, two below the baseline, antialiased
/// along its normal so steep parts don't thin out.
fn draw_squiggle(canvas: &mut TileCanvas<'_>, squiggle: &Squiggle, scale: f32, color: CosmicColor) {
    let amplitude = scale;
    let wavelength = 4.0 * scale;
    let half_width = 0.5 * scale.max(1.0);
    let middle = squiggle.baseline + 2.0 * scale + amplitude;
    let top = (middle - amplitude - half_width - 1.0).floor() as i32;
    let bottom = (middle + amplitude + half_width + 1.0).ceil() as i32;
    if bottom < canvas.y_offset as i32 || top > canvas.y_offset.saturating_add(canvas.height) as i32
    {
        return;
    }
    let tau = std::f32::consts::TAU;
    for x in squiggle.left.floor() as i32..squiggle.right.ceil() as i32 {
        let phase = (x as f32 + 0.5 - squiggle.left) / wavelength * tau;
        let centre = middle - amplitude * phase.sin();
        let slope = amplitude * tau / wavelength * phase.cos();
        let reach = half_width * (1.0 + slope * slope).sqrt();
        for y in (centre - reach - 1.0).floor() as i32..=(centre + reach + 1.0).ceil() as i32 {
            let coverage = (reach + 0.5 - (y as f32 + 0.5 - centre).abs()).clamp(0.0, 1.0);
            if coverage > 0.0 {
                let alpha = (f32::from(color.a()) * coverage).round() as u8;
                canvas.blend(
                    x,
                    y,
                    CosmicColor::rgba(color.r(), color.g(), color.b(), alpha),
                );
            }
        }
    }
}

struct TileCanvas<'a> {
    width: u32,
    height: u32,
    y_offset: u32,
    pixels: &'a mut [u8],
}

impl<'a> TileCanvas<'a> {
    fn new(width: u32, height: u32, y_offset: u32, pixels: &'a mut [u8]) -> Self {
        Self {
            width,
            height,
            y_offset,
            pixels,
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

    /// Draws `pixels`, premultiplied RGBA `width` by `height`, with its top left at `x`, `y`.
    fn image(&mut self, x: i32, y: i32, width: u32, height: u32, pixels: &[u8]) {
        let top = self.y_offset as i32;
        let rows = (y.max(top)
            ..y.saturating_add(height as i32)
                .min(top + self.height as i32))
            .map(|row| row - y);
        for row in rows {
            for column in x.max(0)..x.saturating_add(width as i32).min(self.width as i32) {
                let source = ((row as usize * width as usize) + (column - x) as usize) * 4;
                let Some(source) = pixels.get(source..source + 4) else {
                    continue;
                };
                let index = (((y + row) as u32 - self.y_offset) as usize * self.width as usize
                    + column as usize)
                    * 4;
                let destination = &mut self.pixels[index..index + 4];
                let inverse = 255 - u32::from(source[3]);
                for (destination, source) in destination.iter_mut().zip(source) {
                    *destination = (u32::from(*source) + u32::from(*destination) * inverse / 255)
                        .min(255) as u8;
                }
            }
        }
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
}

struct TileRenderer<'a, 'pixels> {
    font_system: &'a mut FontSystem,
    swash_cache: &'a mut SwashCache,
    canvas: &'a mut TileCanvas<'pixels>,
}

impl Renderer for TileRenderer<'_, '_> {
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

/// An image's pixels at `width` by `height`, turned upright as its camera recorded, as
/// premultiplied RGBA. One it can't decode shows as a gray box.
fn decode_image(image: &InlineImage, width: u32, height: u32) -> Arc<[u8]> {
    let decoded = (|| {
        // A photo is read at little more than the size it shows, rather than all of it.
        let read = if image.mime_type == "image/jpeg" {
            crate::photo_shrink::read_jpeg(&image.data, width, height)
        } else {
            None
        };
        let picture = match read {
            Some(picture) => picture,
            None => {
                let mut decoder = image::ImageReader::new(std::io::Cursor::new(&*image.data))
                    .with_guessed_format()
                    .ok()?
                    .into_decoder()
                    .ok()?;
                let orientation = image::ImageDecoder::orientation(&mut decoder).ok();
                let mut picture = image::DynamicImage::from_decoder(decoder).ok()?;
                if let Some(orientation) = orientation {
                    picture.apply_orientation(orientation);
                }
                picture
            }
        };
        Some(
            picture
                .resize_exact(width, height, image::imageops::FilterType::Triangle)
                .into_rgba8()
                .into_raw(),
        )
    })();
    let mut pixels = decoded
        .unwrap_or_else(|| [0xc8, 0xc8, 0xc8, 0xff].repeat(width as usize * height as usize));
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    pixels.into()
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
    fn selection_color_changes_repaint_without_relayout() {
        let mut document = RichComposeDocument::default();
        document.synchronize("selected phrase and more", 0, 15);
        let base = ComposeEditorStyle {
            text: Color::from_rgb_u8(30, 30, 30),
            link: Color::from_rgb_u8(0, 90, 180),
            selection: Color::from_argb_u8(140, 60, 120, 220),
            selected_text: Color::from_rgb_u8(255, 255, 255),
            misspelling: Color::from_rgb_u8(183, 55, 49),
        };
        for scale in [1.0, 1.5, 2.0] {
            let mut surface = CosmicComposeEditor::default();
            let draw =
                |surface: &mut CosmicComposeEditor, document: &RichComposeDocument, style| {
                    surface.render(
                        document,
                        document.selection(),
                        320.0,
                        120.0,
                        0.0,
                        scale,
                        style,
                        "",
                    )
                };
            let initial = draw(&mut surface, &document, base);
            let original_layout = surface.layout_key;
            let highlight = ComposeEditorStyle {
                selection: Color::from_argb_u8(140, 220, 70, 40),
                ..base
            };
            assert!(!surface.ensure_layout(
                &document,
                document.selection(),
                320.0,
                scale,
                highlight,
                "",
            ));
            let highlighted = draw(&mut surface, &document, highlight);
            let expected = draw(&mut CosmicComposeEditor::default(), &document, highlight);
            let pixels = |frame: &RenderedComposeEditor| {
                frame.tiles[0]
                    .image
                    .to_rgba8_premultiplied()
                    .unwrap()
                    .as_bytes()
                    .to_vec()
            };
            assert_eq!(surface.layout_key, original_layout);
            assert_ne!(pixels(&initial), pixels(&highlighted));
            assert_eq!(pixels(&highlighted), pixels(&expected));

            let selected_text = ComposeEditorStyle {
                selected_text: Color::from_rgb_u8(20, 220, 20),
                ..highlight
            };
            assert!(!surface.ensure_layout(
                &document,
                document.selection(),
                320.0,
                scale,
                selected_text,
                "",
            ));
            let recolored = draw(&mut surface, &document, selected_text);
            let expected = draw(
                &mut CosmicComposeEditor::default(),
                &document,
                selected_text,
            );
            assert_eq!(surface.layout_key, original_layout);
            assert_ne!(pixels(&highlighted), pixels(&recolored));
            assert_eq!(pixels(&recolored), pixels(&expected));

            document.update_selection(15, 15);
            let collapsed = draw(&mut surface, &document, selected_text);
            let backing = collapsed.tiles[0].image.to_rgba8_premultiplied().unwrap();
            let no_selection = draw(&mut surface, &document, base);
            let unchanged = no_selection.tiles[0]
                .image
                .to_rgba8_premultiplied()
                .unwrap();
            assert!(std::ptr::eq(backing.as_bytes(), unchanged.as_bytes()));
            assert_eq!(surface.layout_key, original_layout);
            document.update_selection(0, 15);
        }
    }

    #[test]
    fn misspellings_are_underlined_under_their_words_only() {
        let mut document = RichComposeDocument::default();
        document.load("Teh cat sat", 0, 0);
        document.update_selection(11, 11);
        let style = ComposeEditorStyle {
            text: Color::from_rgb_u8(30, 30, 30),
            link: Color::from_rgb_u8(0, 90, 180),
            selection: Color::from_argb_u8(140, 60, 120, 220),
            selected_text: Color::from_rgb_u8(255, 255, 255),
            misspelling: Color::from_rgb_u8(255, 0, 0),
        };
        for scale in [1.0, 2.0] {
            let render = |surface: &mut CosmicComposeEditor| {
                let rendered = surface.render(
                    &document,
                    document.selection(),
                    320.0,
                    120.0,
                    0.0,
                    scale,
                    style,
                    "",
                );
                rendered.tiles[0]
                    .image
                    .to_rgba8_premultiplied()
                    .unwrap()
                    .as_bytes()
                    .to_vec()
            };
            let mut plain = CosmicComposeEditor::default();
            let mut marked = CosmicComposeEditor::default();
            let without = render(&mut plain);
            let teh = 0.."Teh".len();
            marked.set_misspellings(&document, document.revision(), vec![teh]);
            let with = render(&mut marked);
            let [squiggle] = marked.squiggles[..] else {
                panic!("one squiggle expected, got {:?}", marked.squiggles);
            };
            let width = (320.0 * scale).ceil() as usize;
            let changed: Vec<(usize, usize)> = with
                .as_chunks::<4>()
                .0
                .iter()
                .zip(without.as_chunks::<4>().0)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, _)| (i % width, i / width))
                .collect();
            assert!(!changed.is_empty());
            for (x, y) in changed {
                assert!(
                    x as f32 >= squiggle.left - 1.0 && x as f32 <= squiggle.right + 1.0,
                    "pixel {x},{y} outside {squiggle:?}"
                );
                assert!(
                    y as f32 >= squiggle.baseline,
                    "pixel {x},{y} above {squiggle:?}"
                );
            }
            // The word "cat" after it stays clear.
            let cat = document.text().find("cat").unwrap();
            assert!(squiggle.right < marked.glyph_left(cat));

            // Taking the misspelling back repaints the plain text.
            marked.set_misspellings(&document, document.revision(), Vec::new());
            assert_eq!(render(&mut marked), without);
        }
    }

    #[test]
    fn moves_other_than_left_and_right_go_on_from_a_selection() {
        let style = ComposeEditorStyle {
            text: Color::from_rgb_u8(30, 30, 30),
            link: Color::from_rgb_u8(0, 90, 180),
            selection: Color::from_argb_u8(140, 60, 120, 220),
            selected_text: Color::from_rgb_u8(255, 255, 255),
            misspelling: Color::from_rgb_u8(183, 55, 49),
        };
        let mut document = RichComposeDocument::default();
        document.synchronize("abc def\nsecond line", 0, 0);
        let mut surface = CosmicComposeEditor::default();
        let mut press = |selection: ComposeSelection, key: &str| {
            surface
                .handle_navigation(
                    &document, selection, 520.0, 1.0, style, "", key, false, false, false, false,
                )
                .unwrap()
        };
        let abc = ComposeSelection { start: 0, end: 3 };
        let def = ComposeSelection { start: 4, end: 7 };
        // Right and Left collapse to the selection's end and start.
        assert_eq!(
            press(abc, "\u{f703}"),
            ComposeSelection { start: 3, end: 3 }
        );
        assert_eq!(
            press(def, "\u{f702}"),
            ComposeSelection { start: 4, end: 4 }
        );
        // End and Home reach the ends of the line; Down the next line.
        assert_eq!(
            press(abc, "\u{f72b}"),
            ComposeSelection { start: 7, end: 7 }
        );
        assert_eq!(
            press(def, "\u{f729}"),
            ComposeSelection { start: 0, end: 0 }
        );
        let down = press(abc, "\u{f701}");
        assert!(down.start == down.end && down.start > 8, "{down:?}");
    }

    #[test]
    fn selection_repaints_nearby_tiles_and_matches_full_render() {
        let text = (0..160)
            .map(|line| format!("Line {line} with some draft text\n"))
            .collect::<String>();
        let start = text.find("Line 95").unwrap() as i32;
        let end = start + "Line 95 with".len() as i32;
        let style = ComposeEditorStyle {
            text: Color::from_rgb_u8(30, 30, 30),
            link: Color::from_rgb_u8(0, 90, 180),
            selection: Color::from_argb_u8(140, 60, 120, 220),
            selected_text: Color::from_rgb_u8(255, 255, 255),
            misspelling: Color::from_rgb_u8(183, 55, 49),
        };
        for scale in [1.0, 1.5, 2.0] {
            let mut document = RichComposeDocument::default();
            document.synchronize(&text, 0, 0);
            let mut surface = CosmicComposeEditor::default();
            let initial = surface.render(
                &document,
                document.selection(),
                520.0,
                900.0,
                1600.0,
                scale,
                style,
                "",
            );
            let selection = document.update_selection(start, end);
            let updated =
                surface.render(&document, selection, 520.0, 900.0, 1600.0, scale, style, "");
            let mut fresh = CosmicComposeEditor::default();
            let expected =
                fresh.render(&document, selection, 520.0, 900.0, 1600.0, scale, style, "");
            assert_eq!(initial.tiles.len(), updated.tiles.len());
            assert_eq!(updated.tiles.len(), expected.tiles.len());
            let mut preserved = 0;
            let mut repainted = 0;
            for ((before, after), full) in initial
                .tiles
                .iter()
                .zip(&updated.tiles)
                .zip(&expected.tiles)
            {
                assert_eq!(after.y, full.y);
                let before_buffer = before.image.to_rgba8_premultiplied().unwrap();
                let after_buffer = after.image.to_rgba8_premultiplied().unwrap();
                let full_buffer = full.image.to_rgba8_premultiplied().unwrap();
                assert_eq!(
                    after_buffer.as_bytes(),
                    full_buffer.as_bytes(),
                    "scale={scale}, tile_y={}",
                    after.y
                );
                if std::ptr::eq(before_buffer.as_bytes(), after_buffer.as_bytes()) {
                    preserved += 1;
                } else {
                    repainted += 1;
                }
            }
            assert!(
                preserved > 0,
                "scale={scale}: unrelated tiles were repainted"
            );
            assert!(
                repainted > 0,
                "scale={scale}: selection tiles were not repainted"
            );

            // The navigation handler changes Cosmic Text before the document
            // selection is published back to the surface.
            let next = surface
                .handle_navigation(
                    &document, selection, 520.0, scale, style, "", "\u{f702}", false, false, false,
                    false,
                )
                .unwrap();
            let collapsed = document.update_selection(next.start, next.end);
            let after_collapse =
                surface.render(&document, collapsed, 520.0, 900.0, 1600.0, scale, style, "");
            let mut fresh = CosmicComposeEditor::default();
            let full_collapse =
                fresh.render(&document, collapsed, 520.0, 900.0, 1600.0, scale, style, "");
            let mut preserved_after_collapse = 0;
            for (after, full) in after_collapse.tiles.iter().zip(&full_collapse.tiles) {
                assert_eq!(after.y, full.y);
                assert_eq!(
                    after.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                    full.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                    "scale={scale}, collapsed tile_y={}",
                    after.y
                );
            }
            for (before, after) in updated.tiles.iter().zip(&after_collapse.tiles) {
                let before = before.image.to_rgba8_premultiplied().unwrap();
                let after = after.image.to_rgba8_premultiplied().unwrap();
                if std::ptr::eq(before.as_bytes(), after.as_bytes()) {
                    preserved_after_collapse += 1;
                }
            }
            assert!(
                preserved_after_collapse > 0,
                "scale={scale}: collapse cleared all tiles"
            );

            let far_end = text.find("Line 120").unwrap() as i32 + "Line 120 with".len() as i32;
            let reverse_multiline = document.update_selection(far_end, start);
            let after_drag = surface.render(
                &document,
                reverse_multiline,
                520.0,
                900.0,
                1600.0,
                scale,
                style,
                "",
            );
            let mut fresh = CosmicComposeEditor::default();
            let full_drag = fresh.render(
                &document,
                reverse_multiline,
                520.0,
                900.0,
                1600.0,
                scale,
                style,
                "",
            );
            for (after, full) in after_drag.tiles.iter().zip(&full_drag.tiles) {
                assert_eq!(after.y, full.y);
                assert_eq!(
                    after.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                    full.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                    "scale={scale}, drag tile_y={}",
                    after.y
                );
            }
        }
    }

    #[test]
    fn wrapped_selection_tiles_match_full_render() {
        let text = "wideword ".repeat(450);
        let style = ComposeEditorStyle {
            text: Color::from_rgb_u8(30, 30, 30),
            link: Color::from_rgb_u8(0, 90, 180),
            selection: Color::from_argb_u8(140, 60, 120, 220),
            selected_text: Color::from_rgb_u8(255, 255, 255),
            misspelling: Color::from_rgb_u8(183, 55, 49),
        };
        for scale in [1.0, 1.5, 2.0] {
            let mut document = RichComposeDocument::default();
            document.synchronize(&text, 0, 0);
            let mut surface = CosmicComposeEditor::default();
            let initial = surface.render(
                &document,
                document.selection(),
                280.0,
                600.0,
                350.0,
                scale,
                style,
                "",
            );
            let selection = document.update_selection(1300, 1330);
            let updated =
                surface.render(&document, selection, 280.0, 600.0, 350.0, scale, style, "");
            let mut fresh = CosmicComposeEditor::default();
            let expected =
                fresh.render(&document, selection, 280.0, 600.0, 350.0, scale, style, "");
            assert_eq!(updated.tiles.len(), expected.tiles.len());
            let mut preserved = 0;
            let mut repainted = 0;
            for ((before, after), full) in initial
                .tiles
                .iter()
                .zip(&updated.tiles)
                .zip(&expected.tiles)
            {
                if std::ptr::eq(
                    before.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                    after.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                ) {
                    preserved += 1;
                } else {
                    repainted += 1;
                }
                assert_eq!(after.y, full.y);
                assert_eq!(
                    after.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                    full.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                    "scale={scale}, wrapped tile_y={}",
                    after.y
                );
            }
            assert!(
                preserved > 0,
                "scale={scale}: wrapping repainted every tile"
            );
            assert!(repainted > 0, "scale={scale}: selection failed to repaint");
            println!("wrapped selection scale={scale} preserved={preserved} repainted={repainted}");
        }
    }

    #[test]
    fn mixed_direction_selection_and_preedit_tiles_match_full_render() {
        let text = (0..50)
            .map(|line| format!("Row {line}: café e\u{301} العربية 👩‍💻 text that wraps\n"))
            .collect::<String>();
        let start = text.find("Row 6:").unwrap() as i32;
        let end = text.find("Row 12:").unwrap() as i32;
        let style = ComposeEditorStyle {
            text: Color::from_rgb_u8(30, 30, 30),
            link: Color::from_rgb_u8(0, 90, 180),
            selection: Color::from_argb_u8(140, 60, 120, 220),
            selected_text: Color::from_rgb_u8(255, 255, 255),
            misspelling: Color::from_rgb_u8(183, 55, 49),
        };
        for scale in [1.0, 1.5, 2.0] {
            let mut document = RichComposeDocument::default();
            document.synchronize(&text, 0, 0);
            let mut surface = CosmicComposeEditor::default();
            surface.render(
                &document,
                document.selection(),
                280.0,
                700.0,
                128.0,
                scale,
                style,
                "",
            );
            for (anchor, cursor, preedit) in [
                (start, end, ""),
                (end, start, ""),
                (start, start, ""),
                (start, start, "世界"),
                (start, end, ""),
                (end, end, ""),
            ] {
                let selection = document.update_selection(anchor, cursor);
                let actual = surface.render(
                    &document, selection, 280.0, 700.0, 128.0, scale, style, preedit,
                );
                let expected = CosmicComposeEditor::default().render(
                    &document, selection, 280.0, 700.0, 128.0, scale, style, preedit,
                );
                assert_eq!(actual.tiles.len(), expected.tiles.len());
                for (actual, expected) in actual.tiles.iter().zip(&expected.tiles) {
                    assert_eq!(actual.y, expected.y);
                    assert_eq!(
                        actual.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                        expected.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                        "scale={scale} anchor={anchor} cursor={cursor} preedit={preedit:?} tile_y={}",
                        actual.y,
                    );
                }
            }
        }
    }

    /// The look of a light theme at scale 1.
    fn test_look() -> TextLook {
        TextLook {
            dark: false,
            metrics: Metrics::new(FONT_SIZE_LOGICAL, LINE_HEIGHT_LOGICAL),
            link: CosmicColor::rgb(0, 90, 180),
            width: 600.0,
        }
    }

    #[test]
    fn layout_spans_borrow_long_unicode_text_and_preedit() {
        let mut document = RichComposeDocument::default();
        document.synchronize(&"é".repeat(512 * 1024), 2, 2);
        let preedit = String::from("世界");
        let attrs = Attrs::new().family(Family::Name(UI_FONT_FAMILY));
        let spans = rich_spans(&document, 2, &preedit, &attrs, test_look(), &mut Vec::new());

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].0, "é");
        assert_eq!(spans[0].0.as_ptr(), document.text().as_ptr());
        assert_eq!(spans[1].0, "世界");
        assert_eq!(spans[1].0.as_ptr(), preedit.as_ptr());
        assert_eq!(spans[1].1.text_decoration.underline, UnderlineStyle::Single);
        assert_eq!(spans[2].0.as_ptr(), document.text()[2..].as_ptr());
        assert_eq!(spans[0].0.len() + spans[2].0.len(), 1024 * 1024);
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
                misspelling: Color::from_rgb_u8(183, 55, 49),
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
            test_look(),
            &mut Vec::new(),
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
                misspelling: Color::from_rgb_u8(183, 55, 49),
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
            misspelling: Color::from_rgb_u8(183, 55, 49),
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
                misspelling: Color::from_rgb_u8(183, 55, 49),
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

    #[test]
    fn rich_lines_match_cosmic_texts_own_paragraphs() {
        let mut font_system = FontSystem::new();
        let base = Attrs::new().family(Family::Name(UI_FONT_FAMILY));
        let bold = base.clone().weight(Weight::BOLD);
        let link = base.clone().color(CosmicColor::rgb(0, 90, 180));
        let cases: Vec<Vec<(&str, Attrs<'static>)>> = vec![
            vec![],
            vec![("plain", base.clone())],
            vec![
                ("first\n\nthird\n", base.clone()),
                (TRAILING_LINE_SENTINEL, base.clone()),
            ],
            vec![
                ("Hello ", base.clone()),
                ("bold\nnext", bold.clone()),
                (" line", base.clone()),
            ],
            vec![
                ("שלום ", base.clone()),
                ("مرحبا\n", link.clone()),
                ("世界 👩‍💻", bold.clone()),
            ],
            vec![("a\r\nb\u{2029}c", base.clone())],
        ];
        for spans in cases {
            let mut expected = Buffer::new(&mut font_system, Metrics::new(13.0, 18.0));
            expected.set_rich_text(
                spans.iter().map(|(text, attrs)| (*text, attrs.clone())),
                &base,
                Shaping::Advanced,
                None,
            );
            let lines = rich_lines(&spans, &base, |_| None);
            assert_eq!(lines.len(), expected.lines.len(), "{spans:?}");
            for ((text, attrs, align), line) in lines.iter().zip(&expected.lines) {
                assert_eq!(text, line.text());
                assert_eq!(attrs, line.attrs_list());
                assert_eq!(*align, line.align());
            }
        }
    }

    #[test]
    fn typing_reshapes_only_the_edited_paragraph() {
        let mut font_system = FontSystem::new();
        let attrs = Attrs::new().family(Family::Name(UI_FONT_FAMILY));
        let mut buffer = Buffer::new(&mut font_system, Metrics::new(13.0, 18.0));
        buffer.set_size(Some(600.0), None);
        let text = (0..200)
            .map(|line| format!("> quoted line {line}\n"))
            .collect::<String>();
        let mut apply = |buffer: &mut Buffer, text: &str| {
            let replaced = replace_changed_lines(
                buffer,
                rich_lines(&[(text, attrs.clone())], &attrs, |_| None),
            );
            let reshaped = buffer
                .lines
                .iter()
                .filter(|line| line.shape_opt().is_none())
                .count();
            for line in replaced.clone() {
                buffer.line_layout(&mut font_system, line);
            }
            buffer.shape_until_scroll(&mut font_system, false);
            assert!(buffer.lines.iter().all(|line| line.shape_opt().is_some()));
            (replaced, reshaped)
        };

        let (_, reshaped) = apply(&mut buffer, &text);
        assert_eq!(reshaped, 200);
        let typed = text.replacen("quoted line 100", "quoted line 100!", 1);
        assert_eq!(apply(&mut buffer, &typed), (100..101, 1));
        let split = typed.replacen("quoted line 50", "quoted\nline 50", 1);
        assert_eq!(apply(&mut buffer, &split), (50..52, 2));
        assert_eq!(buffer.lines.len(), 201);
        assert_eq!(apply(&mut buffer, &typed), (50..51, 1));
        assert_eq!(apply(&mut buffer, &typed), (200..200, 0));
        assert_eq!(buffer.lines.len(), 200);
    }

    #[test]
    fn edits_render_like_a_fresh_layout() {
        let style = ComposeEditorStyle {
            text: Color::from_rgb_u8(30, 30, 30),
            link: Color::from_rgb_u8(0, 90, 180),
            selection: Color::from_argb_u8(140, 60, 120, 220),
            selected_text: Color::from_rgb_u8(255, 255, 255),
            misspelling: Color::from_rgb_u8(183, 55, 49),
        };
        let quote = (0..40)
            .map(|line| format!("> Row {line}: café العربية 👩‍💻 quoted text that wraps\n"))
            .collect::<String>();
        let mut document = RichComposeDocument::default();
        document.load(&format!("\n\n{quote}"), 0, 0);
        let mut surface = CosmicComposeEditor::default();
        let mut text = document.text().to_owned();
        let edits: [&dyn Fn(&str) -> String; 6] = [
            &|text| format!("Thanks{text}"),
            &|text| text.replacen("Thanks", "Thanks\nfor this", 1),
            &|text| text.replacen("Thanks\nfor", "Thanks for", 1),
            &|text| text.replacen("Row 3", "Row three", 1),
            &|text| text.replacen("> Row 7", "", 1),
            &|text| text.replacen("Row 9:", "Row 9:\npasted\nlines", 1),
        ];
        for (step, edit) in edits.into_iter().enumerate() {
            text = edit(&text);
            let end = text.len() as i32;
            document.synchronize(&text, end, end);
            if step == 3 {
                document.format("bold", &text, 0, 10);
            }
            for scale in [1.0, 2.0] {
                let actual = surface.render(
                    &document,
                    document.selection(),
                    320.0,
                    600.0,
                    0.0,
                    scale,
                    style,
                    "",
                );
                let expected = CosmicComposeEditor::default().render(
                    &document,
                    document.selection(),
                    320.0,
                    600.0,
                    0.0,
                    scale,
                    style,
                    "",
                );
                assert_eq!(
                    actual.content_height, expected.content_height,
                    "step {step}"
                );
                assert_eq!(
                    (actual.caret_x, actual.caret_y, actual.caret_height),
                    (expected.caret_x, expected.caret_y, expected.caret_height),
                    "step {step}"
                );
                assert_eq!(actual.tiles.len(), expected.tiles.len(), "step {step}");
                for (actual, expected) in actual.tiles.iter().zip(&expected.tiles) {
                    assert_eq!(
                        actual.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                        expected.image.to_rgba8_premultiplied().unwrap().as_bytes(),
                        "step {step} scale {scale} tile {}",
                        actual.y
                    );
                }
            }
        }
    }

    fn spelling_style() -> ComposeEditorStyle {
        ComposeEditorStyle {
            text: Color::from_rgb_u8(30, 30, 30),
            link: Color::from_rgb_u8(0, 90, 180),
            selection: Color::from_argb_u8(140, 60, 120, 220),
            selected_text: Color::from_rgb_u8(255, 255, 255),
            misspelling: Color::from_rgb_u8(255, 0, 0),
        }
    }

    fn underlined(editor: &mut CosmicComposeEditor, document: &RichComposeDocument) -> Vec<String> {
        editor.render(
            document,
            document.selection(),
            320.0,
            120.0,
            0.0,
            1.0,
            spelling_style(),
            "",
        );
        editor
            .squiggles
            .iter()
            .map(|squiggle| document.text()[squiggle.start..squiggle.end].to_owned())
            .collect()
    }

    #[test]
    fn underlines_follow_edits_and_leave_words_edits_change() {
        let mut document = RichComposeDocument::default();
        document.load("Teh cat sat on teh mat", 22, 22);
        let mut editor = CosmicComposeEditor::default();
        let found_at = document.revision();
        editor.set_misspellings(&document, found_at, vec![0..3, 15..18]);
        assert_eq!(underlined(&mut editor, &document), ["Teh", "teh"]);

        // Typing at the start moves both lines along.
        document.synchronize_key_edit("So Teh cat sat on teh mat", 3, 3, "");
        assert_eq!(underlined(&mut editor, &document), ["Teh", "teh"]);
        assert_eq!(editor.squiggles[0].start, 3);

        // Correcting a word takes its line away; the other stays.
        document.synchronize_key_edit("So Teh cat sat on the mat", 21, 21, "");
        assert_eq!(underlined(&mut editor, &document), ["Teh"]);

        // A letter typed onto the word makes it another word.
        document.synchronize_key_edit("So Tehx cat sat on the mat", 7, 7, "x");
        assert!(underlined(&mut editor, &document).is_empty());

        // Undo brings the text back, but not the line: it waits for the next check.
        document.history("undo");
        assert!(underlined(&mut editor, &document).is_empty());

        // A check that started before these edits still lands on the right word.
        editor.set_misspellings(&document, found_at, vec![0..3, 15..18]);
        assert_eq!(underlined(&mut editor, &document), ["Teh"]);
    }

    #[test]
    fn the_word_being_typed_is_underlined_once_the_caret_leaves_it() {
        let mut document = RichComposeDocument::default();
        document.load("Thanks, recieve", 15, 15);
        let mut editor = CosmicComposeEditor::default();
        let recieve = 8..15;
        editor.set_misspellings(&document, document.revision(), vec![recieve.clone()]);
        assert!(underlined(&mut editor, &document).is_empty());

        // Typing a space finishes the word.
        document.synchronize_key_edit("Thanks, recieve ", 16, 16, " ");
        assert_eq!(underlined(&mut editor, &document), ["recieve"]);

        // A word already underlined stays so when a later check finds it again with the caret
        // in it.
        document.update_selection(10, 10);
        editor.set_misspellings(&document, document.revision(), vec![recieve]);
        assert_eq!(underlined(&mut editor, &document), ["recieve"]);
    }

    #[test]
    fn the_underlined_word_under_the_pointer_is_found_and_accepted_words_go() {
        let mut document = RichComposeDocument::default();
        document.load("Teh cat sat on teh mat", 22, 22);
        let mut editor = CosmicComposeEditor::default();
        editor.set_misspellings(&document, document.revision(), vec![0..3, 15..18]);
        underlined(&mut editor, &document);
        let first = editor.squiggles[0];
        let middle_y = (first.top + first.bottom) / 2.0;
        assert_eq!(
            editor.misspelling_at((first.left + first.right) / 2.0, middle_y, 1.0),
            Some(0..3)
        );
        assert_eq!(
            editor.misspelling_at(first.right + 12.0, middle_y, 1.0),
            None
        );
        assert_eq!(
            editor.misspelling_at(first.left + 1.0, first.bottom + 30.0, 1.0),
            None
        );
        // A tapped word's menu opens below its line, in logical pixels at any scale.
        let middle_x = (first.left + first.right) / 2.0;
        assert_eq!(
            editor.misspelling_line_bottom(middle_x, middle_y, 1.0),
            Some(first.bottom)
        );
        assert_eq!(
            editor.misspelling_line_bottom(middle_x / 2.0, middle_y / 2.0, 2.0),
            Some(first.bottom / 2.0)
        );

        editor.accept_word(&document, "teh");
        assert_eq!(underlined(&mut editor, &document), ["Teh"]);
    }

    #[test]
    fn whole_words_are_told_from_parts_of_words() {
        let text = "Teh. teh.s teh's x-teh";
        assert!(is_whole_word(text, &(0..3)));
        assert!(!is_whole_word(text, &(5..8)));
        assert!(!is_whole_word(text, &(11..14)));
        assert!(is_whole_word(text, &(19..22)));
        assert!(!is_whole_word(text, &(0..2)));
        assert!(!is_whole_word(text, &(20..30)));
        let accented = "très bien";
        assert!(is_whole_word(accented, &(0..5)));
        assert!(!is_whole_word(accented, &(0..3)));
    }

    /// Lays `document` out `width` logical pixels wide at `scale` and returns the first tile's
    /// pixels, one row after another, `width * scale` pixels to a row.
    fn painted(
        editor: &mut CosmicComposeEditor,
        document: &RichComposeDocument,
        width: f32,
        scale: f32,
        style: ComposeEditorStyle,
    ) -> Vec<[u8; 4]> {
        let rendered = editor.render(
            document,
            document.selection(),
            width,
            200.0,
            0.0,
            scale,
            style,
            "",
        );
        rendered.tiles[0]
            .image
            .to_rgba8_premultiplied()
            .unwrap()
            .as_bytes()
            .as_chunks::<4>()
            .0
            .to_vec()
    }

    /// Where each visual line's glyphs start and end.
    fn line_extents(editor: &CosmicComposeEditor) -> Vec<(f32, f32)> {
        editor.editor.with_buffer(|buffer| {
            buffer
                .layout_runs()
                .map(|run| {
                    let left = run.glyphs.first().map_or(f32::MAX, |glyph| glyph.x);
                    let right = run
                        .glyphs
                        .last()
                        .map_or(f32::MIN, |glyph| glyph.x + glyph.w);
                    (left, right)
                })
                .collect()
        })
    }

    #[test]
    fn highlights_paint_behind_their_words_only() {
        let mut document = RichComposeDocument::default();
        document.load("plain marked plain", 0, 0);
        let text = document.text().to_owned();
        let marked = text.find("marked").unwrap();
        let marked_end = marked + "marked".len();
        document.format("highlight:#ffeb3b", &text, marked as i32, marked_end as i32);
        document.update_selection(0, 0);
        // On a dark theme the text on a light highlight turns dark to stay readable.
        let dark = ComposeEditorStyle {
            text: Color::from_rgb_u8(230, 230, 230),
            ..spelling_style()
        };
        for (style, scale) in [(spelling_style(), 1.0), (dark, 2.0)] {
            let mut editor = CosmicComposeEditor::default();
            let pixels = painted(&mut editor, &document, 320.0, scale, style);
            let width = (320.0 * scale) as usize;
            let (left, right) = (editor.glyph_left(marked), editor.glyph_left(marked_end));
            let highlighted: Vec<usize> = pixels
                .iter()
                .enumerate()
                .filter(|(_, pixel)| **pixel == [0xff, 0xeb, 0x3b, 0xff])
                .map(|(index, _)| index % width)
                .collect();
            assert!(!highlighted.is_empty());
            for x in highlighted {
                assert!(
                    x as f32 >= left.floor() && x as f32 <= right.ceil(),
                    "highlight at {x} outside {left}..{right}"
                );
            }
            assert!(
                pixels.contains(&[0x20, 0x21, 0x24, 0xff]),
                "no dark text on the highlight"
            );
        }
    }

    #[test]
    fn paragraphs_line_up_as_the_document_says() {
        let mut document = RichComposeDocument::default();
        document.load("left\ncenter\nright\n", 0, 0);
        let mut editor = CosmicComposeEditor::default();
        painted(&mut editor, &document, 320.0, 1.0, spelling_style());
        let text = document.text().to_owned();
        let center = text.find("center").unwrap() as i32;
        let right = text.find("right").unwrap() as i32;
        document.format("align:center", &text, center, center);
        document.format("align:right", &text, right, right);
        let end = text.len() as i32;
        document.update_selection(end, end);
        let pixels = painted(&mut editor, &document, 320.0, 1.0, spelling_style());

        let lines = line_extents(&editor);
        assert!(lines[0].0 < 1.0, "{lines:?}");
        assert!(
            (lines[1].0 - (320.0 - lines[1].1)).abs() <= 1.0,
            "{lines:?}"
        );
        assert!(lines[2].1 > 319.0, "{lines:?}");
        // The empty line after them goes on as the paragraph above it.
        let rendered = editor.render(
            &document,
            document.selection(),
            320.0,
            200.0,
            0.0,
            1.0,
            spelling_style(),
            "",
        );
        assert!(rendered.caret_x > 310.0, "caret at {}", rendered.caret_x);

        // Lines kept from the layout before match a fresh one.
        assert_eq!(
            pixels,
            painted(
                &mut CosmicComposeEditor::default(),
                &document,
                320.0,
                1.0,
                spelling_style()
            )
        );

        // Centering the empty line moves the caret there, and leaving it takes it back to the
        // alignment typing there would have.
        document.format("align:center", &text, end, end);
        let caret_x = |editor: &mut CosmicComposeEditor, document: &RichComposeDocument| {
            editor
                .render(
                    document,
                    document.selection(),
                    320.0,
                    200.0,
                    0.0,
                    1.0,
                    spelling_style(),
                    "",
                )
                .caret_x
        };
        assert!((caret_x(&mut editor, &document) - 160.0).abs() <= 1.0);
        document.update_selection(0, 0);
        caret_x(&mut editor, &document);
        document.update_selection(end, end);
        assert!(caret_x(&mut editor, &document) > 310.0);
    }

    #[test]
    fn the_caret_on_an_empty_line_sits_where_typing_goes() {
        let caret_x = |document: &RichComposeDocument| {
            CosmicComposeEditor::default()
                .render(
                    document,
                    document.selection(),
                    320.0,
                    200.0,
                    0.0,
                    1.0,
                    spelling_style(),
                    "",
                )
                .caret_x
        };
        let mut document = RichComposeDocument::default();
        document.format("align:right", "", 0, 0);
        assert_eq!(caret_x(&document), 320.0);

        let mut document = RichComposeDocument::default();
        document.load("first\n\nlast", 0, 0);
        let text = document.text().to_owned();
        document.format("align:center", &text, 0, text.len() as i32);
        document.update_selection(6, 6);
        assert_eq!(caret_x(&document), 160.0);
    }

    #[test]
    fn bigger_text_makes_its_line_taller() {
        let mut document = RichComposeDocument::default();
        document.load("normal\nhuge", 0, 0);
        let text = document.text().to_owned();
        let huge = text.find("huge").unwrap() as i32;
        let mut editor = CosmicComposeEditor::default();
        let render = |editor: &mut CosmicComposeEditor, document: &RichComposeDocument| {
            editor.render(
                document,
                document.selection(),
                320.0,
                200.0,
                0.0,
                1.0,
                spelling_style(),
                "",
            )
        };
        let before = render(&mut editor, &document).content_height;
        document.format("size:huge", &text, huge, text.len() as i32);
        let end = text.len() as i32;
        document.update_selection(end, end);
        let after = render(&mut editor, &document);
        assert_eq!(after.content_height, before + LINE_HEIGHT_LOGICAL);
        assert_eq!(after.caret_height, LINE_HEIGHT_LOGICAL * 2.0);
        assert_eq!(after.caret_y, LINE_HEIGHT_LOGICAL);
    }

    /// The style a character gets from the toolbar's `commands`.
    fn style_from(commands: &[&str]) -> CharacterStyle {
        let mut document = RichComposeDocument::default();
        document.load("x", 0, 0);
        for command in commands {
            document.format(command, "x", 0, 1);
        }
        document.style_runs()[0].style.clone()
    }

    #[test]
    fn character_styles_become_text_attributes() {
        let base = Attrs::new().family(Family::Name(UI_FONT_FAMILY));
        let look = test_look();
        let styled = style_from(&["color:#1a73e8", "size:small", "family:serif"]);
        let attrs = character_attrs(&base, &styled, look);
        assert_eq!(attrs.color_opt, Some(CosmicColor::rgb(0x1a, 0x73, 0xe8)));
        assert_eq!(attrs.family, Family::Serif);
        assert_eq!(
            attrs.metrics_opt.map(Metrics::from),
            Some(Metrics::new(
                FONT_SIZE_LOGICAL * 13.0 / 16.0,
                LINE_HEIGHT_LOGICAL * 13.0 / 16.0
            ))
        );
        assert_eq!(attrs.metadata, 0);

        // Normal text keeps the editor's own size, family and color.
        let plain = character_attrs(&base, &CharacterStyle::default(), look);
        assert_eq!(plain, base);

        // Code stays monospace whatever the family.
        let code = style_from(&["family:serif", "code"]);
        assert_eq!(
            character_attrs(&base, &code, look).family,
            Family::Monospace
        );

        // A highlight marks the glyphs with its color, and the text on it reads either way.
        let highlighted = style_from(&["highlight:#0b5394"]);
        let attrs = character_attrs(&base, &highlighted, look);
        assert_eq!(attrs.metadata, HIGHLIGHTED | 0x0b_53_94);
        assert_eq!(attrs.color_opt, Some(CosmicColor::rgb(0xff, 0xff, 0xff)));
    }

    #[test]
    fn serif_and_monospace_text_use_installed_fonts() {
        let editor = CosmicComposeEditor::default();
        let db = editor.font_system.db();
        for family in [Family::Serif, Family::Monospace] {
            let query = cosmic_text::fontdb::Query {
                families: &[family],
                ..Default::default()
            };
            assert!(
                db.query(&query).is_some(),
                "no font for {}",
                db.family_name(&family)
            );
        }
    }

    /// Where the layout's image is: its left and its width, in physical pixels.
    fn image_box(editor: &CosmicComposeEditor) -> (f32, f32) {
        editor.editor.with_buffer(|buffer| {
            buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter())
                .find(|glyph| glyph.metadata & IMAGE != 0)
                .map(|glyph| (glyph.x, glyph.w))
                .expect("an image in the layout")
        })
    }

    /// A PNG `width` by `height` in pure red.
    fn red_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([255, 0, 0, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        bytes
    }

    #[test]
    fn images_take_their_room_in_the_text_and_show_there() {
        let mut document = RichComposeDocument::default();
        document.load("ab", 1, 1);
        document.insert_image(InlineImage::from_file_bytes(red_png(40, 30), "").unwrap());
        assert_eq!(document.text(), "a\u{fffc}b");
        let mut editor = CosmicComposeEditor::default();
        let pixels = painted(&mut editor, &document, 320.0, 2.0, spelling_style());
        let width = 640;
        // The image's room: 40 by 30 points, at twice the pixels.
        let (left, image_width) = image_box(&editor);
        assert_eq!(image_width, 80.0);
        assert_eq!(editor.glyph_left(4), left + image_width);
        let rendered = editor.render(
            &document,
            document.selection(),
            320.0,
            200.0,
            0.0,
            2.0,
            spelling_style(),
            "",
        );
        assert_eq!(rendered.content_height, 30.0);
        let red: Vec<(usize, usize)> = pixels
            .iter()
            .enumerate()
            .filter(|(_, pixel)| **pixel == [255, 0, 0, 255])
            .map(|(index, _)| (index % width, index / width))
            .collect();
        assert_eq!(red.len(), 80 * 60);
        let columns = left.floor() as usize..=(left + image_width).ceil() as usize;
        assert!(red.iter().all(|(x, y)| columns.contains(x) && *y < 60));

        // A click on it selects it.
        let selection = editor
            .handle_pointer(
                &document,
                document.selection(),
                320.0,
                2.0,
                spelling_style(),
                "",
                (left / 2.0) + 10.0,
                10.0,
                "down",
                false,
            )
            .unwrap();
        assert_eq!(selection, ComposeSelection { start: 1, end: 4 });
    }

    #[test]
    fn images_wider_than_the_text_shrink_to_fit() {
        let mut document = RichComposeDocument::default();
        document.load("", 0, 0);
        document.insert_image(InlineImage::from_file_bytes(red_png(800, 400), "").unwrap());
        let mut editor = CosmicComposeEditor::default();
        let rendered = editor.render(
            &document,
            document.selection(),
            320.0,
            400.0,
            0.0,
            1.0,
            spelling_style(),
            "",
        );
        assert_eq!(rendered.content_height, 160.0);
        assert_eq!(image_box(&editor).1, 320.0);
    }

    #[test]
    fn photos_show_read_at_their_size_and_keep_only_that_size() {
        // A photo far larger than it shows: red above, blue below.
        let picture = image::RgbImage::from_fn(1600, 1200, |_, y| {
            image::Rgb(if y < 600 { [255, 0, 0] } else { [0, 0, 255] })
        });
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(picture)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let mut document = RichComposeDocument::default();
        document.load("", 0, 0);
        document.insert_image(InlineImage::from_file_bytes(jpeg, "").unwrap());
        let mut editor = CosmicComposeEditor::default();
        let pixels = painted(&mut editor, &document, 200.0, 1.0, spelling_style());
        let (left, width) = image_box(&editor);
        assert_eq!(width, 200.0);
        let middle = (left + width / 2.0) as usize;
        let at = |y: usize| pixels[y * 200 + middle];
        assert!(at(20)[0] > 200 && at(20)[2] < 60, "{:?}", at(20));
        assert!(at(130)[2] > 200 && at(130)[0] < 60, "{:?}", at(130));
        // As the window narrows, only the size it shows at now is kept.
        painted(&mut editor, &document, 150.0, 1.0, spelling_style());
        painted(&mut editor, &document, 120.0, 1.0, spelling_style());
        let sizes: Vec<_> = editor
            .image_pixels
            .keys()
            .map(|(_, w, h)| (*w, *h))
            .collect();
        assert_eq!(sizes, [(120, 90)]);
    }

    #[test]
    fn dark_colors_lighten_on_a_dark_theme_only() {
        let navy = 0x0b_1f_4d;
        assert_eq!(shown_color(navy, false), navy);
        let shown = shown_color(navy, true);
        assert!(rgb_luminance(shown) > rgb_luminance(navy));
        // Colors that already read on a dark background stay as chosen.
        assert_eq!(shown_color(0xff_eb_3b, true), 0xff_eb_3b);
        assert_eq!(text_on(0xff_eb_3b), 0x20_21_24);
        assert_eq!(text_on(0x0b_53_94), 0xff_ff_ff);
    }
}
