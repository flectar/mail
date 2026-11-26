//! Messages kept open beside the active one in a conversation (#435). The
//! active message keeps the reader's own renderer. Each kept message has a
//! renderer of its own on the same paint backend, painted on the CPU only
//! where its card is in view.

use super::*;
use crate::open_messages::{KeptOpenRow, OpenMessageSet, kept_open_rows};
use crate::renderer::PaintBackend;
use std::hash::{Hash, Hasher};

/// The space under a kept message's body inside its card.
const BODY_PADDING: f32 = 12.0;

struct KeptMessage {
    id: i32,
    renderer: GpuEmailRenderer,
    tiles: Rc<VecModel<EmailTile>>,
    /// The part of its body in view, from the top of its body.
    region: (f32, f32),
    /// The height it was shown at when it was last painted.
    shown_height: f32,
}

impl KeptMessage {
    /// Paint the part in view, or give the pixels back out of view. Returns
    /// whether its tiles or its height changed.
    fn paint(&mut self, width: u32, height: u32, scale: f32) -> bool {
        let (start, shown) = self.region;
        let changed = if shown <= 0.0 {
            self.renderer.set_offscreen();
            let had_tiles = self.tiles.row_count() > 0;
            self.tiles.set_vec(Vec::new());
            had_tiles
        } else {
            self.renderer.set_visible_region(start, shown);
            match self.renderer.render_cpu_if_needed(width, height, scale) {
                Ok(Some(frame)) => {
                    let tiles = frame.tiles.into_iter().map(slint_email_tile).collect();
                    crate::reconcile_model_rows_by(
                        &*self.tiles,
                        tiles,
                        |tile| tile.y.to_bits(),
                        PartialEq::eq,
                    );
                    true
                }
                Ok(None) => false,
                Err(error) => {
                    eprintln!("a message kept open could not be painted: {error}");
                    self.tiles.set_vec(Vec::new());
                    true
                }
            }
        };
        let height = self.renderer.displayed_height();
        let height_changed = height != self.shown_height;
        self.shown_height = height;
        changed || height_changed
    }
}

