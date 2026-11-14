use html5ever::tokenizer::{BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer};
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashSet, VecDeque},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub(crate) const BOLD: u8 = 1 << 0;
pub(crate) const ITALIC: u8 = 1 << 1;
pub(crate) const UNDERLINE: u8 = 1 << 2;
pub(crate) const STRIKE: u8 = 1 << 3;
pub(crate) const CODE: u8 = 1 << 4;
/// What starts each line of a quote block in the document's text.
pub(crate) const QUOTE_MARKER: &str = "│ ";
/// Marks HTML the composer copied, which pastes back as it was.
const COPIED_MARK: &str = "<!--flectar-mail-composer-->";
/// The line a signature starts under, after a blank line, as mail has long marked them.
pub(crate) const SIGNATURE_SEPARATOR: &str = "\n\n-- \n";
/// Holds an image's place in the text, as in other rich text: its style carries the image.
pub(crate) const IMAGE_PLACEHOLDER: char = '\u{fffc}';
/// Images larger than this, in pixels across or down, aren't taken.
const MAX_IMAGE_SIDE: u32 = 16_384;
/// The narrowest an image can be made to show, in CSS pixels.
const MIN_IMAGE_WIDTH: u32 = 16;

/// An image in the text, as the file it goes out as.
#[derive(Clone, Debug)]
pub(crate) struct InlineImage {
    /// Tells this image from others, also when two hold the same file.
    pub(crate) id: u64,
    pub(crate) data: Arc<[u8]>,
    pub(crate) mime_type: &'static str,
    /// Its size in pixels.
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The width it's shown at in CSS pixels, when it isn't its own.
    pub(crate) shown_width: Option<u32>,
    /// What it shows, for readers who can't see it.
    pub(crate) alt: Arc<str>,
}

impl InlineImage {
    /// The image a PNG, JPEG, GIF or WebP file holds, or `None` for anything else.
    pub(crate) fn from_file_bytes(data: Vec<u8>, alt: &str) -> Option<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        use image::{ImageDecoder, metadata::Orientation};
        let (mime_type, width, height) = {
            let reader = image::ImageReader::new(std::io::Cursor::new(&data))
                .with_guessed_format()
                .ok()?;
            let mime_type = match reader.format()? {
                image::ImageFormat::Png => "image/png",
                image::ImageFormat::Jpeg => "image/jpeg",
                image::ImageFormat::Gif => "image/gif",
                image::ImageFormat::WebP => "image/webp",
                _ => return None,
            };
            let mut decoder = reader.into_decoder().ok()?;
            let (width, height) = decoder.dimensions();
            // A photo turned on its side by the camera shows upright, as readers show it.
            match decoder.orientation() {
                Ok(
                    Orientation::Rotate90
                    | Orientation::Rotate270
                    | Orientation::Rotate90FlipH
                    | Orientation::Rotate270FlipH,
                ) => (mime_type, height, width),
                _ => (mime_type, width, height),
            }
        };
        if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
            return None;
        }
        Some(Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            data: data.into(),
            mime_type,
            width,
            height,
            shown_width: None,
            alt: alt.into(),
        })
    }

    /// The image a `data:` URI holds, as the composer writes them.
    pub(crate) fn from_data_uri(uri: &str, alt: &str) -> Option<Self> {
        use base64::Engine;
        let (_, encoded) = uri.strip_prefix("data:")?.split_once(";base64,")?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()?;
        Self::from_file_bytes(data, alt)
    }

    /// Its size as shown, in CSS pixels.
    pub(crate) fn shown_size(&self) -> (u32, u32) {
        let width = self.shown_width.unwrap_or(self.width).max(1);
        let height =
            (u64::from(self.height) * u64::from(width) / u64::from(self.width.max(1))).max(1);
        (width, u32::try_from(height).unwrap_or(u32::MAX))
    }

    /// What plain text shows in its place.
    fn description(&self) -> String {
        if self.alt.trim().is_empty() {
            "[image]".to_owned()
        } else {
            format!("[image: {}]", self.alt.trim())
        }
    }
}

/// The sizes an image can show at. Each changes only how wide it shows: the file stays as it
/// is, so readers who zoom in still see all of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageSize {
    Small,
    Medium,
    Large,
    Original,
}

impl ImageSize {
    pub(crate) const ALL: [Self; 4] = [Self::Small, Self::Medium, Self::Large, Self::Original];

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|size| size.name() == name)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Original => "original",
        }
    }

    /// Its width in CSS pixels. A message's text is often about 600 pixels wide, so large
    /// fills it, medium takes half, and small a quarter.
    fn width(self) -> Option<u32> {
        match self {
            Self::Small => Some(160),
            Self::Medium => Some(320),
            Self::Large => Some(640),
            Self::Original => None,
        }
    }

    /// Whether an image `natural` pixels wide can show smaller at this size. Images never
    /// grow past their own size.
    pub(crate) fn fits(self, natural: u32) -> bool {
        self.width().is_none_or(|width| width < natural)
    }

    /// The size `image` shows at, when it's one of these.
    pub(crate) fn of(image: &InlineImage) -> Option<Self> {
        match image.shown_width {
            None => Some(Self::Original),
            Some(width) => Self::ALL
                .into_iter()
                .find(|size| size.width() == Some(width)),
        }
    }
}

impl PartialEq for InlineImage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.shown_width == other.shown_width && self.alt == other.alt
    }
}

impl Eq for InlineImage {}

/// `text` with its quote blocks written as plain-text mail quotes, in lines
/// that start with `>`, for the plain-text part of a message being sent.
pub(crate) fn with_plain_text_quotes(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            plain.push('\n');
        }
        match line.strip_prefix(QUOTE_MARKER) {
            Some("") => plain.push('>'),
            Some(rest) => {
                plain.push_str("> ");
                plain.push_str(rest);
            }
            None => plain.push_str(line),
        }
    }
    plain
}
const MAX_HISTORY: usize = 128;
/// Text edits kept for following positions through them, such as misspelled words found in
/// the text as it was when a check began.
const MAX_TEXT_CHANGES: usize = 64;
// The most recent edit remains undoable even when one transaction exceeds this limit.
const MAX_HISTORY_BYTES: usize = 32 * 1024 * 1024;
const MAX_TYPING_GROUP_BYTES: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CharacterStyle {
    pub(crate) marks: u8,
    pub(crate) link: Option<Arc<str>>,
    /// The link was made from a web address typed in the text, and follows it as it's edited.
    pub(crate) link_is_auto: bool,
    /// The text color, or none for the reader's own.
    pub(crate) color: Option<Rgb>,
    /// The color behind the text.
    pub(crate) highlight: Option<Rgb>,
    pub(crate) size: TextSize,
    pub(crate) family: FontFamily,
    /// The alignment of the paragraph the character is in, which all of its characters share:
    /// that of its first character after any list or quote marker.
    pub(crate) align: Alignment,
    /// The image an image placeholder shows.
    pub(crate) image: Option<Arc<InlineImage>>,
}

/// A color as `0xRRGGBB`.
pub(crate) type Rgb = u32;

/// The text sizes the composer offers. Mail goes out with CSS size keywords, as other
/// composers' `<font size>` steps do, so each reader scales them from their own normal size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TextSize {
    Small,
    #[default]
    Normal,
    Large,
    Huge,
}

impl TextSize {
    pub(crate) const ALL: [Self; 4] = [Self::Small, Self::Normal, Self::Large, Self::Huge];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Normal => "normal",
            Self::Large => "large",
            Self::Huge => "huge",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|size| size.name() == name)
    }

    /// The CSS keyword sent for it: 13, 24 and 32 pixels against the usual 16, as `<font
    /// size>` 2, 5 and 6 are.
    fn css(self) -> Option<&'static str> {
        match self {
            Self::Small => Some("small"),
            Self::Normal => None,
            Self::Large => Some("x-large"),
            Self::Huge => Some("xx-large"),
        }
    }

    fn from_css(value: &str) -> Option<Self> {
        [Self::Small, Self::Large, Self::Huge]
            .into_iter()
            .find(|size| size.css() == Some(value))
    }

    /// Its size against normal text, as the keywords it's sent with are.
    pub(crate) fn scale(self) -> f32 {
        match self {
            Self::Small => 13.0 / 16.0,
            Self::Normal => 1.0,
            Self::Large => 1.5,
            Self::Huge => 2.0,
        }
    }
}

/// The font families the composer offers: none, which leaves each reader their own font as
/// plain text does, or a serif or monospace one, sent with the font lists Thunderbird uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum FontFamily {
    #[default]
    Default,
    Serif,
    Monospace,
}

impl FontFamily {
    pub(crate) const ALL: [Self; 3] = [Self::Default, Self::Serif, Self::Monospace];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Serif => "serif",
            Self::Monospace => "monospace",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|family| family.name() == name)
    }

    fn css(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Serif => Some("Times New Roman, Times, serif"),
            Self::Monospace => Some("Courier New, Courier, monospace"),
        }
    }

    fn from_css(value: &str) -> Option<Self> {
        [Self::Serif, Self::Monospace]
            .into_iter()
            .find(|family| family.css() == Some(value))
    }
}

/// How a paragraph lines up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Alignment {
    #[default]
    Left,
    Center,
    Right,
}

impl Alignment {
    pub(crate) const ALL: [Self; 3] = [Self::Left, Self::Center, Self::Right];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|align| align.name() == name)
    }
}

/// `#rrggbb` as a color.
fn parse_rgb(value: &str) -> Option<Rgb> {
    let hex = value.strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| Rgb::from_str_radix(hex, 16).ok())
        .flatten()
}

/// A color as CSS writes it in mail: `#rrggbb`, which every reader takes.
pub(crate) fn css_rgb(color: Rgb) -> String {
    format!("#{color:06x}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ComposeStyleRun {
    pub(crate) range: Range<usize>,
    pub(crate) style: CharacterStyle,
}

#[derive(Clone, Debug)]
struct Snapshot {
    text: String,
    styles: Vec<ComposeStyleRun>,
    typing_style: CharacterStyle,
    typing_override: Option<usize>,
    selection: (i32, i32),
}

#[derive(Clone, Debug)]
struct EditFragment {
    text: Arc<str>,
    span_len: usize,
    styles: Vec<ComposeStyleRun>,
}

struct StyleEditBefore {
    fragment: EditFragment,
    typing_style: CharacterStyle,
    typing_override: Option<usize>,
    selection: (i32, i32),
}

#[derive(Clone, Debug)]
struct EditTransaction {
    start: usize,
    before: EditFragment,
    after: EditFragment,
    before_typing_style: CharacterStyle,
    after_typing_style: CharacterStyle,
    before_typing_override: Option<usize>,
    after_typing_override: Option<usize>,
    before_selection: (i32, i32),
    after_selection: (i32, i32),
    retained_bytes: usize,
    /// Set by [`RichComposeDocument::mark_last_edit`].
    mark: Option<EditMark>,
}

/// An edit someone wants to follow through undo and redo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditMark(u64);

/// Where a marked edit stands in the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditState {
    Applied,
    Undone,
    /// It can no longer be undone or redone.
    Gone,
}

/// One edit of the text: the bytes from `start` to `old_end` became those from `start` to
/// `new_end`, making `revision`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextChange {
    revision: u64,
    start: usize,
    old_end: usize,
    new_end: usize,
}

/// Where a range of the text as it was at some revision is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Followed {
    /// No edit since then reached it.
    Kept(Range<usize>),
    /// No edit reached it, but one started or ended right at its edge, so what borders it
    /// may have changed.
    Bordered(Range<usize>),
    /// An edit changed it, or the revision is too old to follow.
    Lost,
}

/// Estimated bytes owned by the document and its history. This excludes
/// allocator overhead, temporary edit buffers, renderer state, and process RAM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct ComposeMemoryUsage {
    pub(crate) text_capacity: usize,
    pub(crate) style_run_bytes: usize,
    pub(crate) link_bytes: usize,
    pub(crate) undo_bytes: usize,
    pub(crate) redo_bytes: usize,
    pub(crate) total_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveMarks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    /// The text color all of the selection shares, which may be none, or nothing when it
    /// mixes several.
    pub(crate) color: Option<Option<Rgb>>,
    pub(crate) highlight: Option<Option<Rgb>>,
    /// The size all of the selection has, or none when it has several.
    pub(crate) size: Option<TextSize>,
    pub(crate) family: Option<FontFamily>,
    /// The alignment of the paragraph the selection starts in.
    pub(crate) align: Alignment,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ComposeSelection {
    pub start: i32,
    pub end: i32,
}

/// Rich formatting state for the native composer.
///
/// The editable string never contains markup. Styles are retained as merged
/// UTF-8 byte ranges and exposed to the cosmic-text layout surface. Slint's
/// hidden TextInput is only the platform IME/clipboard
/// bridge; it is not used to position visible text, selection, or the caret.
#[derive(Default)]
pub struct RichComposeDocument {
    text: String,
    styles: Vec<ComposeStyleRun>,
    typing_style: CharacterStyle,
    typing_override: Option<usize>,
    /// The font and size of the default text style from Settings, which an empty message starts
    /// in and removing formatting returns to.
    base: CharacterStyle,
    selection: (i32, i32),
    undo: VecDeque<EditTransaction>,
    redo: VecDeque<EditTransaction>,
    typing_group_active: bool,
    revision: u64,
    /// The latest edits of the text, oldest first.
    text_changes: VecDeque<TextChange>,
    /// Positions in the text from this revision on can be followed through `text_changes`.
    text_changes_since: u64,
    /// The revision when it was last opened or saved, to tell whether it changed since.
    saved_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Bullet,
    Number,
    Quote,
}

impl RichComposeDocument {
    pub fn reset(&mut self) {
        let revision = self.revision.wrapping_add(1);
        *self = Self::default();
        self.revision = revision;
        self.text_changes_since = revision;
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Takes the text and formatting as they are for what was last opened or saved.
    pub(crate) fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// Whether the text or its formatting changed since it was last opened or saved.
    pub(crate) fn changed_since_saved(&self) -> bool {
        self.revision != self.saved_revision
    }

    /// Opens a signature's HTML to write, the composer's own as it was and other HTML with
    /// the formatting pasting keeps, with nothing to undo.
    pub(crate) fn load_signature_html(&mut self, html: &str) -> ComposeSelection {
        let selection = match signature_runs(html, &self.base) {
            Some((text, styles)) => self.reopen(text, styles),
            None => self.load("", 0, 0),
        };
        self.mark_saved();
        selection
    }

    /// Sets the default text style's font and size. What's typed into an empty message starts
    /// in it.
    pub(crate) fn set_base_style(&mut self, family: FontFamily, size: TextSize) {
        self.base = CharacterStyle {
            family,
            size,
            ..CharacterStyle::default()
        };
        if self.text.is_empty() {
            self.typing_style = CharacterStyle {
                align: self.typing_style.align,
                ..self.base.clone()
            };
        }
    }

    pub fn selection(&self) -> ComposeSelection {
        ComposeSelection {
            start: self.selection.0,
            end: self.selection.1,
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Where `range`, in the text as it was at `revision`, is now.
    pub(crate) fn follow(&self, revision: u64, range: Range<usize>) -> Followed {
        if revision < self.text_changes_since || revision > self.revision {
            return Followed::Lost;
        }
        let mut range = range;
        let mut bordered = false;
        for change in self.text_changes.iter().filter(|c| c.revision > revision) {
            if range.end < change.start {
                continue;
            }
            if range.end == change.start {
                bordered = true;
                continue;
            }
            if range.start < change.old_end {
                return Followed::Lost;
            }
            bordered |= range.start == change.old_end;
            let shift = |offset: usize| offset - change.old_end + change.new_end;
            range = shift(range.start)..shift(range.end);
        }
        if bordered {
            Followed::Bordered(range)
        } else {
            Followed::Kept(range)
        }
    }

    fn record_text_change(&mut self, start: usize, old_end: usize, new_end: usize) {
        if self.text_changes.len() == MAX_TEXT_CHANGES
            && let Some(oldest) = self.text_changes.pop_front()
        {
            self.text_changes_since = oldest.revision;
        }
        self.text_changes.push_back(TextChange {
            revision: self.revision,
            start,
            old_end,
            new_end,
        });
    }

    pub(crate) fn style_runs(&self) -> &[ComposeStyleRun] {
        &self.styles
    }

    pub(crate) fn typing_style(&self) -> &CharacterStyle {
        &self.typing_style
    }

    #[allow(dead_code)]
    pub(crate) fn memory_usage(&self) -> ComposeMemoryUsage {
        let mut usage = ComposeMemoryUsage {
            text_capacity: self.text.capacity(),
            style_run_bytes: self.styles.capacity() * std::mem::size_of::<ComposeStyleRun>(),
            undo_bytes: self.undo.capacity() * std::mem::size_of::<EditTransaction>(),
            redo_bytes: self.redo.capacity() * std::mem::size_of::<EditTransaction>(),
            ..Default::default()
        };
        let mut seen_text = HashSet::new();
        let mut seen_links = HashSet::new();
        count_style_links(&self.typing_style, &mut seen_links, &mut usage.link_bytes);
        for run in &self.styles {
            count_style_links(&run.style, &mut seen_links, &mut usage.link_bytes);
        }
        for (transactions, bytes) in [
            (&self.undo, &mut usage.undo_bytes),
            (&self.redo, &mut usage.redo_bytes),
        ] {
            for transaction in transactions {
                for style in [
                    &transaction.before_typing_style,
                    &transaction.after_typing_style,
                ] {
                    count_style_links(style, &mut seen_links, &mut usage.link_bytes);
                }
                for fragment in [&transaction.before, &transaction.after] {
                    *bytes += fragment.styles.capacity() * std::mem::size_of::<ComposeStyleRun>();
                    if seen_text.insert(Arc::as_ptr(&fragment.text) as *const u8 as usize) {
                        *bytes += fragment_text_bytes(&fragment.text);
                    }
                    for run in &fragment.styles {
                        count_style_links(&run.style, &mut seen_links, &mut usage.link_bytes);
                    }
                }
            }
        }
        usage.total_bytes = std::mem::size_of::<Self>()
            + usage.text_capacity
            + usage.style_run_bytes
            + usage.link_bytes
            + usage.undo_bytes
            + usage.redo_bytes;
        usage
    }

    /// Set the text a message starts with: a reply's quote, a reopened draft,
    /// a mail link's body. Undo stops here instead of removing it.
    pub fn load(&mut self, text: &str, anchor: i32, cursor: i32) -> ComposeSelection {
        let selection = self.synchronize(text, anchor, cursor);
        self.undo.clear();
        self.redo.clear();
        self.typing_group_active = false;
        selection
    }

    pub fn synchronize(&mut self, new_text: &str, anchor: i32, cursor: i32) -> ComposeSelection {
        self.synchronize_key_edit(new_text, anchor, cursor, "")
    }

    /// `key_text` must come from the immediately preceding ordinary key event.
    /// Callers pass an empty string for paste, IME, and unclassified edits.
    pub fn synchronize_key_edit(
        &mut self,
        new_text: &str,
        anchor: i32,
        cursor: i32,
        key_text: &str,
    ) -> ComposeSelection {
        let continuation = continue_list_edit(&self.text, new_text);
        let (effective_text, anchor, cursor) = match continuation {
            Some((text, position)) => (Cow::Owned(text), position, position),
            None => (Cow::Borrowed(new_text), anchor, cursor),
        };

        if effective_text.as_ref() != self.text {
            let (start, old_end, new_end) = text_change_ranges(&self.text, &effective_text);
            let inserted_len = new_end - start;
            let inserted = ComposeStyleRun {
                range: 0..inserted_len,
                style: if inserted_len > 0 {
                    self.typing_style.clone()
                } else {
                    CharacterStyle::default()
                },
            };
            let styles = replaced_style_runs(
                &self.styles,
                start,
                old_end,
                std::slice::from_ref(&inserted),
            );
            let before = self.take_snapshot();
            let marker_len = effective_text.len().saturating_sub(new_text.len());
            self.styles = styles;
            self.text = effective_text.into_owned();
            if marker_len > 0 {
                // The marker of a list item that Enter continues, which readers show in their
                // usual text, as markers the toolbar adds come in.
                let marker_end = valid_offset(&self.text, cursor);
                self.edit_styles(marker_end - marker_len, marker_end, |style| {
                    *style = CharacterStyle::default();
                });
            }
            self.auto_link_urls();
            self.normalize_alignment(start..new_end);
            self.bump_revision();
            let selection = self.update_selection_internal(anchor, cursor, false);
            self.commit_edit_with_key(before, key_text);
            return selection;
        }
        self.update_selection(anchor, cursor)
    }

    pub fn update_selection(&mut self, anchor: i32, cursor: i32) -> ComposeSelection {
        self.update_selection_internal(anchor, cursor, true)
    }

    fn update_selection_internal(
        &mut self,
        anchor: i32,
        cursor: i32,
        break_typing_group: bool,
    ) -> ComposeSelection {
        let anchor = valid_offset(&self.text, anchor);
        let cursor = valid_offset(&self.text, cursor);
        if break_typing_group && self.selection != (to_i32(anchor), to_i32(cursor)) {
            self.typing_group_active = false;
        }
        self.selection = (to_i32(anchor), to_i32(cursor));

        if anchor == cursor && self.typing_override != Some(cursor) {
            self.typing_style = self.typing_style_at(cursor);
            self.typing_override = None;
        } else if anchor != cursor {
            self.typing_override = None;
        }

        ComposeSelection {
            start: to_i32(anchor),
            end: to_i32(cursor),
        }
    }

    /// The style of what's typed at `cursor`: that of the text before it, or at the start of a
    /// line, list item or quote, of the text after it, past the marker. An empty last line goes
    /// on in the style and alignment of the line above it, as pressing Enter does in a word
    /// processor, but not in its link, and an empty message starts in the default text style.
    fn typing_style_at(&self, cursor: usize) -> CharacterStyle {
        // What's typed next to an image is text.
        CharacterStyle {
            image: None,
            ..self.typing_style_near(cursor)
        }
    }

    /// The style of the text around `cursor`, which typing there follows.
    fn typing_style_near(&self, cursor: usize) -> CharacterStyle {
        let line_start = self.text[..cursor].rfind('\n').map_or(0, |index| index + 1);
        let line_end = self.text[cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| cursor + index);
        let content_start = line_start
            + block_prefix_text(&self.text[line_start..line_end]).map_or(0, |(_, bytes)| bytes);
        if cursor > content_start {
            return self
                .style_at(cursor - 1)
                .cloned()
                .unwrap_or_else(|| self.base.clone());
        }
        if let Some(style) = self.style_at(content_start) {
            return style.clone();
        }
        line_start
            .checked_sub(1)
            .and_then(|line_end_above| self.style_at(line_end_above))
            .map_or_else(
                || self.base.clone(),
                |style| CharacterStyle {
                    link: None,
                    link_is_auto: false,
                    ..style.clone()
                },
            )
    }

    pub fn insert_text(&mut self, value: &str) -> ComposeSelection {
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        if value.is_empty() && start == end {
            return self.update_selection(to_i32(start), to_i32(end));
        }

        let style = self.typing_style.clone();
        let inserted = ComposeStyleRun {
            range: 0..value.len(),
            style,
        };
        let styles = replaced_style_runs(&self.styles, start, end, std::slice::from_ref(&inserted));
        let mut text = String::with_capacity(self.text.len() - (end - start) + value.len());
        text.push_str(&self.text[..start]);
        text.push_str(value);
        text.push_str(&self.text[end..]);
        let before = self.take_snapshot();
        self.typing_override = None;
        self.styles = styles;
        self.text = text;
        self.auto_link_urls();
        self.normalize_alignment(start..start + value.len());
        self.bump_revision();
        let caret = start.saturating_add(value.len());
        let selection = self.update_selection(to_i32(caret), to_i32(caret));
        self.commit_edit(before);
        selection
    }

    /// Replaces `range` with `text` as one undoable edit and puts the caret
    /// at `caret`. The text comes in unformatted: what moves into the message
    /// from outside it, such as a quote, takes no formatting from the caret.
    pub fn replace_unformatted(
        &mut self,
        range: Range<usize>,
        text: &str,
        caret: usize,
    ) -> ComposeSelection {
        self.replace_styled(range, text, CharacterStyle::default(), caret)
    }

    /// Pastes HTML from the clipboard over the selection, as one undoable edit with the caret
    /// after it: text copied from a message being written as it was, and HTML from elsewhere
    /// with the formatting `compose_paste` keeps. `None` when it holds no text, for the plain
    /// text to go in instead.
    pub fn paste_html(&mut self, html: &str) -> Option<ComposeSelection> {
        let (mut text, mut runs) = if html.contains(COPIED_MARK) {
            import_body_html(html)
        } else {
            crate::compose_paste::styled_text(html, &self.base)
        }
        .filter(|(text, _)| !text.trim().is_empty())?;
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        // A list or quote marker belongs at the start of a line.
        let line_start = self.text[..start].rfind('\n').map_or(0, |index| index + 1);
        let first_line = &text[..text.find('\n').unwrap_or(text.len())];
        if start != line_start
            && let Some((_, marker)) = block_prefix_text(first_line)
        {
            text.drain(..marker);
            runs.retain_mut(|run| {
                run.range =
                    run.range.start.saturating_sub(marker)..run.range.end.saturating_sub(marker);
                !run.range.is_empty()
            });
        }
        if text.is_empty() {
            return None;
        }
        let caret = start + text.len();
        Some(self.replace_runs(start..end, &text, &runs, caret))
    }

    /// Swaps the signature block `old`, which the text holds once, for the signature `html`
    /// under a `-- ` line, in its formatting or as `plain` text, or puts that block at `at` when
    /// there's no old one, as one undoable edit that leaves the caret where it was. Returns the
    /// block it put in, which marks the signature for the next swap, or `None` when `html` holds
    /// no text.
    pub(crate) fn replace_signature(
        &mut self,
        old: Option<&str>,
        at: usize,
        html: Option<&str>,
        plain: bool,
    ) -> (ComposeSelection, Option<String>) {
        let range =
            match old.and_then(|old| self.text.find(old).map(|start| start..start + old.len())) {
                Some(range) => range,
                None => {
                    let at = valid_offset(&self.text, to_i32(at));
                    at..at
                }
            };
        let signature =
            html.and_then(|html| signature_runs(html, &self.base))
                .map(|(text, runs)| {
                    // A signature takes the message's font and size where it names none, as mail
                    // apps' signatures match the default message font.
                    let runs = runs
                        .into_iter()
                        .map(|mut run| {
                            if run.style.family == FontFamily::Default {
                                run.style.family = self.base.family;
                            }
                            if run.style.size == TextSize::Normal {
                                run.style.size = self.base.size;
                            }
                            run
                        })
                        .collect::<Vec<_>>();
                    if !plain {
                        return (text, runs);
                    }
                    // In plain text, as plain text writes links and pictures.
                    let (text, _) = plain_text_of(&text, &runs, false, 0);
                    let run = ComposeStyleRun {
                        range: 0..text.len(),
                        style: self.base.clone(),
                    };
                    (text, vec![run])
                });
        let mut block = String::new();
        let mut runs = Vec::new();
        if let Some((text, signature_runs)) = signature {
            block.push_str(SIGNATURE_SEPARATOR);
            runs.push(ComposeStyleRun {
                range: 0..SIGNATURE_SEPARATOR.len(),
                style: self.base.clone(),
            });
            let shift = block.len();
            block.push_str(&text);
            runs.extend(signature_runs.into_iter().map(|run| ComposeStyleRun {
                range: run.range.start + shift..run.range.end + shift,
                style: run.style,
            }));
        }
        if range.is_empty() && block.is_empty() {
            return (self.selection(), None);
        }
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let follow = |offset: usize| {
            if offset <= range.start {
                offset
            } else if offset >= range.end {
                offset - range.len() + block.len()
            } else {
                range.start
            }
        };
        let (anchor, cursor) = (follow(anchor), follow(cursor));
        let selection = self.replace_runs(range, &block, &runs, cursor);
        let selection = if anchor == cursor {
            selection
        } else {
            self.update_selection(to_i32(anchor), to_i32(cursor))
        };
        (selection, (!block.is_empty()).then_some(block))
    }

    /// Puts `image` in place of the selection, as one undoable edit with the caret after it,
    /// in the paragraph's alignment and any link the text there has.
    pub(crate) fn insert_image(&mut self, image: InlineImage) -> ComposeSelection {
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        let style = CharacterStyle {
            image: Some(Arc::new(image)),
            ..self.typing_style.clone()
        };
        let mut placeholder = [0; 4];
        let text = IMAGE_PLACEHOLDER.encode_utf8(&mut placeholder);
        let caret = start + text.len();
        self.replace_styled(start..end, text, style, caret)
    }

    /// The image the selection holds, when it holds that and nothing else, for its controls.
    pub(crate) fn selected_image(&self) -> Option<&Arc<InlineImage>> {
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        let mut selected = self.text[start..end].chars();
        if selected.next() != Some(IMAGE_PLACEHOLDER) || selected.next().is_some() {
            return None;
        }
        self.style_at(start)?.image.as_ref()
    }

    /// The images in the text, in order.
    pub(crate) fn images(&self) -> impl Iterator<Item = &Arc<InlineImage>> {
        self.styles
            .iter()
            .filter_map(|run| run.style.image.as_ref())
    }

    /// The text as mail read as plain text gets it: each link followed by its address where its
    /// text doesn't show it, and each picture named in its place.
    pub(crate) fn plain_text(&self) -> String {
        plain_text_of(&self.text, &self.styles, true, 0).0
    }

    /// The text in `range` as [`Self::make_plain`] writes it.
    pub(crate) fn plain_text_in(&self, range: Range<usize>) -> String {
        let range = valid_offset(&self.text, to_i32(range.start))
            ..valid_offset(&self.text, to_i32(range.end));
        let runs = self
            .styles
            .iter()
            .filter_map(|run| {
                let start = run.range.start.max(range.start);
                let end = run.range.end.min(range.end);
                (start < end).then(|| ComposeStyleRun {
                    range: start - range.start..end - range.start,
                    style: run.style.clone(),
                })
            })
            .collect::<Vec<_>>();
        plain_text_of(&self.text[range], &runs, false, 0).0
    }

    /// Takes all formatting out of the text, as one undoable edit that keeps the caret in its
    /// place: links keep their addresses after their text, and pictures leave their alt text.
    /// `None` when the text has no formatting to take out.
    pub(crate) fn make_plain(&mut self) -> Option<ComposeSelection> {
        if self
            .styles
            .iter()
            .all(|run| is_plain_style(&run.style, &self.base))
        {
            return None;
        }
        let caret = valid_offset(&self.text, self.selection.1);
        let (text, caret) = plain_text_of(&self.text, &self.styles, false, caret);
        let end = self.text.len();
        Some(self.replace_styled(0..end, &text, self.base.clone(), caret))
    }

    /// The text as plain text shows it, each image as its description.
    pub(crate) fn text_with_image_descriptions(&self) -> String {
        let mut text = String::with_capacity(self.text.len());
        let mut copied = 0;
        for (offset, _) in self.text.match_indices(IMAGE_PLACEHOLDER) {
            if let Some(image) = self.style_at(offset).and_then(|style| style.image.as_ref()) {
                text.push_str(&self.text[copied..offset]);
                text.push_str(&image.description());
                copied = offset + IMAGE_PLACEHOLDER.len_utf8();
            }
        }
        text.push_str(&self.text[copied..]);
        text
    }

    /// The selection as HTML and as plain text, for the clipboard: the HTML pastes back into a
    /// message as it was, and other apps read either.
    pub fn selection_html(&self) -> Option<(String, String)> {
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        if start == end {
            return None;
        }
        let part = RichComposeDocument {
            text: self.text[start..end].to_owned(),
            styles: self
                .styles
                .iter()
                .filter(|run| run.range.start < end && run.range.end > start)
                .map(|run| ComposeStyleRun {
                    range: run.range.start.max(start) - start..run.range.end.min(end) - start,
                    style: run.style.clone(),
                })
                .collect(),
            ..Self::default()
        };
        let html = part.body_html()?;
        Some((
            format!("{COPIED_MARK}{html}"),
            with_plain_text_quotes(&part.text),
        ))
    }

    /// Replaces the word at `range` with `text` in the word's formatting, as one undoable
    /// edit, with the caret after it: a spelling correction.
    pub fn replace_word(&mut self, range: Range<usize>, text: &str) -> ComposeSelection {
        let style = self.style_at(range.start).cloned().unwrap_or_default();
        let caret = range.start + text.len();
        self.replace_styled(range, text, style, caret)
    }

    fn replace_styled(
        &mut self,
        range: Range<usize>,
        text: &str,
        style: CharacterStyle,
        caret: usize,
    ) -> ComposeSelection {
        let inserted = ComposeStyleRun {
            range: 0..text.len(),
            style,
        };
        self.replace_runs(range, text, std::slice::from_ref(&inserted), caret)
    }

    /// Replaces `range` with `text` in the styles of `runs`, which cover it, as one undoable
    /// edit, and puts the caret at `caret`.
    fn replace_runs(
        &mut self,
        range: Range<usize>,
        text: &str,
        runs: &[ComposeStyleRun],
        caret: usize,
    ) -> ComposeSelection {
        let styles = replaced_style_runs(&self.styles, range.start, range.end, runs);
        let mut replaced = String::with_capacity(self.text.len() - range.len() + text.len());
        replaced.push_str(&self.text[..range.start]);
        replaced.push_str(text);
        replaced.push_str(&self.text[range.end..]);
        let before = self.take_snapshot();
        self.typing_override = None;
        self.styles = styles;
        self.text = replaced;
        self.auto_link_urls();
        self.normalize_alignment(range.start..range.start + text.len());
        self.bump_revision();
        let selection = self.update_selection(to_i32(caret), to_i32(caret));
        self.commit_edit(before);
        selection
    }

    pub fn format(
        &mut self,
        kind: &str,
        source: &str,
        anchor: i32,
        cursor: i32,
    ) -> ComposeSelection {
        self.typing_group_active = false;
        if source != self.text {
            self.synchronize(source, anchor, cursor);
        } else {
            self.update_selection(anchor, cursor);
        }

        let anchor = valid_offset(&self.text, anchor);
        let cursor = valid_offset(&self.text, cursor);
        let (start, end) = ordered(anchor, cursor);

        match kind.split_once(':') {
            Some(("color", value)) => {
                let color = parse_rgb(value);
                self.set_text_style(start, end, |style| style.color = color);
            }
            Some(("highlight", value)) => {
                let highlight = parse_rgb(value);
                self.set_text_style(start, end, |style| style.highlight = highlight);
            }
            Some(("size", value)) => {
                if let Some(size) = TextSize::from_name(value) {
                    self.set_text_style(start, end, |style| style.size = size);
                }
            }
            Some(("family", value)) => {
                if let Some(family) = FontFamily::from_name(value) {
                    self.set_text_style(start, end, |style| style.family = family);
                }
            }
            Some(("align", value)) => {
                if let Some(align) = Alignment::from_name(value) {
                    self.set_alignment(start, end, align);
                }
            }
            Some(("image-size", value)) => {
                if let Some(size) = ImageSize::from_name(value) {
                    self.edit_images(start, end, |image| {
                        if size.fits(image.width) {
                            image.shown_width = size.width();
                        }
                    });
                }
            }
            // A width from dragging the image's corner, in CSS pixels.
            Some(("image-width", value)) => {
                if let Ok(width) = value.trim().parse::<u32>() {
                    self.edit_images(start, end, |image| {
                        image.shown_width =
                            (width < image.width).then_some(width.max(MIN_IMAGE_WIDTH));
                    });
                }
            }
            Some(("image-alt", value)) => {
                let alt: Arc<str> = value
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .into();
                self.edit_images(start, end, |image| image.alt = alt.clone());
            }
            _ => match kind {
                "bold" => self.toggle_mark(BOLD, start, end),
                "italic" => self.toggle_mark(ITALIC, start, end),
                "underline" => self.toggle_mark(UNDERLINE, start, end),
                "strike" => self.toggle_mark(STRIKE, start, end),
                "code" => self.toggle_mark(CODE, start, end),
                // Back to the default text style, as Remove formatting leaves text in other
                // composers: links and paragraphs' alignment stay.
                "clear" => {
                    let base = self.base.clone();
                    self.set_text_style(start, end, |style| {
                        *style = CharacterStyle {
                            link: style.link.clone(),
                            link_is_auto: style.link_is_auto,
                            align: style.align,
                            image: style.image.clone(),
                            ..base.clone()
                        };
                    });
                }
                // What the selection holds goes, as a selected image's Remove button asks.
                "delete" if start != end => return self.insert_text(""),
                "bullet" => return self.toggle_block(BlockKind::Bullet, start, end),
                "number" => return self.toggle_block(BlockKind::Number, start, end),
                "quote" => return self.toggle_block(BlockKind::Quote, start, end),
                _ => {}
            },
        }

        self.selection = (to_i32(start), to_i32(end));
        ComposeSelection {
            start: to_i32(start),
            end: to_i32(end),
        }
    }

    /// Mark the edit just made, to follow it through undo and redo.
    pub fn mark_last_edit(&mut self) -> Option<EditMark> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Typing that follows is an edit of its own.
        self.typing_group_active = false;
        let transaction = self.undo.back_mut()?;
        let mark = EditMark(NEXT.fetch_add(1, Ordering::Relaxed));
        transaction.mark = Some(mark);
        Some(mark)
    }

    pub fn edit_state(&self, mark: EditMark) -> EditState {
        let marked = |transaction: &EditTransaction| transaction.mark == Some(mark);
        if self.undo.iter().any(marked) {
            EditState::Applied
        } else if self.redo.iter().any(marked) {
            EditState::Undone
        } else {
            EditState::Gone
        }
    }

    pub fn history(&mut self, direction: &str) -> Option<ComposeSelection> {
        self.typing_group_active = false;
        if direction == "redo" {
            let mut transaction = self.redo.pop_back()?;
            transaction.before_typing_style = self.typing_style.clone();
            transaction.before_typing_override = self.typing_override;
            transaction.before_selection = self.selection;
            self.apply_transaction(&transaction, true);
            self.undo.push_back(transaction);
        } else {
            let mut transaction = self.undo.pop_back()?;
            transaction.after_typing_style = self.typing_style.clone();
            transaction.after_typing_override = self.typing_override;
            transaction.after_selection = self.selection;
            self.apply_transaction(&transaction, false);
            self.redo.push_back(transaction);
        }
        self.trim_history();
        Some(ComposeSelection {
            start: self.selection.0,
            end: self.selection.1,
        })
    }

    pub fn active_marks(&self) -> ActiveMarks {
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        let typing = std::slice::from_ref(&self.typing_style);
        let selected: Vec<&CharacterStyle> = if start == end {
            typing.iter().collect()
        } else {
            self.styles
                .iter()
                .filter(|run| run.range.start < end && run.range.end > start)
                .filter(|run| {
                    self.text[run.range.start.max(start)..run.range.end.min(end)]
                        .chars()
                        .any(|character| !character.is_whitespace())
                })
                .map(|run| &run.style)
                .collect()
        };
        let marks = selected
            .iter()
            .map(|style| style.marks)
            .reduce(|common, value| common & value)
            .unwrap_or_default();
        /// The value every selected character shares, if they share one.
        fn shared<T: PartialEq + Copy>(
            styles: &[&CharacterStyle],
            value: impl Fn(&CharacterStyle) -> T,
        ) -> Option<T> {
            let first = value(styles.first()?);
            styles
                .iter()
                .all(|style| value(style) == first)
                .then_some(first)
        }
        let line_start = self.text[..start].rfind('\n').map_or(0, |index| index + 1);
        ActiveMarks {
            bold: marks & BOLD != 0,
            italic: marks & ITALIC != 0,
            underline: marks & UNDERLINE != 0,
            strike: marks & STRIKE != 0,
            code: marks & CODE != 0,
            color: shared(&selected, |style| style.color),
            highlight: shared(&selected, |style| style.highlight),
            size: shared(&selected, |style| style.size),
            family: shared(&selected, |style| style.family),
            align: if start == end && line_start == self.text.len() {
                self.typing_style.align
            } else {
                self.paragraph_alignment(line_start)
            },
        }
    }

    /// The alignment of the paragraph starting at `line_start`: that of its first character
    /// after any list or quote marker, or of its line end when it holds nothing else.
    pub(crate) fn paragraph_alignment(&self, line_start: usize) -> Alignment {
        let line = &self.text[line_start..];
        let line = &line[..line.find('\n').unwrap_or(line.len())];
        let marker = block_prefix_text(line).map_or(0, |(_, bytes)| bytes);
        self.style_at(line_start + marker)
            .or_else(|| self.style_at(line_start))
            .map_or_else(Alignment::default, |style| style.align)
    }

    pub fn active_link(&self) -> Option<&str> {
        let anchor = valid_offset(&self.text, self.selection.0);
        let cursor = valid_offset(&self.text, self.selection.1);
        let (start, end) = ordered(anchor, cursor);
        if start == end {
            return None;
        }

        let mut links = self
            .styles
            .iter()
            .filter(|run| run.range.start < end && run.range.end > start)
            .filter(|run| {
                self.text[run.range.start.max(start)..run.range.end.min(end)]
                    .chars()
                    .any(|character| !character.is_whitespace())
            })
            .map(|run| run.style.link.as_deref());
        let first = links.next().flatten()?;
        links.all(|link| link == Some(first)).then_some(first)
    }

    pub fn set_link(
        &mut self,
        url: &str,
        source: &str,
        anchor: i32,
        cursor: i32,
    ) -> ComposeSelection {
        self.typing_group_active = false;
        if source != self.text {
            self.synchronize(source, anchor, cursor);
        } else {
            self.update_selection(anchor, cursor);
        }

        let anchor = valid_offset(&self.text, anchor);
        let cursor = valid_offset(&self.text, cursor);
        let (start_byte, end_byte) = ordered(anchor, cursor);
        if start_byte == end_byte {
            return ComposeSelection {
                start: to_i32(start_byte),
                end: to_i32(end_byte),
            };
        }

        let normalized = normalize_link(url).map(Arc::<str>::from);
        let before = self.begin_style_edit(start_byte..end_byte);
        self.typing_override = None;
        self.edit_styles(start_byte, end_byte, |style| {
            style.link = normalized.clone();
            style.link_is_auto = false;
        });
        self.selection = (to_i32(start_byte), to_i32(end_byte));
        self.bump_revision();
        self.commit_style_edit(start_byte, before);
        ComposeSelection {
            start: to_i32(start_byte),
            end: to_i32(end_byte),
        }
    }

    pub fn body_html(&self) -> Option<String> {
        self.html_for(&self.text, &self.styles)
    }

    /// [`Self::body_html`], or the HTML for what the user wrote above the original a reply
    /// quotes, without the blank lines the text ends with, with the images `file` gives
    /// another file for written with that one, as photos go out shrunk. They show at the same
    /// size.
    pub(crate) fn body_html_with(
        &self,
        above_quote: bool,
        file: impl Fn(&InlineImage) -> Option<Arc<[u8]>>,
    ) -> Option<String> {
        let styles = self
            .styles
            .iter()
            .map(|run| {
                let mut run = run.clone();
                if let Some(image) = &run.style.image
                    && let Some(data) = file(image)
                {
                    run.style.image = Some(Arc::new(InlineImage {
                        data,
                        ..InlineImage::clone(image)
                    }));
                }
                run
            })
            .collect::<Vec<_>>();
        let text = if above_quote {
            self.text.trim_end_matches(['\r', '\n'])
        } else {
            &self.text
        };
        self.html_for(text, &styles)
    }

    /// The HTML for `text`, the document's text or the start of it, in `styles`.
    fn html_for(&self, text: &str, styles: &[ComposeStyleRun]) -> Option<String> {
        if text.trim().is_empty() {
            return None;
        }

        let mut html = String::with_capacity(text.len());
        let mut open = None;
        let mut line_start = 0;

        for line in text.split('\n') {
            let block = html_line_block(line);
            let skip = block.map_or(0, |(_, bytes)| bytes);
            // Text pasted from Windows ends its lines with CRLF.
            let end = line_start + line.strip_suffix('\r').unwrap_or(line).len();
            let content = (line_start + skip).min(end)..end;
            let align = self.paragraph_alignment(line_start);
            match block.map(|(block, _)| block) {
                Some(LineBlock::Bullet) => {
                    if open != Some(OpenHtmlBlock::Bullets) {
                        close_html_block(&mut html, &mut open);
                        html.push_str("<ul>");
                        open = Some(OpenHtmlBlock::Bullets);
                    }
                    append_list_item_start(&mut html, align);
                    append_inline_html(&mut html, &self.text, styles, content);
                    html.push_str("</li>");
                }
                Some(LineBlock::Number(number)) => {
                    // Recipients see the numbers shown in the composer: a
                    // number that does not continue the list starts a new one.
                    let number = u64::from(number);
                    if open != Some(OpenHtmlBlock::Numbers { next: number }) {
                        close_html_block(&mut html, &mut open);
                        if number == 1 {
                            html.push_str("<ol>");
                        } else {
                            use std::fmt::Write;
                            write!(html, "<ol start=\"{number}\">")
                                .expect("writing to a String cannot fail");
                        }
                    }
                    open = Some(OpenHtmlBlock::Numbers { next: number + 1 });
                    append_list_item_start(&mut html, align);
                    append_inline_html(&mut html, &self.text, styles, content);
                    html.push_str("</li>");
                }
                Some(LineBlock::Quote) => {
                    if open != Some(OpenHtmlBlock::Quote) {
                        close_html_block(&mut html, &mut open);
                        html.push_str("<blockquote>");
                        open = Some(OpenHtmlBlock::Quote);
                    }
                    append_line_html(&mut html, &self.text, styles, content, align);
                }
                None => {
                    close_html_block(&mut html, &mut open);
                    append_line_html(&mut html, &self.text, styles, content, align);
                }
            }
            line_start += line.len() + 1;
        }
        close_html_block(&mut html, &mut open);
        Some(html)
    }

    /// Reopens `text`, a saved message's plain text, with the formatting the
    /// HTML `body_html` wrote for it. Returns `None`, leaving the document
    /// untouched, when the HTML holds other text: markup the composer could
    /// not have written, or HTML from versions that kept neither list
    /// numbers nor spacing. Callers then load the plain text.
    pub fn load_html(&mut self, html: &str, text: &str) -> Option<ComposeSelection> {
        let (_, styles) = saved_styles(html, text, true)?;
        Some(self.reopen(text.to_owned(), styles))
    }

    /// Reopens HTML that another mail app wrote, for a message edited as new, with the
    /// formatting and images pasting keeps. Returns `None`, leaving the document untouched,
    /// when it holds no text.
    pub fn load_foreign_html(&mut self, html: &str) -> Option<ComposeSelection> {
        let (text, styles) = crate::compose_paste::styled_text(html, &self.base)
            .filter(|(text, _)| !text.trim().is_empty())?;
        Some(self.reopen(text, styles))
    }

    /// Reopens the start of `text` that `html` holds, up to a line break:
    /// what the user wrote above the original a saved reply quotes. Returns
    /// how much of `text` that is, or `None`, leaving the document
    /// untouched, when the HTML holds other text.
    pub fn load_html_start(&mut self, html: &str, text: &str) -> Option<usize> {
        let (end, styles) = saved_styles(html, text, false)?;
        self.reopen(text[..end].to_owned(), styles);
        Some(end)
    }

    /// Loads `text` formatted with `styles` as a reopened message: nothing
    /// to undo, and the caret at its end. What's written next keeps the
    /// default text style.
    fn reopen(&mut self, text: String, styles: Vec<ComposeStyleRun>) -> ComposeSelection {
        let revision = self.revision.wrapping_add(1);
        *self = Self {
            text,
            styles,
            revision,
            text_changes_since: revision,
            base: std::mem::take(&mut self.base),
            ..Self::default()
        };
        self.auto_link_urls();
        self.normalize_alignment(0..self.text.len());
        let end = to_i32(self.text.len());
        self.update_selection(end, end)
    }

    fn toggle_mark(&mut self, mark: u8, start_byte: usize, end_byte: usize) {
        if start_byte == end_byte {
            self.typing_style.marks ^= mark;
            self.typing_override = Some(start_byte);
            self.bump_revision();
            return;
        }

        let before = self.begin_style_edit(start_byte..end_byte);
        self.typing_override = None;
        let remove = self
            .styles
            .iter()
            .filter(|run| run.range.start < end_byte && run.range.end > start_byte)
            .filter(|run| {
                self.text[run.range.start.max(start_byte)..run.range.end.min(end_byte)]
                    .chars()
                    .any(|character| !character.is_whitespace())
            })
            .all(|run| run.style.marks & mark != 0);
        self.edit_styles(start_byte, end_byte, |style| {
            if remove {
                style.marks &= !mark;
            } else {
                style.marks |= mark;
            }
        });
        self.bump_revision();
        self.commit_style_edit(start_byte, before);
    }

    /// Changes the images from `start_byte` to `end_byte` as one undoable edit.
    fn edit_images(
        &mut self,
        start_byte: usize,
        end_byte: usize,
        mut edit: impl FnMut(&mut InlineImage),
    ) {
        // An edit that changes no image leaves nothing to undo.
        let changes = self
            .styles
            .iter()
            .filter(|run| run.range.start < end_byte && run.range.end > start_byte)
            .filter_map(|run| run.style.image.as_ref())
            .any(|image| {
                let mut changed = InlineImage::clone(image);
                edit(&mut changed);
                changed != **image
            });
        if !changes {
            return;
        }
        self.set_text_style(start_byte, end_byte, |style| {
            if let Some(image) = &style.image {
                let mut changed = InlineImage::clone(image);
                edit(&mut changed);
                if changed != **image {
                    style.image = Some(Arc::new(changed));
                }
            }
        });
    }

    /// Changes the style of the text from `start_byte` to `end_byte` as one undoable edit, or
    /// of what's typed next at a caret.
    fn set_text_style(
        &mut self,
        start_byte: usize,
        end_byte: usize,
        mut edit: impl FnMut(&mut CharacterStyle),
    ) {
        if start_byte == end_byte {
            edit(&mut self.typing_style);
            self.typing_override = Some(start_byte);
            self.bump_revision();
            return;
        }
        let before = self.begin_style_edit(start_byte..end_byte);
        self.typing_override = None;
        self.edit_styles(start_byte, end_byte, edit);
        self.bump_revision();
        self.commit_style_edit(start_byte, before);
    }

    /// Aligns every paragraph from `start_byte` to `end_byte`, or the caret's, as one undoable
    /// edit. An empty last line has nothing to hold it, so what's typed there takes it.
    fn set_alignment(&mut self, start_byte: usize, end_byte: usize, align: Alignment) {
        let first = self.text[..start_byte]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        // A selection that ends where a paragraph starts leaves that paragraph alone.
        let end_probe = if end_byte > start_byte && self.text[..end_byte].ends_with('\n') {
            end_byte - 1
        } else {
            end_byte
        };
        let last = self.text[end_probe..]
            .find('\n')
            .map_or(self.text.len(), |index| end_probe + index + 1);
        self.typing_style.align = align;
        if first == last {
            self.typing_override = Some(start_byte);
            self.bump_revision();
            return;
        }
        let before = self.begin_style_edit(first..last);
        self.edit_styles(first, last, |style| style.align = align);
        self.bump_revision();
        self.commit_style_edit(first, before);
    }

    /// Gives every character of the paragraphs from `range.start` to `range.end` its
    /// paragraph's alignment, after an edit that may have joined or split them.
    fn normalize_alignment(&mut self, range: Range<usize>) {
        let mut line_start = self.text[..range.start.min(self.text.len())]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let mut changes = Vec::new();
        while line_start < self.text.len() {
            let line_end = self.text[line_start..]
                .find('\n')
                .map_or(self.text.len(), |index| line_start + index + 1);
            let align = self.paragraph_alignment(line_start);
            let first = self
                .styles
                .partition_point(|run| run.range.end <= line_start);
            if self.styles[first..]
                .iter()
                .take_while(|run| run.range.start < line_end)
                .any(|run| run.style.align != align)
            {
                changes.push((line_start..line_end, align));
            }
            if line_end > range.end {
                break;
            }
            line_start = line_end;
        }
        for (line, align) in changes {
            self.edit_styles(line.start, line.end, |style| style.align = align);
        }
    }

    fn toggle_block(
        &mut self,
        kind: BlockKind,
        start_byte: usize,
        end_byte: usize,
    ) -> ComposeSelection {
        let start_line = self.text.as_bytes()[..start_byte]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        let end_probe = if end_byte > start_byte {
            end_byte - 1
        } else {
            end_byte
        };
        let end_line = self.text.as_bytes()[..end_probe]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        let all_target = self
            .text
            .split('\n')
            .skip(start_line)
            .take(end_line - start_line + 1)
            .all(|line| block_prefix_text(line).is_some_and(|(current, _)| current == kind));
        let before = self.take_snapshot();
        self.typing_override = None;
        let text_capacity = before
            .text
            .split('\n')
            .enumerate()
            .skip(start_line)
            .take(end_line - start_line + 1)
            .fold(before.text.len(), |size, (index, line)| {
                let removed = block_prefix_text(line).map_or(0, |(_, bytes)| bytes);
                let added = if all_target {
                    0
                } else {
                    block_marker_len(kind, index - start_line + 1)
                };
                size - removed + added
            });
        let mut text = String::with_capacity(text_capacity);
        let mut styles = Vec::with_capacity(before.styles.len());
        let mut source_start = 0;
        let mut start = 0;
        let mut end = 0;
        for (index, line) in before.text.split('\n').enumerate() {
            if index > 0 {
                // A line end keeps its style, which holds an empty paragraph's alignment.
                let byte = text.len();
                text.push('\n');
                let style = style_at_runs(&before.styles, source_start - 1)
                    .cloned()
                    .unwrap_or_default();
                push_style_run(&mut styles, byte..text.len(), style);
            }
            if index == start_line {
                start = text.len();
            }
            let selected = (start_line..=end_line).contains(&index);
            let skip = if selected {
                block_prefix_text(line).map_or(0, |(_, bytes)| bytes)
            } else {
                0
            };
            if selected && !all_target {
                let byte = text.len();
                append_block_marker(&mut text, kind, index - start_line + 1);
                push_style_run(&mut styles, byte..text.len(), CharacterStyle::default());
            }
            let content_start = source_start + skip;
            let content_end = source_start + line.len();
            let destination_start = text.len();
            text.push_str(&before.text[content_start..content_end]);
            let first = before
                .styles
                .partition_point(|run| run.range.end <= content_start);
            for run in before.styles[first..]
                .iter()
                .take_while(|run| run.range.start < content_end)
            {
                let first_byte =
                    run.range.start.max(content_start) - content_start + destination_start;
                let last_byte = run.range.end.min(content_end) - content_start + destination_start;
                push_style_run(&mut styles, first_byte..last_byte, run.style.clone());
            }
            if index == end_line {
                end = text.len();
            }
            source_start += line.len() + 1;
        }
        self.text = text;
        self.styles = styles;
        // Markers come in plain, and take their paragraphs' alignment.
        self.normalize_alignment(start..end);
        self.bump_revision();
        self.selection = (to_i32(start), to_i32(end));
        self.commit_edit(before);
        ComposeSelection {
            start: to_i32(start),
            end: to_i32(end),
        }
    }

    fn auto_link_urls(&mut self) {
        for run in &mut self.styles {
            let style = &mut run.style;
            if style.link_is_auto {
                style.link = None;
                style.link_is_auto = false;
            }
        }
        self.merge_styles();

        for range in typed_urls(&self.text).collect::<Vec<_>>() {
            let link = Arc::<str>::from(&self.text[range.clone()]);
            self.edit_styles(range.start, range.end, |style| {
                if style.link.is_none() || style.link_is_auto {
                    style.link = Some(link.clone());
                    style.link_is_auto = true;
                }
            });
        }
    }

    fn style_at(&self, byte: usize) -> Option<&CharacterStyle> {
        style_at_runs(&self.styles, byte)
    }

    fn replace_style_runs(&mut self, start: usize, end: usize, inserted: &[ComposeStyleRun]) {
        self.styles = replaced_style_runs(&self.styles, start, end, inserted);
    }

    fn edit_styles(&mut self, start: usize, end: usize, mut edit: impl FnMut(&mut CharacterStyle)) {
        split_style_run_at(&mut self.styles, start);
        split_style_run_at(&mut self.styles, end);
        for run in &mut self.styles {
            if run.range.start >= start && run.range.end <= end {
                edit(&mut run.style);
            }
        }
        self.merge_styles();
    }

    fn merge_styles(&mut self) {
        self.styles.dedup_by(|next, previous| {
            if previous.range.end == next.range.start && previous.style == next.style {
                previous.range.end = next.range.end;
                true
            } else {
                false
            }
        });
    }

    fn take_snapshot(&mut self) -> Snapshot {
        Snapshot {
            text: std::mem::take(&mut self.text),
            styles: std::mem::take(&mut self.styles),
            typing_style: self.typing_style.clone(),
            typing_override: self.typing_override,
            selection: self.selection,
        }
    }

    fn begin_style_edit(&self, range: Range<usize>) -> StyleEditBefore {
        StyleEditBefore {
            fragment: EditFragment {
                text: Arc::from(""),
                span_len: range.end - range.start,
                styles: style_fragment(&self.styles, range),
            },
            typing_style: self.typing_style.clone(),
            typing_override: self.typing_override,
            selection: self.selection,
        }
    }

    fn commit_style_edit(&mut self, start: usize, before: StyleEditBefore) {
        let end = start + before.fragment.span_len;
        let after = EditFragment {
            text: before.fragment.text.clone(),
            span_len: before.fragment.span_len,
            styles: style_fragment(&self.styles, start..end),
        };
        let retained_bytes = transaction_bytes(&before.fragment, &after);
        self.push_transaction(EditTransaction {
            start,
            before: before.fragment,
            after,
            before_typing_style: before.typing_style,
            after_typing_style: self.typing_style.clone(),
            before_typing_override: before.typing_override,
            after_typing_override: self.typing_override,
            before_selection: before.selection,
            after_selection: self.selection,
            retained_bytes,
            mark: None,
        });
    }

    fn commit_edit(&mut self, before: Snapshot) {
        self.commit_edit_with_key(before, "");
    }

    fn commit_edit_with_key(&mut self, before: Snapshot, key_text: &str) {
        let (start, old_end, new_end) = text_change_ranges(&before.text, &self.text);
        if old_end > start || new_end > start {
            self.record_text_change(start, old_end, new_end);
        }
        let (start, before_end, after_end) =
            changed_ranges(&before.text, &before.styles, &self.text, &self.styles);
        let before_fragment = edit_fragment(&before.text, &before.styles, start..before_end);
        let after_fragment = edit_fragment(&self.text, &self.styles, start..after_end);
        let retained_bytes = transaction_bytes(&before_fragment, &after_fragment);
        self.push_key_transaction(
            EditTransaction {
                start,
                before: before_fragment,
                after: after_fragment,
                before_typing_style: before.typing_style,
                after_typing_style: self.typing_style.clone(),
                before_typing_override: before.typing_override,
                after_typing_override: self.typing_override,
                before_selection: before.selection,
                after_selection: self.selection,
                retained_bytes,
                mark: None,
            },
            key_text,
        );
    }

    fn push_transaction(&mut self, transaction: EditTransaction) {
        self.typing_group_active = false;
        self.redo.clear();
        self.undo.push_back(transaction);
        self.trim_history();
    }

    fn push_key_transaction(&mut self, transaction: EditTransaction, key_text: &str) {
        let eligible = is_ordinary_typing(&transaction, key_text);
        self.redo.clear();
        if eligible
            && self.typing_group_active
            && let Some(previous) = self.undo.back_mut()
            && can_group_typing(previous, &transaction)
        {
            let mut text =
                String::with_capacity(previous.after.text.len() + transaction.after.text.len());
            text.push_str(&previous.after.text);
            text.push_str(&transaction.after.text);
            previous.after.text = Arc::from(text);
            previous.after.span_len += transaction.after.span_len;
            previous.after.styles[0].range.end = previous.after.span_len;
            previous.after_typing_style = transaction.after_typing_style;
            previous.after_typing_override = transaction.after_typing_override;
            previous.after_selection = transaction.after_selection;
            previous.retained_bytes = transaction_bytes(&previous.before, &previous.after);
            self.trim_history();
            return;
        }
        self.typing_group_active = eligible;
        self.undo.push_back(transaction);
        self.trim_history();
    }

    fn apply_transaction(&mut self, transaction: &EditTransaction, redo: bool) {
        let (removed, inserted, typing_style, typing_override, selection) = if redo {
            (
                &transaction.before,
                &transaction.after,
                &transaction.after_typing_style,
                transaction.after_typing_override,
                transaction.after_selection,
            )
        } else {
            (
                &transaction.after,
                &transaction.before,
                &transaction.before_typing_style,
                transaction.before_typing_override,
                transaction.before_selection,
            )
        };
        let range = transaction.start..transaction.start + removed.span_len;
        debug_assert_eq!(
            inserted.styles.last().map_or(0, |run| run.range.end),
            inserted.span_len
        );
        self.replace_style_runs(range.start, range.end, &inserted.styles);
        let text_changed = !Arc::ptr_eq(&transaction.before.text, &transaction.after.text);
        if text_changed {
            debug_assert_eq!(removed.span_len, removed.text.len());
            debug_assert_eq!(inserted.span_len, inserted.text.len());
            self.text
                .replace_range(range.clone(), inserted.text.as_ref());
        } else {
            debug_assert_eq!(transaction.before.span_len, transaction.after.span_len);
        }
        self.typing_style = typing_style.clone();
        self.typing_override = typing_override;
        self.selection = selection;
        self.bump_revision();
        if text_changed {
            self.record_text_change(range.start, range.end, range.start + inserted.span_len);
        }
    }

    fn trim_history(&mut self) {
        while self.undo.len() + self.redo.len() > MAX_HISTORY
            || self.history_retained_bytes() > MAX_HISTORY_BYTES
                && self.undo.len() + self.redo.len() > 1
        {
            if !self.undo.is_empty() {
                self.undo.pop_front();
            } else {
                self.redo.pop_front();
            }
        }
    }

    fn history_retained_bytes(&self) -> usize {
        self.undo
            .iter()
            .chain(&self.redo)
            .map(|snapshot| snapshot.retained_bytes)
            .sum()
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}

fn style_at_runs(runs: &[ComposeStyleRun], byte: usize) -> Option<&CharacterStyle> {
    let index = runs.partition_point(|run| run.range.end <= byte);
    runs.get(index)
        .filter(|run| run.range.start <= byte)
        .map(|run| &run.style)
}

fn replaced_style_runs(
    old: &[ComposeStyleRun],
    start: usize,
    end: usize,
    inserted: &[ComposeStyleRun],
) -> Vec<ComposeStyleRun> {
    let inserted_len = inserted.last().map_or(0, |run| run.range.end);
    let prefix_end = old.partition_point(|run| run.range.start < start);
    let suffix_start = old.partition_point(|run| run.range.end <= end);
    let mut styles = Vec::with_capacity(prefix_end + inserted.len() + old.len() - suffix_start);
    for run in &old[..prefix_end] {
        if run.range.start < start {
            push_style_run(
                &mut styles,
                run.range.start..run.range.end.min(start),
                run.style.clone(),
            );
        }
    }
    for run in inserted {
        push_style_run(
            &mut styles,
            start + run.range.start..start + run.range.end,
            run.style.clone(),
        );
    }
    for run in &old[suffix_start..] {
        if run.range.end > end {
            push_style_run(
                &mut styles,
                start + inserted_len + run.range.start.max(end) - end
                    ..start + inserted_len + run.range.end - end,
                run.style.clone(),
            );
        }
    }
    styles
}

fn text_change_ranges(before: &str, after: &str) -> (usize, usize, usize) {
    let before_bytes = before.as_bytes();
    let after_bytes = after.as_bytes();
    let mut start = before_bytes
        .iter()
        .zip(after_bytes)
        .take_while(|(old, new)| old == new)
        .count();
    while !before.is_char_boundary(start) || !after.is_char_boundary(start) {
        start -= 1;
    }
    let mut suffix = 0;
    let limit = (before.len() - start).min(after.len() - start);
    while suffix < limit
        && before_bytes[before.len() - suffix - 1] == after_bytes[after.len() - suffix - 1]
    {
        suffix += 1;
    }
    while !before.is_char_boundary(before.len() - suffix)
        || !after.is_char_boundary(after.len() - suffix)
    {
        suffix -= 1;
    }
    (start, before.len() - suffix, after.len() - suffix)
}

fn changed_ranges(
    before_text: &str,
    before_styles: &[ComposeStyleRun],
    after_text: &str,
    after_styles: &[ComposeStyleRun],
) -> (usize, usize, usize) {
    let before_bytes = before_text.as_bytes();
    let after_bytes = after_text.as_bytes();
    let mut text_prefix = before_bytes
        .iter()
        .zip(after_bytes)
        .take_while(|(before, after)| before == after)
        .count();
    while !before_text.is_char_boundary(text_prefix) || !after_text.is_char_boundary(text_prefix) {
        text_prefix -= 1;
    }

    let mut start = 0;
    while start < text_prefix {
        let before_run =
            &before_styles[before_styles.partition_point(|run| run.range.end <= start)];
        let after_run = &after_styles[after_styles.partition_point(|run| run.range.end <= start)];
        if before_run.style != after_run.style {
            break;
        }
        start = text_prefix
            .min(before_run.range.end)
            .min(after_run.range.end);
    }

    let mut text_suffix = 0;
    let suffix_limit = (before_text.len() - start).min(after_text.len() - start);
    while text_suffix < suffix_limit
        && before_bytes[before_bytes.len() - text_suffix - 1]
            == after_bytes[after_bytes.len() - text_suffix - 1]
    {
        text_suffix += 1;
    }
    while !before_text.is_char_boundary(before_text.len() - text_suffix)
        || !after_text.is_char_boundary(after_text.len() - text_suffix)
    {
        text_suffix -= 1;
    }

    let mut style_suffix = 0;
    while style_suffix < text_suffix {
        let before_pos = before_text.len() - style_suffix - 1;
        let after_pos = after_text.len() - style_suffix - 1;
        let before_run =
            &before_styles[before_styles.partition_point(|run| run.range.end <= before_pos)];
        let after_run =
            &after_styles[after_styles.partition_point(|run| run.range.end <= after_pos)];
        if before_run.style != after_run.style {
            break;
        }
        let step = (text_suffix - style_suffix)
            .min(before_text.len() - style_suffix - before_run.range.start)
            .min(after_text.len() - style_suffix - after_run.range.start);
        style_suffix += step;
    }

    (
        start,
        before_text.len() - style_suffix,
        after_text.len() - style_suffix,
    )
}

fn edit_fragment(text: &str, styles: &[ComposeStyleRun], range: Range<usize>) -> EditFragment {
    EditFragment {
        text: Arc::from(&text[range.clone()]),
        span_len: range.end - range.start,
        styles: style_fragment(styles, range),
    }
}

fn style_fragment(styles: &[ComposeStyleRun], range: Range<usize>) -> Vec<ComposeStyleRun> {
    let first = styles.partition_point(|run| run.range.end <= range.start);
    let end = styles
        .partition_point(|run| run.range.start < range.end)
        .max(first);
    let mut fragment_styles = Vec::with_capacity(end - first);
    for run in &styles[first..end] {
        if run.range.start < range.end && run.range.end > range.start {
            push_style_run(
                &mut fragment_styles,
                run.range.start.max(range.start) - range.start
                    ..run.range.end.min(range.end) - range.start,
                run.style.clone(),
            );
        }
    }
    fragment_styles
}

fn fragment_text_bytes(text: &Arc<str>) -> usize {
    text.len() + 2 * std::mem::size_of::<usize>()
}

fn count_style_links(style: &CharacterStyle, seen: &mut HashSet<usize>, bytes: &mut usize) {
    if let Some(link) = &style.link
        && seen.insert(Arc::as_ptr(link) as *const u8 as usize)
    {
        *bytes += fragment_text_bytes(link);
    }
}

fn transaction_bytes(before: &EditFragment, after: &EditFragment) -> usize {
    let mut bytes = fragment_text_bytes(&before.text)
        + (before.styles.capacity() + after.styles.capacity())
            * std::mem::size_of::<ComposeStyleRun>();
    if !Arc::ptr_eq(&before.text, &after.text) {
        bytes += fragment_text_bytes(&after.text);
    }
    let mut seen_links = HashSet::new();
    for run in before.styles.iter().chain(&after.styles) {
        count_style_links(&run.style, &mut seen_links, &mut bytes);
    }
    bytes
}

fn is_ordinary_typing(transaction: &EditTransaction, key_text: &str) -> bool {
    let mut characters = key_text.chars();
    let Some(character) = characters.next() else {
        return false;
    };
    character.is_alphanumeric()
        && characters.next().is_none()
        && transaction.before_selection == (to_i32(transaction.start), to_i32(transaction.start))
        && transaction.before.span_len == 0
        && transaction.before.text.is_empty()
        && transaction.after.text.as_ref() == key_text
        && transaction.after.span_len == key_text.len()
        && transaction.after.styles.len() == 1
        && transaction.after.styles[0].range == (0..key_text.len())
        && transaction.after.styles[0].style == transaction.before_typing_style
        && transaction.after_typing_style == transaction.before_typing_style
        && transaction.after_typing_override == transaction.before_typing_override
        && transaction.after_selection
            == (
                to_i32(transaction.start + key_text.len()),
                to_i32(transaction.start + key_text.len()),
            )
}

fn can_group_typing(previous: &EditTransaction, next: &EditTransaction) -> bool {
    previous.before.span_len == 0
        && previous.after.styles.len() == 1
        && previous.after.span_len + next.after.span_len <= MAX_TYPING_GROUP_BYTES
        && previous.start + previous.after.span_len == next.start
        && previous.after.styles[0].style == next.after.styles[0].style
        && previous.after_selection == next.before_selection
        && previous.after_typing_style == next.before_typing_style
        && previous.after_typing_override == next.before_typing_override
}

fn push_style_run(runs: &mut Vec<ComposeStyleRun>, range: Range<usize>, style: CharacterStyle) {
    if range.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut() {
        debug_assert_eq!(last.range.end, range.start);
        if last.range.end == range.start && last.style == style {
            last.range.end = range.end;
            return;
        }
    }
    runs.push(ComposeStyleRun { range, style });
}

fn split_style_run_at(runs: &mut Vec<ComposeStyleRun>, byte: usize) {
    if let Some(index) = runs
        .iter()
        .position(|run| run.range.start < byte && byte < run.range.end)
    {
        let original_end = runs[index].range.end;
        let style = runs[index].style.clone();
        runs[index].range.end = byte;
        runs.insert(
            index + 1,
            ComposeStyleRun {
                range: byte..original_end,
                style,
            },
        );
    }
}

fn normalize_link(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with("https://") || value.starts_with("http://") || value.starts_with("mailto:")
    {
        Some(value.to_owned())
    } else {
        Some(format!("https://{value}"))
    }
}

/// Extends a block marker when the only edit is pressing Enter at the end of
/// a list/quote line. Enter on an empty item exits the block, matching common
/// mail editors.
fn continue_list_edit(old_text: &str, new_text: &str) -> Option<(String, i32)> {
    if new_text.len() != old_text.len() + 1 {
        return None;
    }
    let insertion = old_text
        .as_bytes()
        .iter()
        .zip(new_text.as_bytes())
        .take_while(|(old, new)| old == new)
        .count();
    if !old_text.is_char_boundary(insertion)
        || new_text.as_bytes().get(insertion) != Some(&b'\n')
        || old_text[insertion..] != new_text[insertion + 1..]
    {
        return None;
    }

    let line_start = old_text[..insertion].rfind('\n').map_or(0, |byte| byte + 1);
    let line = &old_text[line_start..insertion];
    let (kind, prefix_len) = block_prefix_text(line)?;
    let content_is_empty = line[prefix_len..].chars().all(char::is_whitespace);
    if content_is_empty {
        let mut text = String::with_capacity(new_text.len() - (insertion + 1 - line_start));
        text.push_str(&new_text[..line_start]);
        text.push_str(&new_text[insertion + 1..]);
        return Some((text, to_i32(line_start)));
    }

    let number = if kind == BlockKind::Number {
        line[..prefix_len.saturating_sub(2)]
            .parse::<usize>()
            .unwrap_or(1)
            .saturating_add(1)
    } else {
        1
    };
    let marker_len = block_marker_len(kind, number);
    let mut text = String::with_capacity(new_text.len() + marker_len);
    text.push_str(&new_text[..insertion + 1]);
    append_block_marker(&mut text, kind, number);
    text.push_str(&new_text[insertion + 1..]);
    Some((text, to_i32(insertion + 1 + marker_len)))
}

fn block_marker_len(kind: BlockKind, number: usize) -> usize {
    match kind {
        BlockKind::Bullet => "• ".len(),
        BlockKind::Quote => QUOTE_MARKER.len(),
        BlockKind::Number => number.checked_ilog10().unwrap_or(0) as usize + 3,
    }
}

fn append_block_marker(text: &mut String, kind: BlockKind, number: usize) {
    match kind {
        BlockKind::Bullet => text.push_str("• "),
        BlockKind::Quote => text.push_str(QUOTE_MARKER),
        BlockKind::Number => {
            use std::fmt::Write;
            write!(text, "{number}. ").expect("writing to a String cannot fail");
        }
    }
}

// Prefix lengths are UTF-8 byte offsets into the borrowed line.
fn block_prefix_text(text: &str) -> Option<(BlockKind, usize)> {
    if text.starts_with("• ") {
        return Some((BlockKind::Bullet, "• ".len()));
    }
    if text.starts_with(QUOTE_MARKER) {
        return Some((BlockKind::Quote, QUOTE_MARKER.len()));
    }
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && text.as_bytes().get(digits..digits + 2) == Some(b". ") {
        return Some((BlockKind::Number, digits + 2));
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineBlock {
    Bullet,
    Number(u32),
    Quote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenHtmlBlock {
    Bullets,
    Numbers { next: u64 },
    Quote,
}

/// A numbered line stays a paragraph when an HTML list would show another
/// number: one written with leading zeros, or past what lists count to.
fn html_line_block(line: &str) -> Option<(LineBlock, usize)> {
    let (kind, bytes) = block_prefix_text(line)?;
    let block = match kind {
        BlockKind::Bullet => LineBlock::Bullet,
        BlockKind::Quote => LineBlock::Quote,
        BlockKind::Number => {
            let digits = &line[..bytes - 2];
            if digits.len() > 1 && digits.starts_with('0') {
                return None;
            }
            let number = digits.parse::<i32>().ok()?;
            LineBlock::Number(u32::try_from(number).ok()?)
        }
    };
    Some((block, bytes))
}

fn close_html_block(html: &mut String, open: &mut Option<OpenHtmlBlock>) {
    match open.take() {
        Some(OpenHtmlBlock::Bullets) => html.push_str("</ul>"),
        Some(OpenHtmlBlock::Numbers { .. }) => html.push_str("</ol>"),
        Some(OpenHtmlBlock::Quote) => html.push_str("</blockquote>"),
        None => {}
    }
}

/// A centered or right-aligned paragraph is written with both the `align` attribute and
/// `text-align`, as Outlook writes it: readers that drop one keep the other.
fn append_line_html(
    output: &mut String,
    text: &str,
    styles: &[ComposeStyleRun],
    range: Range<usize>,
    align: Alignment,
) {
    match align {
        Alignment::Left => output.push_str("<div>"),
        align => {
            output.push_str("<div align=\"");
            output.push_str(align.name());
            output.push_str("\" style=\"text-align:");
            output.push_str(align.name());
            output.push_str("\">");
        }
    }
    if range.is_empty() {
        output.push_str("<br>");
    } else {
        append_inline_html(output, text, styles, range);
    }
    output.push_str("</div>");
}

/// An image as `<img>`, with its file in a `data:` URI, which goes out as an inline part. Its
/// size goes in its attributes and in CSS, as some readers take only one, and it shrinks to fit
/// narrower windows.
fn append_image_html(output: &mut String, image: &InlineImage) {
    use base64::Engine;
    use std::fmt::Write;
    let (width, height) = image.shown_size();
    output.push_str("<img src=\"data:");
    output.push_str(image.mime_type);
    output.push_str(";base64,");
    base64::engine::general_purpose::STANDARD.encode_string(&image.data, output);
    output.push_str("\" alt=\"");
    for character in image.alt.chars() {
        append_html_char(output, character);
    }
    let _ = write!(
        output,
        "\" width=\"{width}\" height=\"{height}\" style=\"{}\">",
        image_css(width)
    );
}

/// The CSS for an image shown `width` pixels across.
fn image_css(width: u32) -> String {
    format!("width:{width}px;max-width:100%;height:auto")
}

fn append_list_item_start(output: &mut String, align: Alignment) {
    match align {
        Alignment::Left => output.push_str("<li>"),
        align => {
            output.push_str("<li style=\"text-align:");
            output.push_str(align.name());
            output.push_str("\">");
        }
    }
}

/// The CSS for a run's color, highlight, size and font, if it has any: what goes in the
/// `style` of the span around it. Colors are `#rrggbb`, which every reader takes.
fn run_css(style: &CharacterStyle) -> Option<String> {
    let mut css = String::new();
    let mut declare = |property: &str, value: &str| {
        if !css.is_empty() {
            css.push(';');
        }
        css.push_str(property);
        css.push(':');
        css.push_str(value);
    };
    if let Some(color) = style.color {
        declare("color", &css_rgb(color));
    }
    if let Some(highlight) = style.highlight {
        declare("background-color", &css_rgb(highlight));
    }
    if let Some(size) = style.size.css() {
        declare("font-size", size);
    }
    if let Some(family) = style.family.css() {
        declare("font-family", family);
    }
    (!css.is_empty()).then_some(css)
}

fn append_inline_html(
    output: &mut String,
    text: &str,
    styles: &[ComposeStyleRun],
    range: Range<usize>,
) {
    let first = styles.partition_point(|run| run.range.end <= range.start);
    // HTML collapses repeated spaces and drops them at either end of a line,
    // so those spaces are written as no-break spaces.
    let mut after_space = true;
    for run in styles[first..]
        .iter()
        .take_while(|run| run.range.start < range.end)
    {
        let start = run.range.start.max(range.start);
        let end = run.range.end.min(range.end);
        append_html_run(
            output,
            text,
            start..end,
            range.end,
            &run.style,
            &mut after_space,
        );
    }
}

fn append_html_run(
    output: &mut String,
    text: &str,
    range: Range<usize>,
    line_end: usize,
    style: &CharacterStyle,
    after_space: &mut bool,
) {
    if range.is_empty() {
        return;
    }
    if let Some(url) = style.link.as_deref() {
        output.push_str("<a href=\"");
        for character in url.chars() {
            append_html_char(output, character);
        }
        output.push_str("\">");
    }
    if let Some(image) = &style.image {
        for character in text[range].chars() {
            if character == IMAGE_PLACEHOLDER {
                append_image_html(output, image);
            } else {
                append_html_char(output, character);
            }
        }
        if style.link.is_some() {
            output.push_str("</a>");
        }
        *after_space = false;
        return;
    }
    let css = run_css(style);
    if let Some(css) = &css {
        output.push_str("<span style=\"");
        output.push_str(css);
        output.push_str("\">");
    }
    let tags = [
        (BOLD, "strong"),
        (ITALIC, "em"),
        (STRIKE, "s"),
        (UNDERLINE, "u"),
        (CODE, "code"),
    ];
    for (mark, tag) in tags {
        if style.marks & mark != 0 {
            output.push('<');
            output.push_str(tag);
            output.push('>');
        }
    }
    for (offset, character) in text[range.clone()].char_indices() {
        match character {
            ' ' if *after_space || range.start + offset + 1 == line_end => {
                output.push_str("&nbsp;");
            }
            '\t' => output.push_str("&nbsp;&nbsp;&nbsp;&nbsp;"),
            _ => append_html_char(output, character),
        }
        *after_space = matches!(character, ' ' | '\t');
    }
    for (mark, tag) in tags.into_iter().rev() {
        if style.marks & mark != 0 {
            output.push_str("</");
            output.push_str(tag);
            output.push('>');
        }
    }
    if css.is_some() {
        output.push_str("</span>");
    }
    if style.link.is_some() {
        output.push_str("</a>");
    }
}

fn append_html_char(output: &mut String, character: char) {
    match character {
        '&' => output.push_str("&amp;"),
        '<' => output.push_str("&lt;"),
        '>' => output.push_str("&gt;"),
        '"' => output.push_str("&quot;"),
        '\'' => output.push_str("&#39;"),
        _ => output.push(character),
    }
}

enum HtmlToken {
    Start {
        name: String,
        href: Option<String>,
        start: Option<String>,
        style: Option<String>,
        align: Option<String>,
        src: Option<String>,
        alt: Option<String>,
        width: Option<String>,
    },
    End(String),
    Text(String),
}

struct HtmlTokenSink(RefCell<Vec<HtmlToken>>);

impl TokenSink for HtmlTokenSink {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let token = match token {
            Token::TagToken(tag) if tag.kind == TagKind::EndTag => {
                HtmlToken::End(tag.name.as_str().to_owned())
            }
            Token::TagToken(tag) => {
                let attribute = |wanted: &str| {
                    tag.attrs
                        .iter()
                        .find(|attribute| attribute.name.local.as_str() == wanted)
                        .map(|attribute| attribute.value.to_string())
                };
                HtmlToken::Start {
                    name: tag.name.as_str().to_owned(),
                    href: attribute("href"),
                    start: attribute("start"),
                    style: attribute("style"),
                    align: attribute("align"),
                    src: attribute("src"),
                    alt: attribute("alt"),
                    width: attribute("width"),
                }
            }
            Token::CharacterTokens(text) => HtmlToken::Text(text.to_string()),
            _ => return TokenSinkResult::Continue,
        };
        self.0.borrow_mut().push(token);
        TokenSinkResult::Continue
    }
}

const IMPORTED_MARKS: [u8; 5] = [BOLD, ITALIC, UNDERLINE, STRIKE, CODE];

fn imported_mark(tag: &str) -> Option<usize> {
    Some(match tag {
        "strong" | "b" => 0,
        "em" | "i" => 1,
        "u" => 2,
        "s" | "strike" | "del" => 3,
        "code" => 4,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
struct ImportedList {
    ordered: bool,
    next: u64,
}

/// Rebuilds editable text from the markup `body_html` writes. It also reads
/// drafts saved before quoted lines were grouped, and the copy a mail server
/// returns after sanitizing (an html/body wrapper, `rel` on links).
#[derive(Default)]
struct HtmlImport {
    text: String,
    styles: Vec<ComposeStyleRun>,
    marks: [u32; 5],
    links: Vec<Option<Arc<str>>>,
    /// The styles of the spans the text is in, innermost last: none for a span without one.
    spans: Vec<Option<SpanStyle>>,
    list: Option<ImportedList>,
    in_item: bool,
    in_quote: bool,
    lines: usize,
    line_open: bool,
    line_has_text: bool,
    pending_space: Option<CharacterStyle>,
    /// The alignment of the paragraph being read, and of each line read so far.
    align: Alignment,
    line_aligns: Vec<Alignment>,
}

/// The color, highlight, size and font a span gives the text in it.
#[derive(Clone, Copy, Default)]
struct SpanStyle {
    color: Option<Rgb>,
    highlight: Option<Rgb>,
    size: Option<TextSize>,
    family: Option<FontFamily>,
}

impl SpanStyle {
    /// The style `run_css` writes, or `None` for any other CSS.
    fn from_saved_css(css: &str) -> Option<Self> {
        let mut span = Self::default();
        for declaration in css.split(';').filter(|part| !part.trim().is_empty()) {
            let (property, value) = declaration.split_once(':')?;
            let value = value.trim();
            match property.trim() {
                "color" => span.color = Some(parse_rgb(value)?),
                "background-color" => span.highlight = Some(parse_rgb(value)?),
                "font-size" => span.size = Some(TextSize::from_css(value)?),
                "font-family" => span.family = Some(FontFamily::from_css(value)?),
                _ => return None,
            }
        }
        Some(span)
    }
}

/// The alignment `append_line_html` writes for a paragraph, from its `align` attribute and
/// `style`, or `None` for anything else.
fn saved_alignment(align: Option<&str>, style: Option<&str>) -> Option<Alignment> {
    let from_style = match style.map(str::trim) {
        None => None,
        Some(style) => {
            let value = style.strip_prefix("text-align:")?.trim();
            Some(Alignment::from_name(value).filter(|align| *align != Alignment::Left)?)
        }
    };
    let from_attribute = match align {
        None => None,
        Some(value) => Some(Alignment::from_name(value.trim())?),
    };
    match (from_attribute, from_style) {
        (Some(attribute), Some(style)) if attribute != style => None,
        (attribute, style) => Some(style.or(attribute).unwrap_or_default()),
    }
}

impl HtmlImport {
    fn style(&self) -> CharacterStyle {
        let marks = IMPORTED_MARKS
            .into_iter()
            .zip(self.marks)
            .filter(|(_, depth)| *depth > 0)
            .fold(0, |marks, (mark, _)| marks | mark);
        let span = |value: fn(&SpanStyle) -> bool| {
            self.spans.iter().rev().flatten().find(|span| value(span))
        };
        CharacterStyle {
            marks,
            link: self.links.last().cloned().flatten(),
            link_is_auto: false,
            color: span(|span| span.color.is_some()).and_then(|span| span.color),
            highlight: span(|span| span.highlight.is_some()).and_then(|span| span.highlight),
            size: span(|span| span.size.is_some())
                .and_then(|span| span.size)
                .unwrap_or_default(),
            family: span(|span| span.family.is_some())
                .and_then(|span| span.family)
                .unwrap_or_default(),
            align: Alignment::default(),
            image: None,
        }
    }

    /// An image as `append_image_html` writes it.
    fn image(
        &mut self,
        src: &str,
        alt: Option<&str>,
        width: Option<&str>,
        style: Option<&str>,
    ) -> Option<()> {
        let mut image = InlineImage::from_data_uri(src, alt.unwrap_or_default())?;
        let shown_width = match width {
            Some(width) => width
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|width| *width > 0)?,
            None => image.width,
        };
        if style.is_some_and(|style| style.trim() != image_css(shown_width)) {
            return None;
        }
        image.shown_width = (shown_width != image.width).then_some(shown_width);
        if !self.line_open {
            self.start_line();
        }
        if let Some(space) = self.pending_space.take() {
            self.push(" ", space);
        }
        let style = CharacterStyle {
            image: Some(Arc::new(image)),
            ..self.style()
        };
        self.push(IMAGE_PLACEHOLDER.encode_utf8(&mut [0; 4]), style);
        self.line_has_text = true;
        Some(())
    }

    fn push(&mut self, text: &str, style: CharacterStyle) {
        let start = self.text.len();
        self.text.push_str(text);
        push_style_run(&mut self.styles, start..self.text.len(), style);
    }

    fn start_line(&mut self) {
        if self.lines > 0 {
            self.push("\n", CharacterStyle::default());
        }
        self.lines += 1;
        self.line_aligns.push(self.align);
        self.line_open = true;
        self.line_has_text = false;
        self.pending_space = None;
        if self.in_quote {
            self.push(QUOTE_MARKER, CharacterStyle::default());
        }
    }

    fn end_line(&mut self) {
        self.line_open = false;
        self.pending_space = None;
    }

    fn characters(&mut self, text: &str) {
        for character in text.chars() {
            // Collapsible white space, as HTML renders it: one space between
            // words and none at either end of a line.
            if matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                if self.line_open && self.line_has_text && self.pending_space.is_none() {
                    self.pending_space = Some(self.style());
                }
                continue;
            }
            if !self.line_open {
                self.start_line();
            }
            if let Some(style) = self.pending_space.take() {
                self.push(" ", style);
            }
            let character = if character == '\u{a0}' {
                ' '
            } else {
                character
            };
            let style = self.style();
            self.push(character.encode_utf8(&mut [0; 4]), style);
            self.line_has_text = true;
        }
    }

    fn token(&mut self, token: HtmlToken) -> Option<()> {
        match token {
            HtmlToken::Text(text) => self.characters(&text),
            HtmlToken::Start {
                name,
                src,
                alt,
                width,
                style,
                align,
                ..
            } if name == "img" => {
                if align.is_some() {
                    return None;
                }
                self.image(&src?, alt.as_deref(), width.as_deref(), style.as_deref())?;
            }
            HtmlToken::Start {
                name, style, align, ..
            } if (style.is_some() || align.is_some())
                && !matches!(name.as_str(), "span" | "div" | "li") =>
            {
                return None;
            }
            HtmlToken::Start {
                name,
                href,
                start,
                style,
                align,
                ..
            } => match name.as_str() {
                "html" | "head" | "body" | "meta" => {}
                "span" => {
                    let span = match style {
                        Some(css) => Some(SpanStyle::from_saved_css(&css)?),
                        None => None,
                    };
                    self.spans.push(span);
                }
                "div" if !self.in_item => {
                    self.end_line();
                    self.align = saved_alignment(align.as_deref(), style.as_deref())?;
                }
                "div" if style.is_some() || align.is_some() => return None,
                "br" if !self.in_item => {
                    if !self.line_open {
                        self.start_line();
                    }
                    self.end_line();
                }
                "ul" | "ol" if self.list.is_none() && !self.in_quote => {
                    self.end_line();
                    self.list = Some(ImportedList {
                        ordered: name == "ol",
                        next: start
                            .and_then(|start| start.trim().parse().ok())
                            .unwrap_or(1),
                    });
                }
                "li" => {
                    let list = self.list?;
                    if align.is_some() {
                        return None;
                    }
                    self.end_line();
                    self.align = saved_alignment(None, style.as_deref())?;
                    self.start_line();
                    if list.ordered {
                        self.push(&format!("{}. ", list.next), CharacterStyle::default());
                        self.list = Some(ImportedList {
                            next: list.next.saturating_add(1),
                            ..list
                        });
                    } else {
                        self.push("• ", CharacterStyle::default());
                    }
                    self.in_item = true;
                }
                "blockquote" if !self.in_quote && self.list.is_none() => {
                    self.end_line();
                    self.in_quote = true;
                }
                "a" => {
                    let link = href
                        .as_deref()
                        .map(str::trim)
                        .filter(|href| {
                            href.starts_with("https://")
                                || href.starts_with("http://")
                                || href.starts_with("mailto:")
                        })
                        .map(Arc::<str>::from);
                    self.links.push(link);
                }
                tag => self.marks[imported_mark(tag)?] += 1,
            },
            HtmlToken::End(name) => match name.as_str() {
                "html" | "head" | "body" | "meta" | "br" => {}
                "span" => {
                    self.spans.pop();
                }
                "div" => {
                    self.end_line();
                    self.align = Alignment::default();
                }
                "li" => {
                    self.in_item = false;
                    self.end_line();
                    self.align = Alignment::default();
                }
                "ul" | "ol" => {
                    self.list = None;
                    self.in_item = false;
                    self.end_line();
                }
                "blockquote" => {
                    self.in_quote = false;
                    self.end_line();
                }
                "a" => {
                    self.links.pop();
                }
                tag => {
                    let depth = &mut self.marks[imported_mark(tag)?];
                    *depth = depth.saturating_sub(1);
                }
            },
        }
        Some(())
    }

    fn finish(mut self) -> Option<(String, Vec<ComposeStyleRun>)> {
        if self.lines == 0 {
            return None;
        }
        // Each line, with the line end after it, takes its paragraph's alignment.
        let mut line_start = 0;
        for align in std::mem::take(&mut self.line_aligns) {
            let line_end = self.text[line_start..]
                .find('\n')
                .map_or(self.text.len(), |index| line_start + index + 1);
            if align != Alignment::default() {
                split_style_run_at(&mut self.styles, line_start);
                split_style_run_at(&mut self.styles, line_end);
                for run in &mut self.styles {
                    if run.range.start >= line_start && run.range.end <= line_end {
                        run.style.align = align;
                    }
                }
            }
            line_start = line_end;
        }
        self.styles.dedup_by(|next, previous| {
            if previous.range.end == next.range.start && previous.style == next.style {
                previous.range.end = next.range.end;
                true
            } else {
                false
            }
        });
        // A link written over its own address was found by `auto_link_urls`
        // where it would link that address again; anywhere else the user
        // set it by hand.
        let typed = typed_urls(&self.text).collect::<Vec<_>>();
        let mut index = 0;
        while let Some(run) = self.styles.get(index) {
            let Some(link) = run.style.link.clone() else {
                index += 1;
                continue;
            };
            let start = run.range.start;
            let end_index = index
                + self.styles[index..]
                    .iter()
                    .take_while(|run| run.style.link.as_ref() == Some(&link))
                    .count();
            let end = self.styles[end_index - 1].range.end;
            if typed.contains(&(start..end)) && self.text[start..end] == *link {
                for run in &mut self.styles[index..end_index] {
                    run.style.link_is_auto = true;
                }
            }
            index = end_index;
        }
        Some((self.text, self.styles))
    }
}

/// A signature's text and formatting: the composer's own HTML as it wrote it, and other HTML
/// with the formatting pasting keeps, without the blank space around it. `None` when it holds
/// no text.
fn signature_runs(html: &str, base: &CharacterStyle) -> Option<(String, Vec<ComposeStyleRun>)> {
    import_body_html(html)
        .or_else(|| crate::compose_paste::styled_text(html, base))
        .map(|(text, runs)| trimmed_runs(text, runs))
        .filter(|(text, _)| !text.is_empty())
}

/// Whether text in `style` reads the same as plain text in `base`: no marks, colors, other
/// fonts, alignment, pictures, or links but those typed as addresses.
fn is_plain_style(style: &CharacterStyle, base: &CharacterStyle) -> bool {
    style.marks == 0
        && style.color.is_none()
        && style.highlight.is_none()
        && style.size == base.size
        && style.family == base.family
        && style.align == Alignment::Left
        && style.image.is_none()
        && (style.link.is_none() || style.link_is_auto)
}

/// `text`, styled by `runs`, as plain text: each link followed by its address in angle brackets
/// where its text doesn't show it, as mail apps write links in plain text, and each picture as
/// its description, or as its alt text alone when `described` is false. Also where `offset` in
/// `text` lands in it.
fn plain_text_of(
    text: &str,
    runs: &[ComposeStyleRun],
    described: bool,
    offset: usize,
) -> (String, usize) {
    let mut plain = String::with_capacity(text.len());
    let mut mapped = None;
    let mut copy = |plain: &mut String, range: Range<usize>| {
        if mapped.is_none() && (range.start..=range.end).contains(&offset) {
            mapped = Some(plain.len() + offset - range.start);
        }
        plain.push_str(&text[range]);
    };
    let mut at = 0;
    // Where the link being written started in `text`.
    let mut link_start = 0;
    for (index, run) in runs.iter().enumerate() {
        let range = run.range.start.max(at)..run.range.end.min(text.len());
        if range.start > at {
            copy(&mut plain, at..range.start);
        }
        if range.is_empty() {
            continue;
        }
        let continues_link = index > 0
            && runs[index - 1].range.end == range.start
            && runs[index - 1].style.link.is_some()
            && runs[index - 1].style.link == run.style.link;
        if !continues_link {
            link_start = range.start;
        }
        match &run.style.image {
            Some(image) => {
                for (local, character) in text[range.clone()].char_indices() {
                    if character == IMAGE_PLACEHOLDER {
                        copy(&mut plain, range.start + local..range.start + local);
                        if described {
                            plain.push_str(&image.description());
                        } else {
                            plain.push_str(image.alt.trim());
                        }
                    } else {
                        let start = range.start + local;
                        copy(&mut plain, start..start + character.len_utf8());
                    }
                }
            }
            None => copy(&mut plain, range.clone()),
        }
        at = range.end;
        // A link's address follows its last run.
        if let Some(url) = &run.style.link {
            let ends_link = runs.get(index + 1).is_none_or(|next| {
                next.range.start != range.end || next.style.link.as_ref() != Some(url)
            });
            if ends_link && !shows_address(&text[link_start..range.end], url) {
                plain.push_str(" <");
                plain.push_str(url);
                plain.push('>');
            }
        }
    }
    if at < text.len() {
        copy(&mut plain, at..text.len());
    }
    let mapped = mapped.unwrap_or(plain.len());
    (plain, mapped)
}

/// Whether a link's `text` already shows its `address`, as a typed address does.
fn shows_address(text: &str, address: &str) -> bool {
    fn bare(value: &str) -> &str {
        let value = value.trim();
        let value = ["mailto:", "https://", "http://"]
            .iter()
            .find_map(|scheme| {
                value
                    .get(..scheme.len())
                    .filter(|start| start.eq_ignore_ascii_case(scheme))
                    .map(|_| &value[scheme.len()..])
            })
            .unwrap_or(value);
        value.strip_suffix('/').unwrap_or(value)
    }
    bare(text).eq_ignore_ascii_case(bare(address))
}

/// What a signature's HTML reads as, pictures named in their places, for its preview.
pub(crate) fn signature_preview(html: &str) -> String {
    let mut document = RichComposeDocument::default();
    document.load_signature_html(html);
    document.text_with_image_descriptions()
}

/// `text` without the blank space around it, and `runs` cut to what's left.
fn trimmed_runs(text: String, runs: Vec<ComposeStyleRun>) -> (String, Vec<ComposeStyleRun>) {
    let start = text.len() - text.trim_start().len();
    let end = text.trim_end().len().max(start);
    let runs = runs
        .into_iter()
        .filter_map(|run| {
            let range =
                run.range.start.clamp(start, end) - start..run.range.end.clamp(start, end) - start;
            (!range.is_empty()).then_some(ComposeStyleRun {
                range,
                style: run.style,
            })
        })
        .collect();
    (text[start..end].to_owned(), runs)
}

fn import_body_html(html: &str) -> Option<(String, Vec<ComposeStyleRun>)> {
    let tokenizer = Tokenizer::new(HtmlTokenSink(RefCell::default()), Default::default());
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let mut import = HtmlImport::default();
    for token in tokenizer.sink.0.into_inner() {
        import.token(token)?;
    }
    import.finish()
}

/// Where `text` holds web addresses that link themselves: whole words that
/// start with http:// or https://, less the punctuation that follows them.
fn typed_urls(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut next = 0;
    std::iter::from_fn(move || {
        while next < text.len() {
            let start = next + text[next..].find(|character: char| !character.is_whitespace())?;
            let end = text[start..]
                .find(char::is_whitespace)
                .map_or(text.len(), |relative| start + relative);
            next = end;
            let token =
                text[start..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
            if token.starts_with("https://") || token.starts_with("http://") {
                return Some(start..start + token.len());
            }
        }
        None
    })
}

/// The formatting `html` gives `text`, when the HTML holds that text, all
/// of it when `whole` or else a start of it that ends at a line break, and
/// where in `text` that ends. HTML keeps a tab only as a run of spaces, a
/// no-break space as a plain one and a CRLF line end as LF, so those
/// differences still match; any other means the HTML belongs to other text.
fn saved_styles(html: &str, text: &str, whole: bool) -> Option<(usize, Vec<ComposeStyleRun>)> {
    let (imported, styles) = import_body_html(html)?;
    let (expected, offsets) = text_as_html_reads_it(text);
    let rest = expected.strip_prefix(imported.as_str())?;
    if !(rest.is_empty() || !whole && rest.starts_with('\n')) {
        return None;
    }
    let styles = styles
        .into_iter()
        .map(|run| ComposeStyleRun {
            range: offsets[run.range.start]..offsets[run.range.end],
            style: run.style,
        })
        .filter(|run| !run.range.is_empty())
        .collect();
    Some((offsets[imported.len()], styles))
}

/// `text` as its HTML imports back, with the byte offset in `text` where
/// each byte of the result comes from, plus one for the end.
fn text_as_html_reads_it(text: &str) -> (String, Vec<usize>) {
    let mut read = String::with_capacity(text.len());
    let mut offsets = Vec::with_capacity(text.len() + 1);
    let mut characters = text.char_indices().peekable();
    while let Some((offset, character)) = characters.next() {
        match character {
            '\t' => read.push_str("    "),
            '\u{a0}' => read.push(' '),
            // The LF of a CRLF stands at the CR, so formatting that ends
            // with the line leaves the CR out.
            '\r' if matches!(characters.peek(), Some((_, '\n'))) => {
                characters.next();
                read.push('\n');
            }
            other => read.push(other),
        }
        offsets.resize(read.len(), offset);
    }
    offsets.push(text.len());
    (read, offsets)
}

fn ordered(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

fn valid_offset(text: &str, offset: i32) -> usize {
    let mut offset = usize::try_from(offset).unwrap_or_default().min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    #[test]
    fn loaded_text_is_where_undo_stops() {
        let mut document = RichComposeDocument::default();
        document.load("Quoted reply", 0, 0);
        assert_eq!(document.history("undo"), None);
        assert_eq!(document.text(), "Quoted reply");
        document.synchronize_key_edit("Hi\nQuoted reply", 3, 3, "\n");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "Quoted reply");
        assert_eq!(document.history("undo"), None);
    }

    use super::*;

    fn assert_style_coverage(document: &RichComposeDocument) {
        let mut end = 0;
        for run in &document.styles {
            assert_eq!(run.range.start, end);
            assert!(run.range.end > run.range.start);
            assert!(document.text.is_char_boundary(run.range.start));
            assert!(document.text.is_char_boundary(run.range.end));
            end = run.range.end;
        }
        assert_eq!(end, document.text.len());
        assert!(
            document
                .styles
                .windows(2)
                .all(|pair| pair[0].style != pair[1].style)
        );
    }

    fn assert_fixture_state(
        document: &RichComposeDocument,
        text: &str,
        html: &str,
        selection: (i32, i32),
        runs: &[(Range<usize>, u8, Option<&str>)],
    ) {
        assert_eq!(document.text(), text);
        assert_eq!(document.body_html().as_deref(), Some(html));
        assert_eq!(document.selection, selection);
        assert_eq!(document.styles.len(), runs.len());
        for (actual, expected) in document.styles.iter().zip(runs) {
            assert_eq!(actual.range, expected.0);
            assert_eq!(actual.style.marks, expected.1);
            assert_eq!(actual.style.link.as_deref(), expected.2);
        }
        assert_style_coverage(document);
    }

    #[test]
    fn html_export_preserves_all_marks_and_escaped_unicode() {
        let text = "<&\"'é👩‍🚀>";
        let mut document = RichComposeDocument::default();
        document.synchronize(text, 0, text.len() as i32);
        for mark in ["bold", "italic", "underline", "strike", "code"] {
            document.format(mark, text, 0, text.len() as i32);
        }
        document.set_link("https://example.org/?a=\"&b='", text, 0, text.len() as i32);
        assert_eq!(
            document.body_html().unwrap(),
            "<div><a href=\"https://example.org/?a=&quot;&amp;b=&#39;\"><strong><em><s><u><code>&lt;&amp;&quot;&#39;é👩‍🚀&gt;</code></u></s></em></strong></a></div>"
        );
    }

    #[test]
    fn normalizing_styles_reuses_vector_storage() {
        let mut document = RichComposeDocument {
            text: "é界xy".into(),
            styles: Vec::with_capacity(20),
            ..RichComposeDocument::default()
        };
        for range in [0..2, 2..5, 5..6, 6..7] {
            document.styles.push(ComposeStyleRun {
                range,
                style: CharacterStyle::default(),
            });
        }
        let pointer = document.styles.as_ptr();
        let capacity = document.styles.capacity();
        document.merge_styles();
        assert_eq!(document.styles.as_ptr(), pointer);
        assert_eq!(document.styles.capacity(), capacity);
        assert_eq!(document.styles.len(), 1);
        assert_eq!(document.styles[0].range, 0..7);
        assert_style_coverage(&document);
    }

    #[test]
    fn selected_style_fragment_allocates_only_intersecting_runs() {
        let styles = (0..1000)
            .map(|byte| ComposeStyleRun {
                range: byte..byte + 1,
                style: CharacterStyle {
                    marks: if byte % 2 == 0 { BOLD } else { ITALIC },
                    ..Default::default()
                },
            })
            .collect::<Vec<_>>();
        let fragment = style_fragment(&styles, 900..903);
        assert_eq!(fragment.len(), 3);
        assert_eq!(fragment.capacity(), 3);
        assert_eq!(fragment[0].range, 0..1);
        assert_eq!(fragment[2].range, 2..3);
        assert!(style_fragment(&styles, 900..900).is_empty());
    }

    #[test]
    fn replacing_fragmented_styles_does_not_retain_removed_capacity() {
        let old = (0..1000)
            .map(|byte| ComposeStyleRun {
                range: byte..byte + 1,
                style: CharacterStyle {
                    marks: if byte % 2 == 0 { BOLD } else { ITALIC },
                    ..Default::default()
                },
            })
            .collect::<Vec<_>>();
        let inserted = [ComposeStyleRun {
            range: 0..1,
            style: CharacterStyle::default(),
        }];
        let result = replaced_style_runs(&old, 0, 1000, &inserted);
        assert_eq!(result.len(), 1);
        assert_eq!(result.capacity(), 1);
        assert_eq!(result[0].range, 0..1);
        let empty = replaced_style_runs(&old, 0, 1000, &[]);
        assert_eq!(empty.capacity(), 0);
    }

    #[test]
    fn inline_formatting_never_changes_editable_text() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello world", 0, 5);
        let selection = document.format("bold", "Hello world", 0, 5);
        assert_eq!(document.text(), "Hello world");
        assert_eq!(selection, ComposeSelection { start: 0, end: 5 });
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><strong>Hello</strong> world</div>")
        );
        assert!(document.active_marks().bold);
    }

    #[test]
    fn typing_inside_a_mark_inherits_that_mark() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello", 0, 5);
        document.format("italic", "Hello", 0, 5);
        document.update_selection(5, 5);
        document.synchronize("Hello!", 6, 6);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><em>Hello!</em></div>")
        );
    }

    #[test]
    fn inserted_template_replaces_the_selection_and_can_be_undone() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello placeholder", 6, 17);
        let selection = document.insert_text("there");
        assert_eq!(document.text(), "Hello there");
        assert_eq!(selection, ComposeSelection { start: 11, end: 11 });

        document.history("undo");
        assert_eq!(document.text(), "Hello placeholder");
    }

    #[test]
    fn collapsed_toolbar_mark_survives_the_focus_selection_callback() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello ", 6, 6);
        document.format("bold", "Hello ", 6, 6);
        document.update_selection(6, 6);
        document.synchronize("Hello W", 7, 7);

        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>Hello <strong>W</strong></div>")
        );
    }