/// The messages open in the reader's conversation and the renderers of the
/// ones kept open.
#[derive(Default)]
pub(crate) struct KeptMessages {
    pub(crate) open: OpenMessageSet,
    kept: Vec<KeptMessage>,
    runtime: Option<tokio::runtime::Handle>,
    notifier: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl KeptMessages {
    /// Kept messages load their images like the reader's own renderer.
    pub(crate) fn configure(
        &mut self,
        runtime: tokio::runtime::Handle,
        notifier: Arc<dyn Fn() + Send + Sync>,
    ) {
        self.runtime = Some(runtime);
        self.notifier = Some(notifier);
    }

    /// Another conversation, or none: every message closes.
    pub(crate) fn clear(&mut self) {
        self.open = OpenMessageSet::default();
        self.kept.clear();
    }

    /// Remote images were turned on or off for every message.
    pub(crate) fn allow_remote_images(&mut self, enabled: bool) {
        for message in &mut self.kept {
            if let Some(runtime) = self.runtime.as_ref() {
                let _ = message
                    .renderer
                    .configure_resources(runtime.clone(), enabled);
            }
        }
    }

    fn renderer(&self, backend: Rc<RefCell<PaintBackend>>, allow_remote: bool) -> GpuEmailRenderer {
        let mut renderer = GpuEmailRenderer::with_backend(backend);
        renderer.set_auto_fit(true);
        if let Some(notifier) = self.notifier.as_ref() {
            renderer.set_resource_notifier(Arc::clone(notifier));
        }
        if let Some(runtime) = self.runtime.as_ref()
            && let Err(error) = renderer.configure_resources(runtime.clone(), allow_remote)
        {
            eprintln!("images of a message kept open are unavailable: {error}");
        }
        renderer
    }

    fn get(&self, id: i32) -> Option<&KeptMessage> {
        self.kept.iter().find(|message| message.id == id)
    }

    /// A kept message's body: its height per unit of the reader's width, and
    /// the height under it that doesn't scale.
    fn size(&self, id: i32, viewport_width: f32) -> (f32, f32) {
        self.get(id).map_or((0.0, 0.0), |message| {
            (
                message.renderer.displayed_height() / viewport_width.max(1.0),
                BODY_PADDING,
            )
        })
    }

    fn width_ratio(&self, id: i32, viewport_width: f32) -> f32 {
        self.get(id).map_or(1.0, |message| {
            (message.renderer.layout_width / viewport_width.max(1.0)).max(1.0)
        })
    }

    fn tiles(&self, id: i32) -> ModelRc<EmailTile> {
        self.get(id).map_or_else(ModelRc::default, |message| {
            ModelRc::from(Rc::clone(&message.tiles))
        })
    }

    /// Hand image completions to the kept documents. Returns whether any of
    /// them changed.
    pub(crate) fn poll_resources(&mut self) -> bool {
        // Every document takes its completions, so this doesn't stop early.
        let mut changed = false;
        for message in &mut self.kept {
            changed |= message.renderer.poll_resources();
        }
        changed
    }

    /// The window went to the tray, or came back.
    pub(crate) fn set_suspended(&mut self, suspended: bool) {
        for message in &mut self.kept {
            message.renderer.set_suspended(suspended);
            if suspended {
                message.tiles.set_vec(Vec::new());
            }
        }
    }

    /// Under memory pressure keep only the tiles in view.
    pub(crate) fn release_caches(&mut self, until: std::time::Instant) {
        for message in &mut self.kept {
            if message.renderer.release_caches(until) {
                let tiles: Vec<EmailTile> = message
                    .renderer
                    .retained_tiles()
                    .into_iter()
                    .map(slint_email_tile)
                    .collect();
                message.tiles.set_vec(tiles);
            }
        }
    }
}

/// The message as the reader would show it: its translation, its body, or a
/// stand-in made of its preview while the body loads.
fn shown_html(message: &MailMessage) -> String {
    crate::translation::shown_html(i64::from(message.id))
        .or_else(|| message.html.clone())
        .unwrap_or_else(|| {
            format!(
                "<html><body style=\"font-family:Arial,sans-serif;padding:32px;line-height:1.6;color:#303348\"><p>{}</p></body></html>",
                display_preview(&message.preview)
            )
        })
}

/// Bring the open messages in line with the conversation and its active
/// message, and give each kept message a document of its message as it is
/// now, such as a body that arrived since or remote images allowed.
pub(super) fn reconcile(app: &AppWindow, state: &mut InboxState, active: i32) {
    let keep_open = app.get_keep_messages_open();
    let thread: Vec<i32> = state
        .conversation_messages
        .iter()
        .map(|message| message.id)
        .collect();
    let kept = &mut state.kept;
    kept.open.retain(&thread);
    if kept.open.active() != Some(active) {
        kept.open.open(active, keep_open);
    }
    if !keep_open {
        kept.open.keep_only_active();
    }
    let open = kept.open.clone();
    kept.kept.retain(|message| open.is_kept_open(message.id));
    if open.count() < 2 {
        return;
    }

    let backend = state.email_renderer.borrow().backend();
    let appearance = crate::message_appearance::EmailAppearance::from_app(app);
    let (width, height) = email_viewport_size(&ReaderRef::main(app));
    let scale = app.window().scale_factor();
    for message in &state.conversation_messages {
        if !open.is_kept_open(message.id) {
            continue;
        }
        let allow_remote =
            state.remote_images_enabled || state.remote_images_override_id == Some(message.id);
        let html = shown_html(message);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        html.hash(&mut hash);
        let key = (hash.finish(), allow_remote);
        let position = match state
            .kept
            .kept
            .iter()
            .position(|kept| kept.id == message.id)
        {
            Some(position) => position,
            None => {
                let renderer = state
                    .kept
                    .renderer(Rc::clone(&backend), state.remote_images_enabled);
                state.kept.kept.push(KeptMessage {
                    id: message.id,
                    renderer,
                    tiles: Rc::new(VecModel::default()),
                    region: (0.0, 0.0),
                    shown_height: 0.0,
                });
                state.kept.kept.len() - 1
            }
        };
        let kept = &mut state.kept.kept[position];
        kept.renderer.set_appearance(appearance);
        if kept.renderer.loaded_key == Some(key) {
            continue;
        }
        kept.renderer.preparation_viewport = (
            (width as f32 * scale).ceil() as u32,
            (height as f32 * scale).ceil() as u32,
            scale,
        );
        match kept.renderer.prepare_email_html(&html, allow_remote) {
            Ok(prepared) => kept.renderer.set_email(prepared),
            Err(error) => {
                eprintln!("a message kept open could not be laid out: {error}");
                kept.renderer.clear();
            }
        }
        kept.renderer.loaded_key = Some(key);
        kept.tiles.set_vec(Vec::new());
    }
}

/// A thread row's message kept open, as the row shows it.
#[derive(Clone, Default)]
pub(crate) struct KeptRow {
    pub(crate) layout: KeptOpenRow,
    pub(crate) width_ratio: f32,
    pub(crate) tiles: ModelRc<EmailTile>,
}

/// Each row of the conversation, in its order.
pub(super) fn rows(state: &InboxState, viewport_width: f32) -> Vec<KeptRow> {
    let thread: Vec<i32> = state
        .conversation_messages
        .iter()
        .map(|message| message.id)
        .collect();
    let kept = &state.kept;
    kept_open_rows(&thread, &kept.open, |id| kept.size(id, viewport_width))
        .into_iter()
        .zip(&thread)
        .map(|(layout, &id)| KeptRow {
            width_ratio: kept.width_ratio(id, viewport_width),
            tiles: if layout.open {
                kept.tiles(id)
            } else {
                ModelRc::default()
            },
            layout,
        })
        .collect()
}

/// The part of a kept message's body in view changed.
pub(super) fn set_region(state: &mut InboxState, index: usize, start: f32, height: f32) {
    let Some(id) = state
        .conversation_messages
        .get(index)
        .map(|message| message.id)
    else {
        return;
    };
    if let Some(message) = state.kept.kept.iter_mut().find(|message| message.id == id) {
        message.region = (start.max(0.0), height.max(0.0));
    }
}

/// Paint the kept messages in view and release the ones out of view.
/// Returns whether the rows need building again.
pub(super) fn paint(app: &AppWindow, state: &mut InboxState) -> bool {
    let (width, height) = email_viewport_size(&ReaderRef::main(app));
    let scale = app.window().scale_factor();
    // Every message is painted or released, so this doesn't stop early.
    let mut changed = false;
    for message in &mut state.kept.kept {
        changed |= message.paint(width, height, scale);
    }
    changed
}

/// A conversation that loads again brings only its selected message's body,
/// so messages still open keep the bodies they had.
pub(super) fn keep_bodies(
    kept: &KeptMessages,
    previous: &[MailMessage],
    loaded: &mut [MailMessage],
) {
    for message in loaded
        .iter_mut()
        .filter(|message| message.html.is_none() && kept.open.is_open(message.id))
    {
        if let Some(before) = previous
            .iter()
            .find(|before| before.id == message.id && before.html.is_some())
        {
            message.html = before.html.clone();
            message.text = before.text.clone();
            message.body_pending = false;
        }
    }
}