    #[test]
    fn long_unicode_block_edits_keep_compact_styles_and_undo() {
        let text = format!("{}\nsecond é👩‍🚀\n", "界".repeat(350_000));
        let start = text.find("second").unwrap() as i32;
        let end = (text.len() - 1) as i32;
        let mut document = RichComposeDocument::default();
        document.synchronize(&text, start, end);
        document.format("bold", &text, start, end);
        let html_before = document.body_html();
        document.format("bullet", &text, start, end);
        assert!(document.text().contains("\n• second é👩‍🚀\n"));
        assert!(document.style_runs().len() <= 5);
        assert!(
            document
                .body_html()
                .unwrap()
                .contains("<li><strong>second é👩‍🚀</strong></li>")
        );
        document.history("undo").unwrap();
        assert_eq!(document.text(), text);
        assert_eq!(document.body_html(), html_before);
        document.history("redo").unwrap();
        assert!(document.text().contains("\n• second é👩‍🚀\n"));
        assert_style_coverage(&document);
    }

    #[test]
    fn block_controls_create_semantic_email_html() {
        let mut document = RichComposeDocument::default();
        document.synchronize("One\nTwo", 0, 7);
        document.format("bullet", "One\nTwo", 0, 7);
        assert_eq!(document.text(), "• One\n• Two");
        assert_eq!(
            document.body_html().as_deref(),
            Some("<ul><li>One</li><li>Two</li></ul>")
        );
    }

    #[test]
    fn enter_continues_bulleted_and_numbered_lists() {
        let mut document = RichComposeDocument::default();
        document.synchronize("• First", 9, 9);
        let bullet = document.synchronize("• First\n", 10, 10);
        assert_eq!(document.text(), "• First\n• ");
        assert_eq!(bullet, ComposeSelection { start: 14, end: 14 });

        document.reset();
        document.synchronize("3. Third", 8, 8);
        let numbered = document.synchronize("3. Third\n", 9, 9);
        assert_eq!(document.text(), "3. Third\n4. ");
        assert_eq!(numbered, ComposeSelection { start: 12, end: 12 });
    }

    #[test]
    fn long_list_continuation_preserves_unicode_and_number_overflow() {
        let old = format!("• {}", "界".repeat(350_000));
        let entered = format!("{old}\n");
        let (continued, cursor) = continue_list_edit(&old, &entered).unwrap();
        assert_eq!(continued, format!("{old}\n• "));
        assert_eq!(cursor as usize, continued.len());
        let old = format!("{}. é👩‍🚀", usize::MAX);
        let (continued, _) = continue_list_edit(&old, &format!("{old}\n")).unwrap();
        assert_eq!(continued, format!("{old}\n{}. ", usize::MAX));
        let (empty, cursor) = continue_list_edit("│ \u{2003}", "│ \u{2003}\n").unwrap();
        assert!(empty.is_empty());
        assert_eq!(cursor, 0);
    }

    #[test]
    fn enter_continues_a_list_after_multibyte_text() {
        let mut document = RichComposeDocument::default();
        let original = "• café";
        document.synchronize(original, original.len() as i32, original.len() as i32);
        let entered = format!("{original}\n");
        let selection = document.synchronize(&entered, entered.len() as i32, entered.len() as i32);
        assert_eq!(document.text(), "• café\n• ");
        assert_eq!(selection.start as usize, document.text().len());
        document.history("undo").unwrap();
        assert_eq!(document.text(), original);
        document.history("redo").unwrap();
        assert_eq!(document.text(), "• café\n• ");
        assert_style_coverage(&document);
    }

    #[test]
    fn enter_on_empty_list_item_exits_the_list() {
        let mut document = RichComposeDocument::default();
        document.synchronize("• First\n• ", 14, 14);
        let selection = document.synchronize("• First\n• \n", 15, 15);
        assert_eq!(document.text(), "• First\n");
        assert_eq!(selection, ComposeSelection { start: 10, end: 10 });
    }

    #[test]
    fn pasted_http_urls_are_linked_automatically() {
        let mut document = RichComposeDocument::default();
        let text = "Read https://slint.dev/blog/slint-1.7-released for details.";
        document.synchronize(text, text.len() as i32, text.len() as i32);
        let link_start = "Read ".len();
        let link_end = text.find(" for details.").unwrap();
        let link = document
            .style_at(link_start)
            .unwrap()
            .link
            .as_ref()
            .unwrap();
        assert!(
            document
                .styles
                .iter()
                .filter(|run| run.range.start < link_end && run.range.end > link_start)
                .all(|run| Arc::ptr_eq(link, run.style.link.as_ref().unwrap()))
        );
        assert_eq!(
            document.body_html().as_deref(),
            Some(
                "<div>Read <a href=\"https://slint.dev/blog/slint-1.7-released\">https://slint.dev/blog/slint-1.7-released</a> for details.</div>"
            )
        );
    }

    #[test]
    fn selected_text_uses_the_supplied_link_destination() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Slint release", 0, 13);
        document.set_link("slint.dev/blog/slint-1.7-released", "Slint release", 0, 13);
        assert_eq!(
            document.active_link(),
            Some("https://slint.dev/blog/slint-1.7-released")
        );
        assert_eq!(
            document.body_html().as_deref(),
            Some(
                "<div><a href=\"https://slint.dev/blog/slint-1.7-released\">Slint release</a></div>"
            )
        );
    }

    #[test]
    fn undo_restores_text_and_formatting_together() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello", 0, 5);
        document.format("bold", "Hello", 0, 5);
        document.history("undo").unwrap();
        assert_eq!(document.body_html().as_deref(), Some("<div>Hello</div>"));
    }

    #[test]
    fn long_draft_history_stays_within_byte_budget_and_keeps_recent_undo() {
        let mut document = RichComposeDocument::default();
        let initial = "x".repeat(1024 * 1024);
        document.synchronize(&initial, initial.len() as i32, initial.len() as i32);
        for _ in 0..40 {
            document.insert_text("y");
        }

        assert!(document.history_retained_bytes() <= MAX_HISTORY_BYTES);
        assert_eq!(document.undo.len(), 41);
        assert!(document.history_retained_bytes() < 2 * 1024 * 1024);
        assert_eq!(document.styles.len(), 1);
        let latest = document.text().to_owned();
        document.history("undo").unwrap();
        assert_eq!(document.text().len(), latest.len() - 1);
        document.history("redo").unwrap();
        assert_eq!(document.text(), latest);

        document.history("undo").unwrap();
        document.insert_text("z");
        assert!(document.history("redo").is_none());
    }

    #[test]
    fn synchronized_long_unicode_edit_retains_only_changed_history_text() {
        let mut document = RichComposeDocument::default();
        let original = "é".repeat(512 * 1024);
        document.synchronize(&original, original.len() as i32, original.len() as i32);
        let edited = format!("{original}界");
        document.synchronize(&edited, edited.len() as i32, edited.len() as i32);
        let latest = document.undo.back().unwrap();
        assert_eq!(latest.before.text.as_ref(), "");
        assert_eq!(latest.after.text.as_ref(), "界");
        assert!(document.history_retained_bytes() < 2 * 1024 * 1024);
        document.history("undo").unwrap();
        assert_eq!(document.text(), original);
        document.history("redo").unwrap();
        assert_eq!(document.text(), edited);
        assert_style_coverage(&document);
    }

    #[test]
    fn small_style_edits_in_long_draft_retain_only_the_selection() {
        let mut document = RichComposeDocument::default();
        let draft = "x".repeat(1024 * 1024);
        document.synchronize(&draft, 0, 0);
        document.undo.clear();
        let start = (draft.len() / 2) as i32;
        let end = start + 1;
        let text_pointer = document.text.as_ptr();

        document.format("bold", &draft, start, end);
        let latest = document.undo.back().unwrap();
        assert_eq!(latest.before.text.as_ref(), "");
        assert_eq!(latest.before.span_len, 1);
        assert!(Arc::ptr_eq(&latest.before.text, &latest.after.text));
        assert!(document.history_retained_bytes() < 1024);
        assert!(document.active_marks().bold);
        document.history("undo").unwrap();
        assert!(!document.active_marks().bold);
        assert_eq!(document.text.as_ptr(), text_pointer);
        document.history("redo").unwrap();
        assert!(document.active_marks().bold);

        document.set_link("example.org", &draft, start, end);
        let latest = document.undo.back().unwrap();
        assert!(Arc::ptr_eq(&latest.before.text, &latest.after.text));
        assert!(latest.retained_bytes < 1024);
        document.history("undo").unwrap();
        assert_eq!(document.active_link(), None);
        document.history("redo").unwrap();
        assert_eq!(document.active_link(), Some("https://example.org"));
        assert_eq!(document.text.as_ptr(), text_pointer);
        assert_style_coverage(&document);
    }

    #[test]
    fn formatting_entire_long_draft_keeps_text_out_of_history() {
        let mut document = RichComposeDocument::default();
        let draft = "x".repeat(1024 * 1024);
        document.synchronize(&draft, 0, 0);
        document.undo.clear();
        document.format("bold", &draft, 0, draft.len() as i32);
        let transaction = document.undo.back().unwrap();
        assert_eq!(transaction.before.span_len, draft.len());
        assert_eq!(transaction.before.text.as_ref(), "");
        assert_eq!(transaction.after.text.as_ref(), "");
        assert!(transaction.retained_bytes < 1024);
        document.history("undo").unwrap();
        assert!(!document.active_marks().bold);
        document.history("redo").unwrap();
        assert!(document.active_marks().bold);
        assert_style_coverage(&document);
    }

    #[test]
    fn unicode_edits_preserve_compact_formatting_through_undo_and_redo() {
        let mut document = RichComposeDocument::default();
        let text = "Aé👩‍🚀Z";
        let bold_end = text.find('Z').unwrap();
        document.synchronize(text, 1, bold_end as i32);
        document.format("bold", text, 1, bold_end as i32);
        assert_eq!(document.styles.len(), 3);
        assert_style_coverage(&document);

        let after_accent = "Aé".len() as i32;
        document.update_selection(after_accent, after_accent);
        document.insert_text("界");
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>A<strong>é界👩‍🚀</strong>Z</div>")
        );
        assert_eq!(document.styles.len(), 3);
        assert_style_coverage(&document);

        let shortened = "Aé界Z";
        document.synchronize(shortened, shortened.len() as i32, shortened.len() as i32);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>A<strong>é界</strong>Z</div>")
        );
        assert_style_coverage(&document);
        document.history("undo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>A<strong>é界👩‍🚀</strong>Z</div>")
        );
        assert_style_coverage(&document);
        document.history("redo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>A<strong>é界</strong>Z</div>")
        );
        assert_style_coverage(&document);
    }

    #[test]
    fn undo_replays_automatic_link_changes_and_block_formatting() {
        let mut document = RichComposeDocument::default();
        document.synchronize("http://a", 8, 8);
        assert_eq!(document.active_link(), None);
        document.insert_text("b");
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><a href=\"http://ab\">http://ab</a></div>")
        );
        document.history("undo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><a href=\"http://a\">http://a</a></div>")
        );
        document.history("redo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><a href=\"http://ab\">http://ab</a></div>")
        );
        assert_style_coverage(&document);

        document.reset();
        document.synchronize("One\nTwo", 0, 7);
        document.format("bullet", "One\nTwo", 0, 7);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<ul><li>One</li><li>Two</li></ul>")
        );
        document.history("undo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>One</div><div>Two</div>")
        );
        document.history("redo").unwrap();
        assert_eq!(
            document.body_html().as_deref(),
            Some("<ul><li>One</li><li>Two</li></ul>")
        );
        assert_style_coverage(&document);
    }

    #[test]
    fn history_entry_limit_keeps_the_recent_128_edits() {
        let mut document = RichComposeDocument::default();
        for _ in 0..140 {
            document.insert_text("x");
        }
        assert_eq!(document.undo.len(), MAX_HISTORY);
        for _ in 0..MAX_HISTORY {
            document.history("undo").unwrap();
        }
        assert_eq!(document.text().len(), 140 - MAX_HISTORY);
        assert!(document.history("undo").is_none());
        for _ in 0..MAX_HISTORY {
            document.history("redo").unwrap();
        }
        assert_eq!(document.text().len(), 140);
        assert_style_coverage(&document);
    }

    #[test]
    fn a_marked_edit_is_followed_through_undo_redo_and_new_edits() {
        let mut document = RichComposeDocument::default();
        document.synchronize_key_edit("T", 1, 1, "T");
        document.replace_unformatted(1..1, "\n\n│ Original", 1);
        let mark = document.mark_last_edit().unwrap();
        assert_eq!(document.edit_state(mark), EditState::Applied);

        // Typing next to it doesn't join it.
        document.synchronize_key_edit("Ta\n\n│ Original", 2, 2, "a");
        document.history("undo").unwrap();
        assert_eq!(document.edit_state(mark), EditState::Applied);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "T");
        assert_eq!(document.edit_state(mark), EditState::Undone);
        document.history("redo").unwrap();
        assert_eq!(document.edit_state(mark), EditState::Applied);

        // A new edit after undoing it takes it out of the history.
        document.history("undo").unwrap();
        document.synchronize_key_edit("Tb", 2, 2, "b");
        assert_eq!(document.edit_state(mark), EditState::Gone);

        // So does the history running out.
        document.replace_unformatted(2..2, "\n\n│ Again", 2);
        let again = document.mark_last_edit().unwrap();
        assert_ne!(again, mark);
        for _ in 0..MAX_HISTORY {
            document.insert_text("x");
        }
        assert_eq!(document.edit_state(again), EditState::Gone);
    }

    #[test]
    fn linked_characters_and_snapshots_share_one_link_allocation() {
        let mut document = RichComposeDocument::default();
        let label = "linked passage";
        document.synchronize(label, 0, label.len() as i32);
        document.set_link("example.org", label, 0, label.len() as i32);
        let first = document.styles[0].style.link.as_ref().unwrap().clone();
        assert!(
            document
                .styles
                .iter()
                .all(|run| Arc::ptr_eq(&first, run.style.link.as_ref().unwrap()))
        );

        document.insert_text("!");
        let snapshot_link = document.undo.back().unwrap().before.styles[0]
            .style
            .link
            .as_ref()
            .unwrap();
        assert!(Arc::ptr_eq(&first, snapshot_link));
        document.history("undo").unwrap();
        assert_eq!(document.active_link(), Some("https://example.org"));
        assert_eq!(
            document.styles[0].style.link.as_deref(),
            Some("https://example.org")
        );
    }

    #[test]
    fn memory_usage_counts_shared_style_only_payloads_once() {
        let mut document = RichComposeDocument::default();
        document.synchronize("linked passage", 0, 14);
        document.set_link("example.org", "linked passage", 0, 14);
        let transaction = document.undo.back().unwrap();
        assert!(Arc::ptr_eq(
            &transaction.before.text,
            &transaction.after.text
        ));
        let usage = document.memory_usage();
        let link = document.styles[0].style.link.as_ref().unwrap();
        assert_eq!(usage.link_bytes, fragment_text_bytes(link));
        assert!(usage.undo_bytes >= fragment_text_bytes(&transaction.before.text));
        assert_eq!(
            usage.total_bytes,
            std::mem::size_of::<RichComposeDocument>()
                + usage.text_capacity
                + usage.style_run_bytes
                + usage.link_bytes
                + usage.undo_bytes
                + usage.redo_bytes
        );
        document.history("undo").unwrap();
        let undone = document.memory_usage();
        assert_eq!(undone.link_bytes, usage.link_bytes);
        assert_eq!(document.text(), "linked passage");
    }

    #[test]
    fn transaction_budget_counts_a_shared_link_once_across_runs() {
        let link: Arc<str> = Arc::from("https://example.org/long/path");
        let text: Arc<str> = Arc::from("ab");
        let styles = vec![
            ComposeStyleRun {
                range: 0..1,
                style: CharacterStyle {
                    marks: BOLD,
                    link: Some(link.clone()),
                    ..CharacterStyle::default()
                },
            },
            ComposeStyleRun {
                range: 1..2,
                style: CharacterStyle {
                    marks: ITALIC,
                    link: Some(link.clone()),
                    ..CharacterStyle::default()
                },
            },
        ];
        let before = EditFragment {
            text: text.clone(),
            span_len: 2,
            styles: styles.clone(),
        };
        let after = EditFragment {
            text,
            span_len: 2,
            styles,
        };
        assert_eq!(
            transaction_bytes(&before, &after),
            fragment_text_bytes(&before.text)
                + (before.styles.capacity() + after.styles.capacity())
                    * std::mem::size_of::<ComposeStyleRun>()
                + fragment_text_bytes(&link)
        );
    }

    #[test]
    fn ordinary_key_edits_group_into_bounded_undo_steps() {
        let mut document = RichComposeDocument::default();
        for character in "hello".chars() {
            let mut next = document.text().to_owned();
            next.push(character);
            let end = next.len() as i32;
            document.synchronize_key_edit(&next, end, end, &character.to_string());
        }
        assert_eq!(document.undo.len(), 1);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "");
        document.history("redo").unwrap();
        assert_eq!(document.text(), "hello");

        document.reset();
        for _ in 0..130 {
            let mut next = document.text().to_owned();
            next.push('x');
            let end = next.len() as i32;
            document.synchronize_key_edit(&next, end, end, "x");
        }
        assert_eq!(document.undo.len(), 3);
        document.history("undo").unwrap();
        assert_eq!(document.text().len(), 128);
    }

    #[test]
    fn typing_groups_break_on_selection_paste_and_ime_edits() {
        let mut document = RichComposeDocument::default();
        document.synchronize_key_edit("a", 1, 1, "a");
        document.synchronize_key_edit("ab", 2, 2, "b");
        document.update_selection(0, 0);
        document.update_selection(2, 2);
        document.synchronize_key_edit("abc", 3, 3, "c");
        assert_eq!(document.undo.len(), 2);

        document.synchronize("abcP", 4, 4); // Paste has no key context.
        document.synchronize_key_edit("abcPq", 5, 5, "q");
        document.synchronize("abcPq語", 8, 8); // IME commit has no key context.
        assert_eq!(document.undo.len(), 5);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "abcPq");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "abcP");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "abc");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "");
    }

    #[test]
    fn grouped_unicode_typing_in_middle_preserves_following_text() {
        let mut document = RichComposeDocument::default();
        document.synchronize("left right", 5, 5);
        document.synchronize_key_edit("left 語right", 8, 8, "語");
        document.synchronize_key_edit("left 語界right", 11, 11, "界");
        assert_eq!(document.undo.len(), 2);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "left right");
        assert_eq!(document.selection(), ComposeSelection { start: 5, end: 5 });
        document.history("redo").unwrap();
        assert_eq!(document.text(), "left 語界right");
        assert_style_coverage(&document);
    }

    #[test]
    fn new_typing_after_partial_undo_discards_redo() {
        let mut document = RichComposeDocument::default();
        document.synchronize_key_edit("a", 1, 1, "a");
        document.synchronize_key_edit("ab", 2, 2, "b");
        document.synchronize_key_edit("ab ", 3, 3, " ");
        document.history("undo").unwrap();
        document.synchronize_key_edit("abc", 3, 3, "c");
        assert_eq!(document.text(), "abc");
        assert!(document.redo.is_empty());
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "");
    }

    #[test]
    fn formatting_and_word_boundaries_break_typing_groups() {
        let mut document = RichComposeDocument::default();
        document.synchronize_key_edit("a", 1, 1, "a");
        document.synchronize_key_edit("ab", 2, 2, "b");
        document.synchronize_key_edit("ab ", 3, 3, " ");
        document.format("bold", "ab ", 0, 2);
        document.update_selection(3, 3);
        document.synchronize_key_edit("ab c", 4, 4, "c");
        assert_eq!(document.undo.len(), 4);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab ");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab ");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "ab");
        document.history("undo").unwrap();
        assert_eq!(document.text(), "");
    }

    #[test]
    fn rtl_grapheme_and_link_fixture_matches_original_edit_history() {
        // These checkpoints were captured from the original per-character
        // composer before the compact-run and transaction changes.
        let mut document = RichComposeDocument::default();
        let initial = "אבג e\u{301} 👩‍🚀\nمرحبا";
        let appended = format!("{initial}!");
        let plain_html = "<div>אבג e\u{301} 👩‍🚀</div><div>مرحبا</div>";
        let bold_html = "<div><strong>אבג</strong> e\u{301} 👩‍🚀</div><div>مرحبا</div>";
        let link_html = "<div><strong>אבג</strong> e\u{301} <a href=\"https://example.org/a?b=1&amp;c=2\">👩‍🚀</a></div><div>مرحبا</div>";
        let italic_html = "<div><strong>אבג</strong> <em>e\u{301}</em> <a href=\"https://example.org/a?b=1&amp;c=2\">👩‍🚀</a></div><div>مرحبا</div>";
        let appended_html = "<div><strong>אבג</strong> <em>e\u{301}</em> <a href=\"https://example.org/a?b=1&amp;c=2\">👩‍🚀</a></div><div>مرحبا!</div>";
        let href = "https://example.org/a?b=1&c=2";
        let plain_runs = [(0..33, 0, None)];
        let bold_runs = [(0..6, BOLD, None), (6..33, 0, None)];
        let link_runs = [
            (0..6, BOLD, None),
            (6..11, 0, None),
            (11..22, 0, Some(href)),
            (22..33, 0, None),
        ];
        let italic_runs = [
            (0..6, BOLD, None),
            (6..7, 0, None),
            (7..10, ITALIC, None),
            (10..11, 0, None),
            (11..22, 0, Some(href)),
            (22..33, 0, None),
        ];
        let appended_runs = [
            (0..6, BOLD, None),
            (6..7, 0, None),
            (7..10, ITALIC, None),
            (10..11, 0, None),
            (11..22, 0, Some(href)),
            (22..34, 0, None),
        ];

        document.synchronize(initial, 33, 33);
        assert_fixture_state(&document, initial, plain_html, (33, 33), &plain_runs);
        document.format("bold", initial, 0, 6);
        assert_fixture_state(&document, initial, bold_html, (0, 6), &bold_runs);
        document.set_link("example.org/a?b=1&c=2", initial, 11, 22);
        assert_fixture_state(&document, initial, link_html, (11, 22), &link_runs);
        document.format("italic", initial, 7, 10);
        assert_fixture_state(&document, initial, italic_html, (7, 10), &italic_runs);
        document.update_selection(33, 33);
        document.synchronize(&appended, 34, 34);
        assert_fixture_state(
            &document,
            &appended,
            appended_html,
            (34, 34),
            &appended_runs,
        );

        document.history("undo").unwrap();
        assert_fixture_state(&document, initial, italic_html, (33, 33), &italic_runs);
        document.history("undo").unwrap();
        assert_fixture_state(&document, initial, link_html, (7, 10), &link_runs);
        document.history("undo").unwrap();
        assert_fixture_state(&document, initial, bold_html, (11, 22), &bold_runs);
        document.history("undo").unwrap();
        assert_fixture_state(&document, initial, plain_html, (0, 6), &plain_runs);

        document.history("redo").unwrap();
        assert_fixture_state(&document, initial, bold_html, (11, 22), &bold_runs);
        document.history("redo").unwrap();
        assert_fixture_state(&document, initial, link_html, (7, 10), &link_runs);
        document.history("redo").unwrap();
        assert_fixture_state(&document, initial, italic_html, (33, 33), &italic_runs);
        document.history("redo").unwrap();
        assert_fixture_state(
            &document,
            &appended,
            appended_html,
            (34, 34),
            &appended_runs,
        );
    }

    #[test]
    fn rtl_block_modes_and_line_break_fixture_matches_original_history() {
        let mut document = RichComposeDocument::default();
        let plain = "שלום\nمرحبا\nCafe\u{301}";
        let bullet = "• שלום\n• مرحبا\n• Cafe\u{301}";
        let continued = "• שלום\n• مرحبا\n• Cafe\u{301}\n• ";
        let numbered = "1. שלום\n2. مرحبا\n3. Cafe\u{301}\n4. ";
        let quoted = "│ שלום\n│ مرحبا\n│ Cafe\u{301}\n│ ";
        let plain_html = "<div>שלום</div><div>مرحبا</div><div>Cafe\u{301}</div>";
        let bullet_html = "<ul><li>שלום</li><li>مرحبا</li><li>Cafe\u{301}</li></ul>";
        let continued_html = "<ul><li>שלום</li><li>مرحبا</li><li>Cafe\u{301}</li><li></li></ul>";
        let numbered_html = "<ol><li>שלום</li><li>مرحبا</li><li>Cafe\u{301}</li><li></li></ol>";
        let quoted_html = "<blockquote><div>שלום</div><div>مرحبا</div><div>Cafe\u{301}</div><div><br></div></blockquote>";

        document.synchronize(plain, 0, 26);
        assert_fixture_state(&document, plain, plain_html, (0, 26), &[(0..26, 0, None)]);
        document.format("bullet", plain, 0, 26);
        assert_fixture_state(&document, bullet, bullet_html, (0, 38), &[(0..38, 0, None)]);
        let entered = format!("{bullet}\n");
        document.synchronize(&entered, entered.len() as i32, entered.len() as i32);
        assert_fixture_state(
            &document,
            continued,
            continued_html,
            (43, 43),
            &[(0..43, 0, None)],
        );
        document.history("undo").unwrap();
        assert_fixture_state(&document, bullet, bullet_html, (0, 38), &[(0..38, 0, None)]);
        document.history("undo").unwrap();
        assert_fixture_state(&document, plain, plain_html, (0, 26), &[(0..26, 0, None)]);
        document.history("redo").unwrap();
        assert_fixture_state(&document, bullet, bullet_html, (0, 38), &[(0..38, 0, None)]);
        document.history("redo").unwrap();
        assert_fixture_state(
            &document,
            continued,
            continued_html,
            (43, 43),
            &[(0..43, 0, None)],
        );

        document.format("number", continued, 0, 43);
        assert_fixture_state(
            &document,
            numbered,
            numbered_html,
            (0, 39),
            &[(0..39, 0, None)],
        );
        document.format("quote", numbered, 0, 39);
        assert_fixture_state(&document, quoted, quoted_html, (0, 43), &[(0..43, 0, None)]);
        document.history("undo").unwrap();
        assert_fixture_state(
            &document,
            numbered,
            numbered_html,
            (0, 39),
            &[(0..39, 0, None)],
        );
        document.history("undo").unwrap();
        assert_fixture_state(
            &document,
            continued,
            continued_html,
            (0, 43),
            &[(0..43, 0, None)],
        );
        document.history("redo").unwrap();
        assert_fixture_state(
            &document,
            numbered,
            numbered_html,
            (0, 39),
            &[(0..39, 0, None)],
        );
        document.history("redo").unwrap();
        assert_fixture_state(&document, quoted, quoted_html, (0, 43), &[(0..43, 0, None)]);
    }

    #[test]
    fn html_is_escaped_before_formatting() {
        let mut document = RichComposeDocument::default();
        document.synchronize("<script>&", 0, 9);
        document.format("underline", "<script>&", 0, 9);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><u>&lt;script&gt;&amp;</u></div>")
        );
    }

    #[test]
    fn combined_native_styles_remain_one_unmodified_text_run() {
        let mut document = RichComposeDocument::default();
        document.synchronize("Hello & welcome", 0, 15);
        document.format("bold", "Hello & welcome", 0, 15);
        document.format("underline", "Hello & welcome", 0, 15);
        document.format("strike", "Hello & welcome", 0, 15);
        document.format("code", "Hello & welcome", 0, 15);

        assert_eq!(document.text(), "Hello & welcome");
        assert_eq!(document.style_runs().len(), 1);
        assert_eq!(document.style_runs()[0].range, 0..15);
        assert_eq!(
            document.style_runs()[0].style.marks,
            BOLD | UNDERLINE | STRIKE | CODE
        );
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><strong><s><u><code>Hello &amp; welcome</code></u></s></strong></div>")
        );
    }

    fn html_for(text: &str) -> String {
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        document.body_html().unwrap()
    }

    #[test]
    fn numbered_lists_keep_the_numbers_the_composer_shows() {
        assert_eq!(
            html_for("1. First point\n\n2. Second point"),
            "<ol><li>First point</li></ol><div><br></div><ol start=\"2\"><li>Second point</li></ol>"
        );
        assert_eq!(
            html_for("1. one\n2. two\nA note\n3. three"),
            "<ol><li>one</li><li>two</li></ol><div>A note</div><ol start=\"3\"><li>three</li></ol>"
        );
        assert_eq!(
            html_for("2. two\n3. three"),
            "<ol start=\"2\"><li>two</li><li>three</li></ol>"
        );
        assert_eq!(
            html_for("1. a\n2. b\n5. c"),
            "<ol><li>a</li><li>b</li></ol><ol start=\"5\"><li>c</li></ol>"
        );
        assert_eq!(
            html_for("2026. was a good year"),
            "<ol start=\"2026\"><li>was a good year</li></ol>"
        );
        assert_eq!(
            html_for("99999999999. too long for a list"),
            "<div>99999999999. too long for a list</div>"
        );
        // Lists count in signed 32 bits, and leading zeros show as typed.
        assert_eq!(
            html_for("2147483647. last"),
            "<ol start=\"2147483647\"><li>last</li></ol>"
        );
        assert_eq!(
            html_for("4155550123. Call me"),
            "<div>4155550123. Call me</div>"
        );
        assert_eq!(
            html_for("01. Intro\n02. Scope\n0. Zero"),
            "<div>01. Intro</div><div>02. Scope</div><ol start=\"0\"><li>Zero</li></ol>"
        );
    }

    #[test]
    fn crlf_line_ends_keep_their_blank_lines() {
        let text = "Para one\r\n\r\n1. item\r\nPara two";
        assert_eq!(
            html_for(text),
            "<div>Para one</div><div><br></div><ol><li>item</li></ol><div>Para two</div>"
        );
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        assert_eq!(reopened(&document).text(), text);
    }

    #[test]
    fn quoted_lines_share_one_blockquote() {
        assert_eq!(
            html_for("│ first\n│ second\n│ \nAfter"),
            "<blockquote><div>first</div><div>second</div><div><br></div></blockquote><div>After</div>"
        );
    }

    #[test]
    fn spaces_html_would_collapse_are_written_as_no_break_spaces() {
        assert_eq!(
            html_for("  two  spaces "),
            "<div>&nbsp;&nbsp;two &nbsp;spaces&nbsp;</div>"
        );
        assert_eq!(html_for("one space"), "<div>one space</div>");
        assert_eq!(html_for("a\tb"), "<div>a&nbsp;&nbsp;&nbsp;&nbsp;b</div>");

        let mut document = RichComposeDocument::default();
        document.load("Hello  world", 0, 0);
        document.format("bold", "Hello  world", 0, 6);
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div><strong>Hello </strong>&nbsp;world</div>")
        );
    }

    fn reopened(document: &RichComposeDocument) -> RichComposeDocument {
        let mut reopened = RichComposeDocument::default();
        let html = document.body_html().unwrap();
        let selection = reopened
            .load_html(&html, document.text())
            .expect("the draft's own HTML");
        let end = reopened.text().len() as i32;
        assert_eq!(selection, ComposeSelection { start: end, end });
        reopened
    }

    #[test]
    fn a_reopened_draft_keeps_its_formatting() {
        let text = "Hi  team,\nRead https://example.org/notes and the plan.\n• שלום\n• Café 👩‍🚀\n1. first\n2. second\n7. seventh\n│ quoted\n│ \nBye ";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        let bold = text.find("team").unwrap();
        document.format("bold", text, bold as i32, (bold + 4) as i32);
        let plan = text.find("the plan").unwrap();
        document.set_link("example.com/plan", text, plan as i32, (plan + 8) as i32);
        document.format("italic", text, plan as i32, (plan + 3) as i32);
        let cafe = text.find("Café").unwrap();
        document.format("code", text, cafe as i32, (cafe + "Café".len()) as i32);
        let quoted = text.find("quoted").unwrap();
        document.format("strike", text, quoted as i32, (quoted + 6) as i32);
        document.format("underline", text, 0, 2);

        let mut reopened = reopened(&document);
        assert_eq!(reopened.text(), document.text());
        assert_eq!(reopened.body_html(), document.body_html());
        assert_style_coverage(&reopened);
        assert_eq!(reopened.history("undo"), None);
    }

    #[test]
    fn quote_blocks_are_sent_as_plain_text_quotes() {
        assert_eq!(
            with_plain_text_quotes("Thanks\n\nPat wrote:\n│ Hello\n│ \n│ > earlier\nBye"),
            "Thanks\n\nPat wrote:\n> Hello\n>\n> > earlier\nBye"
        );
        assert_eq!(with_plain_text_quotes("No quotes"), "No quotes");
    }

    #[test]
    fn text_moved_in_takes_no_formatting_from_the_caret() {
        let mut document = RichComposeDocument::default();
        document.load("Thanks", 0, 0);
        document.format("bold", "Thanks", 0, 6);
        document.update_selection(6, 6);
        let selection = document.replace_unformatted(6..6, "\n\nPat wrote:\n│ Hi", 6);
        assert_eq!(document.text(), "Thanks\n\nPat wrote:\n│ Hi");
        assert_eq!(selection, ComposeSelection { start: 6, end: 6 });
        assert_eq!(
            document.body_html().as_deref(),
            Some(
                "<div><strong>Thanks</strong></div><div><br></div><div>Pat wrote:</div>\
                 <blockquote><div>Hi</div></blockquote>"
            )
        );
        assert_style_coverage(&document);
        document.history("undo");
        assert_eq!(document.text(), "Thanks");
    }

    #[test]
    fn a_link_set_by_hand_on_an_address_survives_reopening() {
        let text = "see (https://a.example) or <https://b.example>";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        for address in ["https://a.example", "https://b.example"] {
            let start = text.find(address).unwrap();
            document.set_link(address, text, start as i32, (start + address.len()) as i32);
        }
        let reopened = reopened(&document);
        assert_eq!(reopened.body_html(), document.body_html());
        assert!(
            reopened
                .body_html()
                .unwrap()
                .contains("href=\"https://b.example\"")
        );
    }

    #[test]
    fn a_reopened_automatic_link_still_follows_its_text() {
        let mut document = RichComposeDocument::default();
        document.load("See https://a.example", 0, 0);
        let mut reopened = reopened(&document);
        reopened.insert_text("/b");
        assert_eq!(
            reopened.body_html().as_deref(),
            Some("<div>See <a href=\"https://a.example/b\">https://a.example/b</a></div>")
        );
    }

    #[test]
    fn drafts_saved_by_earlier_versions_and_server_copies_reopen() {
        let mut document = RichComposeDocument::default();
        let selection = document.load_html(
            "<blockquote>one</blockquote><blockquote><br></blockquote><ol><li>a</li><li>b</li></ol>",
            "│ one\n│ \n1. a\n2. b",
        );
        assert!(selection.is_some());
        assert_eq!(document.text(), "│ one\n│ \n1. a\n2. b");

        let selection = document.load_html(
            "<html><head><meta charset=\"utf-8\"></head><body><div>Hi <a href=\"https://x.example/\" rel=\"noopener noreferrer\">there</a></div>\n<div><br></div></body></html>",
            "Hi there\n",
        );
        assert!(selection.is_some());
        assert_eq!(document.text(), "Hi there\n");
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>Hi <a href=\"https://x.example/\">there</a></div><div><br></div>")
        );
    }

    #[test]
    fn markup_the_composer_cannot_write_is_left_to_the_plain_text() {
        for (html, text) in [
            ("<p>Paragraph</p>", "Paragraph"),
            ("<table><tr><td>cell</td></tr></table>", "cell"),
            ("<div style=\"color:red\">red</div>", "red"),
            ("<div><img src=\"cid:logo\" alt=\"Logo\"></div>", "Logo"),
            (
                "<ul><li>one<ul><li>nested</li></ul></li></ul>",
                "• one\n• nested",
            ),
            (
                "<blockquote><blockquote>nested</blockquote></blockquote>",
                "│ nested",
            ),
            ("<h1>Heading</h1>", "Heading"),
            (
                "<style>div { color: red }</style><div>styled</div>",
                "styled",
            ),
            ("", ""),
        ] {
            let mut document = RichComposeDocument::default();
            document.load("unchanged", 0, 0);
            assert_eq!(document.load_html(html, text), None, "{html}");
            assert_eq!(document.text(), "unchanged");
        }
    }

    #[test]
    fn imported_links_keep_only_web_and_mail_addresses() {
        let mut document = RichComposeDocument::default();
        document
            .load_html(
                "<div><a href=\"javascript:alert(1)\">x</a> <a href=\"mailto:a@example.org\">mail</a></div>",
                "x mail",
            )
            .unwrap();
        assert_eq!(document.text(), "x mail");
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>x <a href=\"mailto:a@example.org\">mail</a></div>")
        );
    }

    #[test]
    fn drafts_whose_html_lost_numbers_or_spacing_reopen_as_their_text() {
        // Earlier versions saved HTML without list numbers or spacing; only
        // the plain text still says what these drafts hold.
        for (html, text) in [
            (
                "<ol><li>Budget</li></ol><div><br></div><ol><li>Hiring</li></ol>",
                "1. Budget\n\n2. Hiring",
            ),
            (
                "<div>Agenda:</div><ol><li>A</li><li>B</li></ol>",
                "Agenda:\n3. A\n4. B",
            ),
            ("<ol><li>was a good year</li></ol>", "2026. was a good year"),
            ("<div>    indented</div>", "    indented"),
            ("<div>a</div><div>   </div><div>b</div>", "a\n   \nb"),
        ] {
            let mut document = RichComposeDocument::default();
            document.load("unchanged", 0, 0);
            assert_eq!(document.load_html(html, text), None, "{text:?}");
            assert_eq!(document.text(), "unchanged");
        }
    }

    #[test]
    fn tabs_no_break_spaces_and_crlf_reopen_as_written() {
        let text = "Qty:\t3\u{a0}boxes\r\nNext line";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        let boxes = text.find("boxes").unwrap();
        document.format("bold", text, boxes as i32, (boxes + 5) as i32);
        let next = text.find("Next").unwrap();
        document.format("italic", text, next as i32, (next + 4) as i32);

        let reopened = reopened(&document);
        assert_eq!(reopened.text(), text);
        assert_eq!(reopened.body_html(), document.body_html());
        assert_style_coverage(&reopened);
    }

    #[test]
    fn positions_follow_edits_before_after_and_beside_them() {
        let mut document = RichComposeDocument::default();
        document.load("Teh cat sat on teh mat", 0, 0);
        let start = document.revision();
        let first = 0..3;
        let second = 15..18;

        // Typing at the start moves both words along.
        document.synchronize_key_edit("Oh, Teh cat sat on teh mat", 4, 4, "");
        assert_eq!(
            document.follow(start, first.clone()),
            Followed::Bordered(4..7)
        );
        assert_eq!(
            document.follow(start, second.clone()),
            Followed::Kept(19..22)
        );

        // Bold changes no text and moves nothing.
        let text = document.text().to_owned();
        document.format("bold", &text, 8, 11);
        assert_eq!(
            document.follow(start, second.clone()),
            Followed::Kept(19..22)
        );

        // Editing inside the second word loses it; the first is kept.
        document.synchronize_key_edit("Oh, Teh cat sat on the mat", 21, 21, "");
        assert_eq!(document.follow(start, second.clone()), Followed::Lost);
        assert_eq!(
            document.follow(start, first.clone()),
            Followed::Bordered(4..7)
        );

        // Undo and redo are edits like any other.
        let typed = document.revision();
        document.history("undo");
        assert_eq!(document.text(), "Oh, Teh cat sat on teh mat");
        assert_eq!(document.follow(typed, 4..7), Followed::Kept(4..7));
        document.history("redo");
        assert_eq!(document.follow(typed, 4..7), Followed::Kept(4..7));

        // A word typed onto the end of another borders it.
        let now = document.revision();
        document.synchronize_key_edit("Oh, Tehh cat sat on the mat", 8, 8, "h");
        assert_eq!(document.follow(now, 4..7), Followed::Bordered(4..7));
    }

    #[test]
    fn positions_from_before_a_reset_or_too_many_edits_are_lost() {
        let mut document = RichComposeDocument::default();
        document.load("Teh cat", 0, 0);
        let loaded = document.revision();
        document.reset();
        assert_eq!(document.follow(loaded, 0..3), Followed::Lost);

        document.load("Teh cat", 0, 0);
        let loaded = document.revision();
        let mut text = String::from("Teh cat");
        for _ in 0..MAX_TEXT_CHANGES + 1 {
            text.push('!');
            let end = text.len() as i32;
            document.synchronize_key_edit(&text, end, end, "!");
        }
        assert_eq!(document.follow(loaded, 0..3), Followed::Lost);
        let recent = document.revision();
        assert_eq!(document.follow(recent, 0..3), Followed::Kept(0..3));
        assert_eq!(document.follow(recent + 1, 0..3), Followed::Lost);
    }

    #[test]
    fn a_replaced_word_keeps_its_formatting_and_undoes_in_one_step() {
        let mut document = RichComposeDocument::default();
        document.load("We recieve it", 0, 0);
        let text = document.text().to_owned();
        document.format("bold", &text, 3, 10);
        let selection = document.replace_word(3..10, "receive");
        assert_eq!(document.text(), "We receive it");
        assert_eq!(selection, ComposeSelection { start: 10, end: 10 });
        let bold = document
            .style_runs()
            .iter()
            .find(|run| run.range.start == 3)
            .unwrap();
        assert_eq!(bold.range, 3..10);
        assert_ne!(bold.style.marks & BOLD, 0);
        document.history("undo");
        assert_eq!(document.text(), "We recieve it");
    }

    #[test]
    fn colors_sizes_and_fonts_go_out_as_spans_and_reopen() {
        let text = "Red big serif highlighted plain";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        let at = |word: &str| {
            let start = text.find(word).unwrap();
            (start as i32, (start + word.len()) as i32)
        };
        let (start, end) = at("Red");
        document.format("color:#d93025", text, start, end);
        let (start, end) = at("big");
        document.format("size:huge", text, start, end);
        let (start, end) = at("serif");
        document.format("family:serif", text, start, end);
        let (start, end) = at("highlighted");
        document.format("highlight:#fff475", text, start, end);
        document.format("bold", text, start, end);
        let html = document.body_html().unwrap();
        assert_eq!(
            html,
            "<div><span style=\"color:#d93025\">Red</span> \
             <span style=\"font-size:xx-large\">big</span> \
             <span style=\"font-family:Times New Roman, Times, serif\">serif</span> \
             <span style=\"background-color:#fff475\"><strong>highlighted</strong></span> \
             plain</div>"
        );
        let reopened = reopened(&document);
        assert_eq!(reopened.styles, document.styles);
        assert_eq!(reopened.body_html().unwrap(), html);
    }

    #[test]
    fn a_paragraph_keeps_its_alignment_through_typing_and_reopening() {
        let mut document = RichComposeDocument::default();
        document.load("Title", 5, 5);
        document.format("align:center", "Title", 5, 5);
        assert_eq!(document.active_marks().align, Alignment::Center);
        // Enter at the end goes on centered, as in a word processor, and so does what's
        // typed there.
        document.synchronize_key_edit("Title\n", 6, 6, "\n");
        assert_eq!(document.active_marks().align, Alignment::Center);
        document.synchronize_key_edit("Title\nSub", 9, 9, "");
        let centered = "<div align=\"center\" style=\"text-align:center\">";
        assert_eq!(
            document.body_html().unwrap(),
            format!("{centered}Title</div>{centered}Sub</div>")
        );
        let reopened = reopened(&document);
        assert_eq!(reopened.paragraph_alignment(6), Alignment::Center);
        assert_eq!(reopened.body_html(), document.body_html());

        // Undo takes the alignment back with the rest of the edit.
        document.format("align:right", "Title\nSub", 0, 0);
        assert_eq!(document.paragraph_alignment(0), Alignment::Right);
        document.history("undo").unwrap();
        assert_eq!(document.paragraph_alignment(0), Alignment::Center);
    }

    #[test]
    fn joined_paragraphs_take_the_first_ones_alignment() {
        let mut document = RichComposeDocument::default();
        document.load("Left\nCentered", 0, 0);
        document.format("align:center", "Left\nCentered", 6, 6);
        assert_eq!(document.paragraph_alignment(5), Alignment::Center);
        assert_eq!(document.paragraph_alignment(0), Alignment::Left);
        // Backspace at the start of the second paragraph joins it to the first.
        document.synchronize_key_edit("LeftCentered", 4, 4, "");
        assert!(
            document
                .styles
                .iter()
                .all(|run| run.style.align == Alignment::Left)
        );
        assert_eq!(
            document.body_html().as_deref(),
            Some("<div>LeftCentered</div>")
        );
        assert_style_coverage(&document);
    }

    #[test]
    fn lists_keep_their_paragraphs_alignment() {
        let text = "One\nTwo";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        document.format("align:right", text, 0, text.len() as i32);
        document.format("bullet", text, 0, text.len() as i32);
        assert_eq!(document.text(), "• One\n• Two");
        assert_eq!(
            document.body_html().as_deref(),
            Some(
                "<ul><li style=\"text-align:right\">One</li>\
                 <li style=\"text-align:right\">Two</li></ul>"
            )
        );
        let reopened = reopened(&document);
        assert_eq!(reopened.body_html(), document.body_html());
        assert_style_coverage(&document);
    }

    #[test]
    fn removing_formatting_keeps_links_and_alignment() {
        let text = "See https://example.org now";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        let all = text.len() as i32;
        for kind in [
            "bold",
            "color:#d93025",
            "highlight:#fff475",
            "size:large",
            "family:monospace",
            "align:center",
        ] {
            document.format(kind, text, 0, all);
        }
        document.format("clear", text, 0, all);
        assert_eq!(
            document.body_html().as_deref(),
            Some(
                "<div align=\"center\" style=\"text-align:center\">See \
                 <a href=\"https://example.org\">https://example.org</a> now</div>"
            )
        );
        document.history("undo").unwrap();
        assert!(
            document
                .body_html()
                .unwrap()
                .contains("font-family:Courier New")
        );
    }

    #[test]
    fn the_toolbar_shows_what_the_whole_selection_shares() {
        let text = "ab cd";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        document.format("color:#1a73e8", text, 0, 5);
        document.format("size:large", text, 0, 2);
        document.update_selection(0, 5);
        let active = document.active_marks();
        assert_eq!(active.color, Some(Some(0x1a73e8)));
        assert_eq!(active.highlight, Some(None));
        assert_eq!(active.size, None);
        assert_eq!(active.family, Some(FontFamily::Default));
        document.update_selection(1, 1);
        assert_eq!(document.active_marks().size, Some(TextSize::Large));
        // At a caret, the next characters' style can change before they're typed.
        document.update_selection(5, 5);
        document.format("color:none", text, 5, 5);
        assert_eq!(document.active_marks().color, Some(None));
        // Text in several colors shares none.
        document.format("color:#cc0000", text, 0, 2);
        document.update_selection(0, 5);
        assert_eq!(document.active_marks().color, None);
    }

    #[test]
    fn messages_start_in_the_default_text_style() {
        let mut document = RichComposeDocument::default();
        document.set_base_style(FontFamily::Serif, TextSize::Large);
        document.load("", 0, 0);
        assert_eq!(document.active_marks().family, Some(FontFamily::Serif));
        document.synchronize_key_edit("Hi", 2, 2, "i");
        assert_eq!(
            document.body_html().unwrap(),
            "<div><span style=\"font-size:x-large;font-family:Times New Roman, Times, serif\">Hi</span></div>"
        );

        // Text a new message opens with, such as a signature, starts in it too.
        let mut document = RichComposeDocument::default();
        document.set_base_style(FontFamily::Monospace, TextSize::Normal);
        document.load("\n\n-- \nAlex", 0, 0);
        assert!(
            document
                .styles
                .iter()
                .all(|run| run.style.family == FontFamily::Monospace)
        );
        assert_eq!(document.active_marks().family, Some(FontFamily::Monospace));
    }

    #[test]
    fn removing_formatting_returns_to_the_default_text_style() {
        let mut document = RichComposeDocument::default();
        document.set_base_style(FontFamily::Monospace, TextSize::Small);
        document.load("abc", 3, 3);
        for command in ["bold", "color:#cc0000", "size:huge", "family:serif"] {
            document.format(command, "abc", 0, 3);
        }
        document.format("clear", "abc", 0, 3);
        let [run] = &document.styles[..] else {
            panic!("one run expected, got {:?}", document.styles);
        };
        assert_eq!(run.style, document.base);
    }

    #[test]
    fn enter_at_the_end_goes_on_in_the_text_style_but_not_its_link() {
        let mut document = RichComposeDocument::default();
        document.load("Big", 3, 3);
        document.format("size:huge", "Big", 0, 3);
        document.format("color:#cc0000", "Big", 0, 3);
        document.update_selection(3, 3);
        document.synchronize_key_edit("Big\n", 4, 4, "\n");
        document.synchronize_key_edit("Big\nx", 5, 5, "x");
        let x = document.style_at(4).unwrap();
        assert_eq!((x.size, x.color), (TextSize::Huge, Some(0xcc0000)));

        let text = "See https://example.com";
        let mut document = RichComposeDocument::default();
        document.load(text, text.len() as i32, text.len() as i32);
        assert!(document.style_at(text.len() - 1).unwrap().link.is_some());
        let entered = format!("{text}\n");
        let end = entered.len() as i32;
        document.synchronize_key_edit(&entered, end, end, "\n");
        assert_eq!(document.typing_style().link, None);
    }

    #[test]
    fn list_items_type_in_their_text_style_after_plain_markers() {
        let text = "• Big";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        let content = text.find("Big").unwrap();
        document.format("size:huge", text, content as i32, text.len() as i32);
        // Right after the marker, typing goes on in the item's text.
        document.update_selection(content as i32, content as i32);
        assert_eq!(document.active_marks().size, Some(TextSize::Huge));

        // Enter continues the list. The new marker is plain, as readers show markers, and
        // what's typed after it goes on in the item's style.
        let end = text.len() as i32;
        document.update_selection(end, end);
        document.synchronize_key_edit("• Big\n", end + 1, end + 1, "\n");
        assert_eq!(document.text(), "• Big\n• ");
        let marker = document.text().rfind('•').unwrap();
        assert_eq!(document.style_at(marker).unwrap().size, TextSize::Normal);
        assert_eq!(document.active_marks().size, Some(TextSize::Huge));
        let typed = format!("{}x", document.text());
        let caret = typed.len() as i32;
        document.synchronize_key_edit(&typed, caret, caret, "x");
        assert_eq!(
            document.style_at(typed.len() - 1).unwrap().size,
            TextSize::Huge
        );
        assert_eq!(
            document.body_html().unwrap(),
            "<ul><li><span style=\"font-size:xx-large\">Big</span></li>\
             <li><span style=\"font-size:xx-large\">x</span></li></ul>"
        );
    }

    #[test]
    fn pasting_html_replaces_the_selection_as_one_undoable_edit() {
        let mut document = RichComposeDocument::default();
        document.load("Hello world", 0, 0);
        document.update_selection(6, 11);
        let selection = document
            .paste_html("<b>there</b> <span style=\"color:#cc0000\">friend</span>")
            .unwrap();
        assert_eq!(document.text(), "Hello there friend");
        assert_eq!(selection, ComposeSelection { start: 18, end: 18 });
        assert_eq!(document.style_at(6).unwrap().marks, BOLD);
        assert_eq!(document.style_at(12).unwrap().color, Some(0xcc0000));
        assert_eq!(document.style_at(0).unwrap().marks, 0);
        document.history("undo").unwrap();
        assert_eq!(document.text(), "Hello world");
        // HTML without text leaves the plain text to paste.
        assert!(
            document
                .paste_html("<img src=\"https://example.com/a.png\">")
                .is_none()
        );
    }

    #[test]
    fn html_from_other_apps_pastes_as_a_browser_shows_it() {
        // Markup laid out on lines of its own, which only the composer's own copies aren't.
        let mut document = RichComposeDocument::default();
        document.load("", 0, 0);
        document
            .paste_html("<div>\n  Hello\n  <b>there</b>\n</div>\n<div>again</div>")
            .unwrap();
        assert_eq!(document.text(), "Hello there\nagain");
        assert_eq!(document.style_at(6).unwrap().marks, BOLD);
    }

    #[test]
    fn a_pasted_list_marker_stays_only_where_a_line_starts() {
        let list = "<ul><li>first</li><li>second</li></ul>";
        let mut document = RichComposeDocument::default();
        document.load("Items: ", 7, 7);
        document.paste_html(list).unwrap();
        assert_eq!(document.text(), "Items: first\n• second");

        let mut document = RichComposeDocument::default();
        document.load("Items:\n", 7, 7);
        document.paste_html(list).unwrap();
        assert_eq!(document.text(), "Items:\n• first\n• second");
    }

    #[test]
    fn copied_text_pastes_back_as_it_was() {
        let text = "Title\nRed huge words\n• item\nEnd";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        let at = |word: &str| {
            let start = text.find(word).unwrap();
            (start as i32, (start + word.len()) as i32)
        };
        let (start, end) = at("Title");
        document.format("align:center", text, start, end);
        let (start, end) = at("Red huge words");
        document.format("color:#cc0000", text, start, end);
        document.format("size:huge", text, start, end);
        let (start, end) = at("item");
        document.format("bold", text, start, end);
        document.update_selection(0, text.len() as i32);
        let (html, plain) = document.selection_html().unwrap();
        assert_eq!(plain, text);

        let mut pasted = RichComposeDocument::default();
        pasted.load("", 0, 0);
        pasted.paste_html(&html).unwrap();
        assert_eq!(pasted.text(), text);
        assert_eq!(pasted.styles, document.styles);

        // Part of a line copies as just that part.
        let (start, end) = at("huge words");
        document.update_selection(start, end);
        let (html, plain) = document.selection_html().unwrap();
        assert_eq!(plain, "huge words");
        assert_eq!(
            html,
            "<!--flectar-mail-composer--><div><span style=\"color:#cc0000;font-size:xx-large\">huge words</span></div>"
        );
        document.update_selection(3, 3);
        assert!(document.selection_html().is_none());
    }

    /// A PNG `width` by `height` in one color, as a pasted screenshot might be.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([220, 40, 30, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        bytes
    }

    #[test]
    fn images_go_out_as_img_tags_and_reopen() {
        let mut document = RichComposeDocument::default();
        document.load("Before after", 7, 7);
        let image = InlineImage::from_file_bytes(png(40, 20), "Q3 \"chart\"").unwrap();
        assert_eq!(
            (image.mime_type, image.width, image.height),
            ("image/png", 40, 20)
        );
        document.insert_image(image);
        assert_eq!(document.text(), "Before \u{fffc}after");
        let html = document.body_html().unwrap();
        assert!(
            html.starts_with("<div>Before <img src=\"data:image/png;base64,"),
            "{html}"
        );
        assert!(
            html.ends_with(
                "\" alt=\"Q3 &quot;chart&quot;\" width=\"40\" height=\"20\" \
                 style=\"width:40px;max-width:100%;height:auto\">after</div>"
            ),
            "{html}"
        );
        let reopened = reopened(&document);
        assert_eq!(reopened.text(), document.text());
        assert_eq!(reopened.body_html().unwrap(), html);
        let image = reopened.images().next().unwrap();
        assert_eq!(
            (image.width, image.height, &*image.alt),
            (40, 20, "Q3 \"chart\"")
        );

        // Plain text names the image in its place.
        assert_eq!(
            document.text_with_image_descriptions(),
            "Before [image: Q3 \"chart\"]after"
        );
        // Files that aren't images, and images in formats mail doesn't show, aren't taken.
        assert!(InlineImage::from_file_bytes(b"not an image".to_vec(), "").is_none());
    }

    #[test]
    fn plain_text_writes_out_link_addresses_and_names_pictures() {
        let text = "See the plan, write to ana@example.org or go to example.org/a.";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        let at = |part: &str| {
            let start = text.find(part).unwrap() as i32;
            (start, start + part.len() as i32)
        };
        let (start, end) = at("the plan");
        document.set_link("https://example.org/plan", text, start, end);
        let (start, end) = at("ana@example.org");
        document.set_link("mailto:ana@example.org", text, start, end);
        let (start, end) = at("example.org/a");
        document.set_link("https://example.org/a/", text, start, end);
        // A link partly bold is still one link.
        let (start, _) = at("plan");
        document.format("bold", text, start, start + 4);
        document.update_selection(0, 0);
        document.insert_image(InlineImage::from_file_bytes(png(4, 4), "Map").unwrap());
        assert_eq!(
            document.plain_text(),
            "[image: Map]See the plan <https://example.org/plan>, write to ana@example.org or \
             go to example.org/a."
        );
    }

    #[test]
    fn making_text_plain_takes_formatting_out_in_one_undoable_edit() {
        let text = "Hi Ana\nThe plan is ready";
        let mut document = RichComposeDocument::default();
        document.load(text, 0, 0);
        assert_eq!(
            document.make_plain(),
            None,
            "plain text has nothing to take out"
        );
        document.format("bold", text, 0, 2);
        document.format("color:#c0392b", text, 3, 6);
        let plan = text.find("plan").unwrap() as i32;
        document.set_link("https://example.org/plan", text, plan, plan + 4);
        document.update_selection(7, 7);
        document.insert_image(InlineImage::from_file_bytes(png(4, 4), "Chart").unwrap());
        document.insert_image(InlineImage::from_file_bytes(png(4, 4), "").unwrap());
        let caret = document.selection();
        let formatted_html = document.body_html();

        let selection = document.make_plain().unwrap();
        assert_eq!(
            document.text(),
            "Hi Ana\nChartThe plan <https://example.org/plan> is ready"
        );
        // The caret stays after the pictures' alt text.
        let placeholders = 2 * IMAGE_PLACEHOLDER.len_utf8() as i32;
        assert_eq!(
            selection.start,
            caret.start - placeholders + "Chart".len() as i32
        );
        assert!(
            document
                .style_runs()
                .iter()
                .all(|run| is_plain_style(&run.style, &document.base)),
            "{:?}",
            document.style_runs()
        );
        assert_eq!(document.make_plain(), None);
        // Undo brings the formatting and the pictures back.
        document.history("undo").unwrap();
        assert_eq!(document.body_html(), formatted_html);
    }

    #[test]
    fn plain_signatures_go_in_as_text() {
        use base64::Engine;
        let mut document = RichComposeDocument::default();
        document.load("Thanks", 6, 6);
        let logo = base64::engine::general_purpose::STANDARD.encode(png(4, 4));
        let html = format!(
            "<div><b>Zoë</b> · <a href=\"https://example.org\">Example</a><br>\
             <img src=\"data:image/png;base64,{logo}\" alt=\"Logo\"></div>"
        );
        let (_, block) = document.replace_signature(None, 6, Some(&html), true);
        let signature = "\n\n-- \nZoë · Example <https://example.org>\nLogo";
        assert_eq!(document.text(), format!("Thanks{signature}"));
        assert_eq!(block.as_deref(), Some(signature));
        assert!(
            document
                .style_runs()
                .iter()
                .all(|run| is_plain_style(&run.style, &document.base))
        );
    }

    #[test]
    fn images_can_go_out_with_other_files_at_the_same_size() {
        use base64::Engine;
        let mut document = RichComposeDocument::default();
        document.load("A\n\n", 1, 1);
        document.insert_image(InlineImage::from_file_bytes(png(40, 20), "Chart").unwrap());
        document.insert_image(InlineImage::from_file_bytes(png(30, 30), "").unwrap());
        let swapped = document.images().next().unwrap().id;
        let smaller: Arc<[u8]> = png(4, 2).into();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&smaller);
        let swap = |image: &InlineImage| (image.id == swapped).then(|| Arc::clone(&smaller));
        let html = document.body_html_with(false, swap).unwrap();
        // The other file, at the size the image shows, and the image left as it was.
        assert!(
            html.contains(&format!(
                "<img src=\"data:image/png;base64,{encoded}\" alt=\"Chart\" width=\"40\" \
                 height=\"20\""
            )),
            "{html}"
        );
        assert_eq!(html.matches("<img ").count(), 2);
        assert!(html.ends_with("<div><br></div><div><br></div>"), "{html}");
        let above = document.body_html_with(true, swap).unwrap();
        assert!(above.contains(&encoded) && !above.ends_with("<div><br></div>"));
        // The document keeps its own files.
        assert_eq!(
            document.images().next().unwrap().data.len(),
            png(40, 20).len()
        );
        assert_eq!(
            document.body_html_with(false, |_| None),
            document.body_html()
        );
    }

    #[test]
    fn images_show_at_a_chosen_size_and_carry_alt_text() {
        let mut document = RichComposeDocument::default();
        document.load("See ", 4, 4);
        document.insert_image(InlineImage::from_file_bytes(png(1200, 600), "").unwrap());
        document.insert_image(InlineImage::from_file_bytes(png(200, 100), "").unwrap());
        let text = document.text().to_owned();
        let first = 4..4 + IMAGE_PLACEHOLDER.len_utf8();
        let second = first.end..text.len();

        // Only a selection of one image has controls.
        document.update_selection(first.start as i32, first.end as i32);
        assert_eq!(
            document.selected_image().map(|image| image.width),
            Some(1200)
        );
        document.update_selection(0, text.len() as i32);
        assert!(document.selected_image().is_none());
        document.update_selection(4, 4);
        assert!(document.selected_image().is_none());

        let selected = |document: &RichComposeDocument, range: &std::ops::Range<usize>| {
            document
                .style_at(range.start)
                .unwrap()
                .image
                .clone()
                .unwrap()
        };
        document.format(
            "image-size:medium",
            &text,
            first.start as i32,
            first.end as i32,
        );
        let image = selected(&document, &first);
        assert_eq!(image.shown_width, Some(320));
        assert_eq!(ImageSize::of(&image), Some(ImageSize::Medium));
        let html = document.body_html().unwrap();
        assert!(
            html.contains("width=\"320\" height=\"160\" style=\"width:320px;"),
            "{html}"
        );

        // A size wider than the image leaves it as it is.
        document.format(
            "image-size:large",
            &text,
            second.start as i32,
            second.end as i32,
        );
        assert_eq!(selected(&document, &second).shown_width, None);
        assert!(!ImageSize::Large.fits(200) && ImageSize::Small.fits(200));

        document.format(
            "image-alt:  Sales   by month ",
            &text,
            first.start as i32,
            first.end as i32,
        );
        assert_eq!(&*selected(&document, &first).alt, "Sales by month");
        assert_eq!(
            document.text_with_image_descriptions(),
            "See [image: Sales by month][image]"
        );
        // Both reopen as they were.
        let reopened = reopened(&document);
        let image = reopened.images().next().unwrap();
        assert_eq!(
            (image.shown_width, &*image.alt),
            (Some(320), "Sales by month")
        );

        // Each change undoes on its own, and back to its own size.
        document.history("undo");
        assert_eq!(&*selected(&document, &first).alt, "");
        document.history("undo");
        assert_eq!(selected(&document, &first).shown_width, None);
        document.history("redo");
        assert_eq!(selected(&document, &first).shown_width, Some(320));
        document.format(
            "image-size:original",
            &text,
            first.start as i32,
            first.end as i32,
        );
        assert_eq!(selected(&document, &first).shown_width, None);
    }

    #[test]
    fn dragging_an_image_corner_gives_it_a_width_of_its_own() {
        let mut document = RichComposeDocument::default();
        document.load("", 0, 0);
        document.insert_image(InlineImage::from_file_bytes(png(1000, 500), "").unwrap());
        let text = document.text().to_owned();
        let end = text.len() as i32;
        let shown = |document: &RichComposeDocument| document.images().next().unwrap().shown_width;
        document.format("image-width:437", &text, 0, end);
        assert_eq!(shown(&document), Some(437));
        assert_eq!(ImageSize::of(document.images().next().unwrap()), None);
        // Never past its own width, and never too small to find again.
        document.format("image-width:1400", &text, 0, end);
        assert_eq!(shown(&document), None);
        document.format("image-width:3", &text, 0, end);
        assert_eq!(shown(&document), Some(MIN_IMAGE_WIDTH));
        document.format("image-width:wide", &text, 0, end);
        assert_eq!(shown(&document), Some(MIN_IMAGE_WIDTH));
        document.history("undo");
        assert_eq!(shown(&document), None);
    }

    #[test]
    fn reopened_messages_keep_the_default_text_style() {
        let mut document = RichComposeDocument::default();
        document.set_base_style(FontFamily::Serif, TextSize::Large);
        document.load("Hello", 5, 5);
        document.format("bold", "Hello", 0, 5);
        let html = document.body_html().unwrap();

        let mut reopened = RichComposeDocument::default();
        reopened.set_base_style(FontFamily::Serif, TextSize::Large);
        reopened.load_html(&html, "Hello").unwrap();
        // Remove formatting goes back to it.
        reopened.format("clear", "Hello", 0, 5);
        let style = reopened.style_at(0).unwrap();
        assert_eq!(
            (style.family, style.size, style.marks),
            (FontFamily::Serif, TextSize::Large, 0)
        );
    }

    #[test]
    fn messages_from_other_apps_reopen_with_their_formatting_and_images() {
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD.encode(png(40, 20));
        let html = format!(
            "<div dir=\"ltr\"><div>Hi <b>Jamie</b>,</div><div><br></div>\
             <div><img src=\"data:image/png;base64,{png}\" alt=\"Chart\" width=\"30\"><br></div>\
             <ul><li>First</li><li>Second</li></ul></div>"
        );
        let mut document = RichComposeDocument::default();
        document.set_base_style(FontFamily::Serif, TextSize::Normal);
        // What the composer itself didn't write doesn't reopen as its own.
        assert!(document.load_html(&html, "Hi Jamie,").is_none());
        document.load_foreign_html(&html).unwrap();
        assert_eq!(document.text(), "Hi Jamie,\n\n\u{fffc}\n• First\n• Second");
        let jamie = document.text().find("Jamie").unwrap();
        assert_eq!(document.style_at(jamie).unwrap().marks, BOLD);
        let image = document.images().next().unwrap();
        assert_eq!(
            (image.width, image.shown_width, &*image.alt),
            (40, Some(30), "Chart")
        );
        // Its usual font becomes the message's, and nothing is left to undo.
        assert_eq!(document.style_at(0).unwrap().family, FontFamily::Serif);
        assert!(document.history("undo").is_none());

        let mut untouched = RichComposeDocument::default();
        untouched.load("Kept", 4, 4);
        assert!(untouched.load_foreign_html("<div><br></div>").is_none());
        assert_eq!(untouched.text(), "Kept");
    }

    #[test]
    fn signatures_go_in_with_their_formatting_and_swap_in_place() {
        use base64::Engine;
        let logo = base64::engine::general_purpose::STANDARD.encode(png(60, 20));
        // One the composer wrote, with a picture, and an older plain one.
        let mut source = RichComposeDocument::default();
        source.load("Zoë Ortiz\nEngineer\n", 0, 0);
        source.format("bold", "Zoë Ortiz\nEngineer\n", 0, "Zoë Ortiz".len() as i32);
        let end = source.text().len() as i32;
        source.update_selection(end, end);
        source.insert_image(InlineImage::from_file_bytes(png(60, 20), "Logo").unwrap());
        let rich = source.body_html().unwrap();
        assert!(rich.contains(&logo[..20]));
        let plain = "<div>Pat Lee<br>Support</div>";

        let mut document = RichComposeDocument::default();
        document.load("", 0, 0);
        let (selection, block) = document.replace_signature(None, 0, Some(&rich), false);
        let block = block.unwrap();
        assert_eq!(block, "\n\n-- \nZoë Ortiz\nEngineer\n\u{fffc}");
        assert_eq!(document.text(), block);
        // The caret stays above it, where the message is written.
        assert_eq!((selection.start, selection.end), (0, 0));
        let name = document.text().find("Zoë").unwrap();
        assert_eq!(document.style_at(name).unwrap().marks, BOLD);
        assert_eq!(&*document.images().next().unwrap().alt, "Logo");
        assert_eq!(document.style_at(0).unwrap().marks, 0);

        // Writing goes on above it, and another signature takes its place.
        let written = format!("Hello{}", document.text());
        document.synchronize(&written, 5, 5);
        let (selection, swapped) = document.replace_signature(Some(&block), 0, Some(plain), false);
        let swapped = swapped.unwrap();
        assert_eq!(document.text(), "Hello\n\n-- \nPat Lee\nSupport");
        assert_eq!(swapped, "\n\n-- \nPat Lee\nSupport");
        assert_eq!((selection.start, selection.end), (5, 5));
        assert!(document.images().next().is_none());

        // One undo brings the first back, and no signature takes it out.
        document.history("undo");
        assert_eq!(document.text(), format!("Hello{block}"));
        document.history("redo");
        let (_, none) = document.replace_signature(Some(&swapped), 0, None, false);
        assert_eq!((document.text(), none), ("Hello", None));
        // Nothing to swap is no edit.
        let (_, none) = document.replace_signature(None, 5, Some("<div> </div>"), false);
        assert_eq!((document.text(), none), ("Hello", None));
        document.history("undo");
        assert_eq!(document.text(), format!("Hello{swapped}"));
    }

    #[test]
    fn signatures_open_to_write_and_preview_with_their_pictures() {
        // Written in the composer, plain unless formatted.
        let mut written = RichComposeDocument::default();
        written.load("Zoë Ortiz\n", 0, 0);
        written.format("color:#0b5394", "Zoë Ortiz\n", 0, "Zoë Ortiz".len() as i32);
        let end = written.text().len() as i32;
        written.update_selection(end, end);
        written.insert_image(InlineImage::from_file_bytes(png(60, 20), "Logo").unwrap());
        let html = written.body_html().unwrap();

        let mut editor = RichComposeDocument::default();
        editor.load_signature_html(&html);
        assert_eq!(editor.text(), "Zoë Ortiz\n\u{fffc}");
        assert!(!editor.changed_since_saved());
        assert!(editor.history("undo").is_none());
        editor.format("bold", "Zoë Ortiz\n\u{fffc}", 0, 3);
        assert!(editor.changed_since_saved());
        editor.mark_saved();
        assert!(!editor.changed_since_saved());

        assert_eq!(signature_preview(&html), "Zoë Ortiz\n[image: Logo]");
        assert_eq!(
            signature_preview("<div>Pat Lee<br>Support</div>"),
            "Pat Lee\nSupport"
        );
        assert_eq!(signature_preview("<div> </div>"), "");

        // In a message, plain text takes the message's font and size, and set ones stay.
        let mut message = RichComposeDocument::default();
        message.set_base_style(FontFamily::Serif, TextSize::Large);
        message.load("", 0, 0);
        let (_, block) = message.replace_signature(None, 0, Some(&html), false);
        assert!(block.is_some());
        let name = message.text().find("Zoë").unwrap();
        let style = message.style_at(name).unwrap();
        assert_eq!(
            (style.family, style.size, style.color),
            (FontFamily::Serif, TextSize::Large, Some(0x0b5394))
        );
    }

    #[test]
    fn text_next_to_an_image_is_text() {
        let mut document = RichComposeDocument::default();
        document.load("", 0, 0);
        document.insert_image(InlineImage::from_file_bytes(png(4, 4), "").unwrap());
        let text = format!("{}x", document.text());
        let end = text.len() as i32;
        document.synchronize_key_edit(&text, end, end, "x");
        assert_eq!(document.style_at(text.len() - 1).unwrap().image, None);
        assert_eq!(document.images().count(), 1);

        // Removing formatting leaves the image.
        document.format("bold", &text, 0, end);
        document.format("clear", &text, 0, end);
        assert_eq!(document.images().count(), 1);

        // Typing over it takes it out, and undo puts it back.
        document.update_selection(0, 3);
        let typed = "yx";
        document.synchronize_key_edit(typed, 1, 1, "y");
        assert_eq!(document.images().count(), 0);
        document.history("undo").unwrap();
        assert_eq!(document.images().count(), 1);
    }

    #[test]
    fn only_the_styles_the_composer_writes_reopen() {
        for (html, text) in [
            ("<div><span style=\"color:red\">red</span></div>", "red"),
            (
                "<div><span style=\"color:rgb(1, 2, 3)\">rgb</span></div>",
                "rgb",
            ),
            ("<div><span style=\"font-size:13px\">px</span></div>", "px"),
            (
                "<div><span style=\"font-family:Comic Sans MS\">comic</span></div>",
                "comic",
            ),
            (
                "<div style=\"text-align:justify\">justified</div>",
                "justified",
            ),
            (
                "<div align=\"center\" style=\"text-align:right\">mixed</div>",
                "mixed",
            ),
            (
                "<div><strong style=\"color:#d93025\">red</strong></div>",
                "red",
            ),
        ] {
            let mut document = RichComposeDocument::default();
            document.load("unchanged", 0, 0);
            assert_eq!(document.load_html(html, text), None, "{html}");
            assert_eq!(document.text(), "unchanged");
        }
    }
}
