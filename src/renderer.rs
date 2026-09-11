mod interaction;
#[cfg(test)]
mod regression;
mod semantics;
use anyrender::ImageRenderer;
#[cfg(test)]
use anyrender::render_to_buffer;
use blitz_dom::{Document, DocumentConfig, Point as DomPoint, local_name};
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_traits::net::NetWaker;
use blitz_traits::{
    SmolStr,
    net::NetProvider,
    node_id::NodeId,
    shell::{ColorScheme, Viewport},
};
use keyboard_types::Key;
use parley::layout::PositionedLayoutItem;
use semantics::*;
#[cfg(feature = "gpu-renderer")]
use slint::wgpu_29::wgpu;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(feature = "gpu-renderer")]
use anyrender_vello::VelloScenePainter;
use anyrender_vello_cpu::VelloCpuImageRenderer;
#[cfg(feature = "gpu-renderer")]
use std::num::NonZeroUsize;

const INITIAL_WIDTH: u32 = 520;
const INITIAL_HEIGHT: u32 = 900;
const MIN_EMAIL_SURFACE_HEIGHT: f32 = 64.0;
const EMAIL_SURFACE_BOTTOM_PAD: f32 = 12.0;
const EMAIL_TILE_HEIGHT: f32 = 512.0;
const EMAIL_TILE_OVERSCAN: u32 = 1;
const MAX_EMAIL_SURFACE_HEIGHT: f32 = 100_000.0;

const EMAIL_FONT_FALLBACK_STYLE: &str = r#"
<style data-flectar-mail="font-fallback">
  html, body {
    font-family: Arial, "Liberation Sans", "Noto Sans", sans-serif;
  }
  pre, code, kbd, samp {
    font-family: "DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono", monospace;
  }
</style>
"#;

#[derive(Debug, Clone, PartialEq)]
pub struct EmailLink {
    /// Coordinates are normalized against the current logical Blitz layout
    /// width. The Slint overlay uses the same width when it is resized.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub url: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InputModifiers {
    control: bool,
    shift: bool,
    alt: bool,
    meta: bool,
}

impl InputModifiers {
    pub const fn new(control: bool, shift: bool, alt: bool, meta: bool) -> Self {
        Self {
            control,
            shift,
            alt,
            meta,
        }
    }
}

pub struct PreparedEmail {
    document: HtmlDocument,
    pub links: Vec<EmailLink>,
    pub plain_text: String,
    pub notice: Option<String>,
    abort: Option<blitz_traits::net::AbortController>,
    resolved_size: (u32, u32, u32, u32, u32),
}

impl Drop for PreparedEmail {
    fn drop(&mut self) {
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
    }
}

#[derive(Clone)]
pub struct RenderedEmailTile {
    pub image: slint::Image,
    /// Normalized against the logical email width, like link coordinates.
    pub y: f32,
    pub height: f32,
}

pub struct RenderedEmail {
    pub tiles: Vec<RenderedEmailTile>,
    pub width: u32,
    pub height: u32,
    pub links: Vec<EmailLink>,
}

pub struct GpuEmailRenderer {
    email: Option<PreparedEmail>,
    #[cfg(feature = "gpu-renderer")]
    renderer: Option<vello::Renderer>,
    #[cfg(feature = "gpu-renderer")]
    scene: vello::Scene,
    last_size: Option<(u32, u32, u32, u32, u32)>,
    dirty: bool,
    paint_dirty: bool,
    painted_selection: Vec<(NodeId, usize, usize)>,
    cpu_painter: Option<VelloCpuImageRenderer>,
    cpu_size: (u32, u32),
    pub preparation_viewport: (u32, u32, f32),
    pub loaded_key: Option<(u64, bool)>,
    pub zoom: f32,
    pub auto_fit: bool,
    fit_viewport: Option<(u32, u32)>,
    pub layout_width: f32,
    pub notice: Option<String>,
    pub metadata_revision: u64,
    pub layout_count: u64,
    pub tile_count: u64,
    active_document: Arc<AtomicUsize>,
    resources: crate::remote::ResourceLedger,
    pub(crate) press_link: Option<(f32, f32, String)>,
    pub(crate) activation: Option<String>,
    pub(crate) selection_anchor: Option<(NodeId, usize)>,
    pub(crate) selection_focus: Option<(NodeId, usize)>,
    pub(crate) click_count: u8,
    pub(crate) find_matches: Vec<(NodeId, usize, usize)>,
    pub(crate) find_index: usize,
    pub(crate) find_query: String,
    pub(crate) find_revision: u64,

    region_dirty: bool,
    content_height: f32,
    visible_scroll_y: f32,
    visible_height: f32,
    tiles: BTreeMap<u32, RenderedEmailTile>,
    pointer_down: bool,
    last_pointer_down: Option<(Instant, f32, f32)>,
    resource_runtime: Option<tokio::runtime::Handle>,
    net_provider: Option<Arc<dyn NetProvider>>,
    net_waker: Option<Arc<dyn NetWaker>>,
    remote_resources_enabled: bool,
    resource_poll_ticks: Arc<AtomicU8>,
    resource_notifier: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for GpuEmailRenderer {
    fn default() -> Self {
        Self {
            email: None,
            #[cfg(feature = "gpu-renderer")]
            renderer: None,
            #[cfg(feature = "gpu-renderer")]
            scene: vello::Scene::new(),
            last_size: None,
            dirty: false,
            paint_dirty: false,
            painted_selection: Vec::new(),
            cpu_painter: None,
            cpu_size: (0, 0),
            preparation_viewport: (INITIAL_WIDTH, INITIAL_HEIGHT, 1.0),
            loaded_key: None,
            zoom: 1.0,
            auto_fit: false,
            fit_viewport: None,
            layout_width: 520.0,
            notice: None,
            metadata_revision: 0,
            layout_count: 0,
            tile_count: 0,
            active_document: Arc::new(AtomicUsize::new(usize::MAX)),
            resources: Arc::default(),
            press_link: None,
            activation: None,
            selection_anchor: None,
            selection_focus: None,
            click_count: 0,
            find_matches: Vec::new(),
            find_index: 0,
            find_query: String::new(),
            find_revision: 0,
            region_dirty: false,
            content_height: MIN_EMAIL_SURFACE_HEIGHT,
            visible_scroll_y: 0.0,
            visible_height: INITIAL_HEIGHT as f32,
            tiles: BTreeMap::new(),
            pointer_down: false,
            last_pointer_down: None,
            resource_runtime: None,
            net_provider: None,
            net_waker: None,
            remote_resources_enabled: false,
            resource_poll_ticks: Arc::new(AtomicU8::new(0)),
            resource_notifier: None,
        }
    }
}

impl GpuEmailRenderer {
    /// Notify the UI event loop when Blitz has completed a resource request.
    /// This avoids an always-running timer while keeping the DOM on its owning
    /// Slint thread.
    pub fn set_resource_notifier(&mut self, notifier: Arc<dyn Fn() + Send + Sync>) {
        self.resource_notifier = Some(notifier);
    }

    /// Enable Blitz sub-resource loading on the application's existing Tokio
    /// runtime. The short poll window handles image decode completion without
    /// making the Slint render loop spin continuously.
    pub fn configure_resources(
        &mut self,
        runtime: tokio::runtime::Handle,
        allow_remote: bool,
    ) -> Result<(), String> {
        let poll_ticks = Arc::clone(&self.resource_poll_ticks);
        let notifier = self.resource_notifier.clone();
        let active = self.active_document.clone();
        let waker: Arc<dyn NetWaker> = Arc::new(move |document_id| {
            if active.load(Ordering::Acquire) != document_id {
                return;
            }
            poll_ticks.store(1, Ordering::Release);
            if let Some(notifier) = notifier.as_ref() {
                notifier();
            }
        });
        self.net_provider = Some(crate::remote::email_image_provider_tracked(
            Arc::clone(&waker),
            allow_remote,
            self.resources.clone(),
        )?);
        self.net_waker = Some(waker);
        self.remote_resources_enabled = allow_remote;
        self.resource_runtime = Some(runtime);
        Ok(())
    }

    pub fn prepare_email_html(
        &self,
        html: &str,
        allow_remote_override: bool,
    ) -> Result<PreparedEmail, String> {
        let _runtime = self
            .resource_runtime
            .as_ref()
            .map(tokio::runtime::Handle::enter);
        let provider = if allow_remote_override && !self.remote_resources_enabled {
            self.net_waker
                .as_ref()
                .map(|waker| {
                    crate::remote::email_image_provider_tracked(
                        Arc::clone(waker),
                        true,
                        self.resources.clone(),
                    )
                })
                .transpose()?
        } else {
            self.net_provider.clone()
        };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prepare_email_html_at(
                html,
                provider,
                self.preparation_viewport.0,
                self.preparation_viewport.1,
                self.preparation_viewport.2 * self.zoom,
            )
        }))
        .unwrap_or_else(|_| {
            Err("This message could not be laid out. Use the plain text view.".into())
        })
    }

    /// Incorporate completed image/font requests into the retained DOM.
    /// Returns true while a bounded post-load polling window is active.
    pub fn poll_resources(&mut self) -> bool {
        let ticks = self.resource_poll_ticks.load(Ordering::Acquire);
        if ticks == 0 {
            return false;
        }
        self.resource_poll_ticks
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                Some(value.saturating_sub(1))
            })
            .ok();

        let _runtime = self
            .resource_runtime
            .as_ref()
            .map(tokio::runtime::Handle::enter);
        if let Some(email) = self.email.as_mut() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                email.document.drain_pending_messages()
            }));
            match result {
                Ok(changed) => {
                    self.dirty |= changed;
                    self.metadata_revision += 1;
                }
                Err(_) => {
                    self.clear();
                    self.notice = Some("Image processing failed. Use the plain text view.".into());
                }
            }
        }
        true
    }

    pub fn set_email(&mut self, email: PreparedEmail) {
        self.fit_viewport = None;
        self.resources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(id, _), _| *id == email.document.id());
        self.active_document
            .store(email.document.id(), Ordering::Release);
        self.notice = email.notice.clone();
        self.last_size = Some(email.resolved_size);
        self.content_height = content_surface_height(&email.document);
        self.layout_width =
            content_surface_width(&email.document, f32::from_bits(email.resolved_size.3))
                * self.zoom;
        if self.content_height >= MAX_EMAIL_SURFACE_HEIGHT
            || self.layout_width / self.zoom >= 4096.0
        {
            self.notice = Some("Large message surface limited. Reader or plain text view contains the complete text.".into());
        }
        self.email = Some(email);
        self.layout_count += 1;
        self.metadata_revision += 1;
        self.selection_anchor = None;
        self.selection_focus = None;
        self.painted_selection.clear();
        self.find_matches.clear();
        self.find_query.clear();
        self.cpu_painter = None;
        // Completions may have arrived while the document was being prepared.
        self.resource_poll_ticks.store(1, Ordering::Release);
        self.dirty = false;
        self.paint_dirty = false;
        self.region_dirty = true;
        self.visible_scroll_y = 0.0;
        self.tiles.clear();
        self.pointer_down = false;
        self.press_link = None;
        self.activation = None;
        self.click_count = 0;
        self.last_pointer_down = None;
    }

    pub fn clear(&mut self) {
        self.active_document.store(usize::MAX, Ordering::Release);
        self.email = None;
        self.resources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.find_matches.clear();
        self.find_query.clear();
        self.loaded_key = None;
        self.paint_dirty = false;
        self.painted_selection.clear();
        self.selection_anchor = None;
        self.selection_focus = None;
        self.metadata_revision += 1;
        self.resource_poll_ticks.store(0, Ordering::Release);
        self.cpu_painter = None;
        self.last_size = None;
        self.dirty = false;
        self.region_dirty = false;
        self.tiles.clear();
        self.pointer_down = false;
        self.press_link = None;
        self.activation = None;
        self.click_count = 0;
        self.last_pointer_down = None;
    }

    /// Drop resources tied to Slint's WGPU device. The next frame will build
    /// the small Vello pipeline again against the new device after a suspend,
    /// display change, or Android surface recreation.
    #[cfg(feature = "gpu-renderer")]
    pub fn teardown_gpu(&mut self) {
        self.renderer = None;
        self.last_size = None;
        self.dirty = self.email.is_some();
        self.region_dirty = self.email.is_some();
        self.tiles.clear();
    }

    /// Update the Slint scroll window. Rendering is requested only when the
    /// viewport crosses into a tile that is not already in the bounded cache.
    pub fn set_visible_region(&mut self, scroll_y: f32, viewport_height: f32) -> bool {
        let scroll_y = (scroll_y / self.zoom).max(0.0);
        let viewport_height = (viewport_height / self.zoom).max(1.0);
        self.visible_scroll_y = scroll_y;
        self.visible_height = viewport_height;

        let desired = desired_tile_range(scroll_y, viewport_height, self.content_height);
        let missing = desired
            .clone()
            .any(|index| !self.tiles.contains_key(&index));
        let stale = self
            .tiles
            .keys()
            .any(|index| !desired.clone().any(|wanted| wanted == *index));
        self.region_dirty |= missing || stale;
        self.region_dirty
    }

    /// Forward a Slint pointer event into Blitz's normal DOM event driver.
    /// Blitz owns hit testing and selection geometry; Slint only supplies the
    /// pointer coordinates in the email content's logical coordinate space.
    pub fn handle_pointer_event(
        &mut self,
        x: f32,
        y: f32,
        kind: &str,
        input_modifiers: InputModifiers,
    ) -> bool {
        self.pointer_input(x / self.zoom, y / self.zoom, kind, input_modifiers)
    }

    /// Forward a Slint keyboard event. Returns selected text when the DOM's
    /// copy shortcut is pressed so the UI can perform the platform clipboard
    /// operation through Slint's native clipboard support.
    pub fn handle_key_event(
        &mut self,
        text: &str,
        pressed: bool,
        _repeat: bool,
        input_modifiers: InputModifiers,
    ) -> Option<String> {
        if !pressed {
            return None;
        }
        let (key, _) = slint_key_to_blitz_key(text);
        if input_modifiers.control || input_modifiers.meta {
            if text.eq_ignore_ascii_case("a") {
                self.select_all();
                return None;
            }
            if text.eq_ignore_ascii_case("c") {
                return self.selected_text();
            }
        }
        self.move_selection(key, input_modifiers);
        None
    }

    /// Select all inline text in the retained Blitz document.
    pub fn select_all(&mut self) -> bool {
        let Some(email) = self.email.as_mut() else {
            return false;
        };

        let mut inline_roots = Vec::new();
        email.document.visit(|node_id, node| {
            let Some(element) = node.element_data() else {
                return;
            };
            let Some(layout) = element.inline_layout_data.as_deref() else {
                return;
            };
            if !layout.text.is_empty() {
                inline_roots.push((node_id, layout.text.len()));
            }
        });

        let Some(&(first_node, _)) = inline_roots.first() else {
            return false;
        };
        let Some(&(last_node, last_len)) = inline_roots.last() else {
            return false;
        };
        email
            .document
            .set_text_selection(first_node, 0, last_node, last_len);
        self.selection_anchor = Some((first_node, 0));
        self.selection_focus = Some((last_node, last_len));
        self.paint_dirty = true;
        true
    }

    pub fn selected_text(&self) -> Option<String> {
        self.email
            .as_ref()
            .and_then(|email| email.document.get_selected_text())
    }

    pub fn has_selection(&self) -> bool {
        self.email
            .as_ref()
            .is_some_and(|email| email.document.has_text_selection())
    }

    pub fn has_document(&self) -> bool {
        self.email.is_some()
    }

    pub fn needs_repaint(&self) -> bool {
        self.dirty || self.paint_dirty || self.region_dirty
    }

    /// Render the retained document through the compatibility CPU painter.
    /// This is used only when Slint could not create a WGPU device, but it
    /// shares the same DOM and selection state as the GPU path.
    pub fn set_auto_fit(&mut self, enabled: bool) {
        self.auto_fit = enabled;
        self.fit_viewport = None;
        self.dirty = true;
    }

    fn fit_to_viewport(&mut self, width: u32, height: u32) {
        let _runtime = self
            .resource_runtime
            .as_ref()
            .map(tokio::runtime::Handle::enter);
        if !self.auto_fit {
            return;
        }
        let Some(email) = self.email.as_mut() else {
            return;
        };
        self.dirty |= email.document.drain_pending_messages();
        if !self.dirty && self.fit_viewport == Some((width, height)) {
            return;
        }
        // Measure authored overflow at 100%, then lay out the final viewport
        // at the chosen scale. Recompute when resized or resources change.
        email.document.set_viewport(Viewport::new(
            width.max(1),
            height.max(1),
            1.0,
            ColorScheme::Light,
        ));
        email.document.resolve(0.0);
        let natural = content_surface_width(&email.document, width.max(1) as f32);
        self.zoom = ((width.max(1) as f32) / natural.max(1.0)).clamp(0.1, 1.0);
        self.fit_viewport = Some((width, height));
        self.last_size = None;
        self.dirty = true;
    }

    pub fn render_cpu_if_needed(
        &mut self,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<Option<RenderedEmail>, String> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.fit_to_viewport(width, height);
            self.render_cpu_inner(width, height, scale)
        })) {
            Ok(result) => result,
            Err(_) => {
                self.clear();
                self.notice =
                    Some("This message could not be rendered. Use the plain text view.".into());
                Err(self.notice.clone().unwrap())
            }
        }
    }

    fn render_cpu_inner(
        &mut self,
        logical_width: u32,
        logical_height: u32,
        scale_factor: f32,
    ) -> Result<Option<RenderedEmail>, String> {
        let _runtime = self
            .resource_runtime
            .as_ref()
            .map(tokio::runtime::Handle::enter);
        let Some(email) = self.email.as_mut() else {
            return Ok(None);
        };
        self.dirty |= email.document.drain_pending_messages();

        let logical_width = ((logical_width as f32 / self.zoom).max(1.0)).ceil() as u32;
        let logical_height = ((logical_height as f32 / self.zoom).max(1.0)).ceil() as u32;
        let scale_factor = scale_factor.clamp(0.5, 4.0) * self.zoom;
        let mut physical_width = ((logical_width.max(1) as f32) * scale_factor).ceil() as u32;
        let physical_height = ((logical_height.max(1) as f32) * scale_factor).ceil() as u32;
        let size = (
            physical_width,
            physical_height,
            scale_factor.to_bits(),
            (logical_width as f32).to_bits(),
            (logical_height as f32).to_bits(),
        );
        self.dirty |= email.document.drain_pending_messages();
        let needs_layout = self.dirty || self.last_size != Some(size);
        if !needs_layout && !self.paint_dirty && !self.region_dirty {
            return Ok(None);
        }

        if self.paint_dirty {
            invalidate_selection_tiles(&email.document, &self.painted_selection, &mut self.tiles);
        }
        if needs_layout {
            self.layout_count += 1;
            self.metadata_revision += 1;
            email.document.set_viewport(Viewport::new(
                physical_width,
                physical_height,
                scale_factor,
                ColorScheme::Light,
            ));
            email.document.resolve(0.0);
            email.links = collect_email_links(&email.document, logical_width.max(1) as f32);
            self.content_height = content_surface_height(&email.document);
            self.layout_width =
                content_surface_width(&email.document, logical_width as f32) * self.zoom;
            if self.content_height >= MAX_EMAIL_SURFACE_HEIGHT
                || self.layout_width / self.zoom >= 4096.0
            {
                self.notice = Some("Large message surface limited. Reader or plain text view contains the complete text.".into());
            }
            self.tiles.clear();
        }

        let canvas_width = (self.layout_width / self.zoom).max(logical_width as f32);
        physical_width = (canvas_width * scale_factor).ceil() as u32;
        let wanted = desired_tile_range(
            self.visible_scroll_y,
            self.visible_height.min(logical_height.max(1) as f32),
            self.content_height,
        );
        self.tiles
            .retain(|index, _| wanted.clone().any(|wanted| wanted == *index));
        for index in wanted {
            if self.tiles.contains_key(&index) {
                continue;
            }
            let tile = render_cpu_tile_cached(
                &mut self.cpu_painter,
                &mut self.cpu_size,
                email,
                canvas_width,
                self.content_height,
                index,
                scale_factor,
            )?;
            self.tile_count += 1;
            self.tiles.insert(index, tile);
        }

        self.last_size = Some(size);
        self.dirty = false;
        self.paint_dirty = false;
        self.painted_selection = email.document.get_text_selection_ranges();
        self.region_dirty = false;
        Ok(Some(RenderedEmail {
            tiles: self.tiles.values().cloned().collect(),
            width: physical_width,
            height: (self.content_height * scale_factor).ceil().max(1.0) as u32,
            links: email.links.clone(),
        }))
    }

    #[cfg(feature = "gpu-renderer")]
    pub fn render_if_needed(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: f32,
        height: f32,
        scale: f32,
    ) -> Result<Option<RenderedEmail>, String> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.fit_to_viewport(width.max(1.0) as u32, height.max(1.0) as u32);
            self.render_gpu_inner(device, queue, width, height, scale)
        })) {
            Ok(result) => result,
            Err(_) => {
                self.clear();
                self.notice =
                    Some("This message could not be rendered. Use the plain text view.".into());
                Err(self.notice.clone().unwrap())
            }
        }
    }

    #[cfg(feature = "gpu-renderer")]
    fn render_gpu_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        logical_width: f32,
        logical_height: f32,
        scale_factor: f32,
    ) -> Result<Option<RenderedEmail>, String> {
        let Some(email) = self.email.as_mut() else {
            return Ok(None);
        };

        let logical_width = (logical_width / self.zoom).max(1.0);
        let logical_height = (logical_height / self.zoom).max(1.0);
        let scale_factor = scale_factor.clamp(0.5, 4.0) * self.zoom;
        let mut physical_width = (logical_width * scale_factor).ceil() as u32;
        let physical_height = (logical_height * scale_factor).ceil() as u32;
        let size = (
            physical_width,
            physical_height,
            scale_factor.to_bits(),
            logical_width.to_bits(),
            logical_height.to_bits(),
        );

        self.dirty |= email.document.drain_pending_messages();
        let needs_layout = self.dirty || self.last_size != Some(size);
        if !needs_layout && !self.paint_dirty && !self.region_dirty {
            return Ok(None);
        }
        let _runtime = self
            .resource_runtime
            .as_ref()
            .map(tokio::runtime::Handle::enter);
        if self.paint_dirty {
            invalidate_selection_tiles(&email.document, &self.painted_selection, &mut self.tiles);
        }
        if needs_layout {
            self.layout_count += 1;
            self.metadata_revision += 1;
            email.document.set_viewport(Viewport::new(
                physical_width,
                physical_height,
                scale_factor,
                ColorScheme::Light,
            ));
            email.document.resolve(0.0);
            email.links = collect_email_links(&email.document, logical_width);
            self.content_height = content_surface_height(&email.document);
            self.layout_width = content_surface_width(&email.document, logical_width) * self.zoom;
            if self.content_height >= MAX_EMAIL_SURFACE_HEIGHT
                || self.layout_width / self.zoom >= 4096.0
            {
                self.notice = Some("Large message surface limited. Reader or plain text view contains the complete text.".into());
            }
            self.tiles.clear();
        }

        let logical_width = (self.layout_width / self.zoom).max(logical_width);
        physical_width = (logical_width * scale_factor).ceil() as u32;
        if self.renderer.is_none() {
            self.renderer = Some(
                vello::Renderer::new(
                    device,
                    vello::RendererOptions {
                        use_cpu: false,
                        // Area AA is the lowest-memory Vello pipeline and is
                        // sufficient for email text and vector decoration.
                        antialiasing_support: vello::AaSupport::area_only(),
                        num_init_threads: NonZeroUsize::new(1),
                        pipeline_cache: None,
                    },
                )
                .map_err(|error| format!("could not initialize Vello GPU renderer: {error:?}"))?,
            );
        }

        let wanted = desired_tile_range(
            self.visible_scroll_y,
            self.visible_height.min(logical_height),
            self.content_height,
        );
        self.tiles
            .retain(|index, _| wanted.clone().any(|wanted| wanted == *index));
        for index in wanted {
            if self.tiles.contains_key(&index) {
                continue;
            }
            let logical_y = index as f32 * EMAIL_TILE_HEIGHT;
            let logical_tile_height =
                (self.content_height - logical_y).clamp(1.0, EMAIL_TILE_HEIGHT);
            let physical_tile_height = (logical_tile_height * scale_factor).ceil() as u32;
            let limit = device.limits().max_texture_dimension_2d;
            if physical_width > limit || physical_tile_height > limit {
                return Err(
                    "Email surface exceeds GPU limits. Use reader or plain text view.".into(),
                );
            }
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("flectar-mail-email-tile"),
                size: wgpu::Extent3d {
                    width: physical_width,
                    height: physical_tile_height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

            email.document.set_viewport_scroll(DomPoint {
                x: 0.0,
                y: logical_y as f64,
            });
            let mut painter = VelloScenePainter::new(&mut self.scene);
            paint_scene(
                &mut painter,
                &mut email.document,
                scale_factor as f64,
                physical_width,
                physical_tile_height,
                0,
                0,
            );
            email.document.set_viewport_scroll(DomPoint::ZERO);

            let render_result = self
                .renderer
                .as_mut()
                .ok_or_else(|| "Vello renderer was not initialized".to_owned())?
                .render_to_texture(
                    device,
                    queue,
                    &self.scene,
                    &texture_view,
                    &vello::RenderParams {
                        base_color: vello::peniko::Color::WHITE,
                        width: physical_width,
                        height: physical_tile_height,
                        antialiasing_method: vello::AaConfig::Area,
                    },
                );
            self.scene.reset();
            if let Err(error) = render_result {
                self.renderer = None;
                return Err(format!("could not render email tile with Vello: {error:?}"));
            }
            let image = slint::Image::try_from(texture)
                .map_err(|error| format!("could not import email tile into Slint: {error}"))?;
            self.tiles.insert(
                index,
                RenderedEmailTile {
                    image,
                    y: logical_y / logical_width,
                    height: logical_tile_height / logical_width,
                },
            );
        }
        self.last_size = Some(size);
        self.dirty = false;
        self.paint_dirty = false;
        self.painted_selection = email.document.get_text_selection_ranges();
        self.region_dirty = false;

        Ok(Some(RenderedEmail {
            tiles: self.tiles.values().cloned().collect(),
            width: physical_width,
            height: (self.content_height * scale_factor).ceil().max(1.0) as u32,
            links: email.links.clone(),
        }))
    }
}

fn slint_key_to_blitz_key(text: &str) -> (Key, Option<SmolStr>) {
    let key = match text.chars().next() {
        Some('\u{0008}') => Key::Backspace,
        Some('\u{0009}') => Key::Tab,
        Some('\u{000a}') => Key::Enter,
        Some('\u{001b}') => Key::Escape,
        Some('\u{007f}') => Key::Delete,
        Some('\u{0010}') => Key::Shift,
        Some('\u{0011}') => Key::Control,
        Some('\u{0012}') => Key::Alt,
        Some('\u{0017}') | Some('\u{0018}') => Key::Meta,
        Some('\u{f700}') => Key::ArrowUp,
        Some('\u{f701}') => Key::ArrowDown,
        Some('\u{f702}') => Key::ArrowLeft,
        Some('\u{f703}') => Key::ArrowRight,
        Some('\u{f729}') => Key::Home,
        Some('\u{f72b}') => Key::End,
        Some('\u{f72c}') => Key::PageUp,
        Some('\u{f72d}') => Key::PageDown,
        _ => Key::Character(text.to_owned()),
    };
    let text_value = match &key {
        Key::Character(value) => Some(value.as_str().into()),
        _ => None,
    };
    (key, text_value)
}

#[cfg(test)]
pub fn render_prepared_cpu(
    email: &mut PreparedEmail,
    logical_width: u32,
    logical_height: u32,
    scale_factor: f32,
) -> Result<RenderedEmail, String> {
    let scale_factor = scale_factor.max(1.0);
    let physical_width = ((logical_width.max(1) as f32) * scale_factor).ceil() as u32;
    let physical_height = ((logical_height.max(1) as f32) * scale_factor).ceil() as u32;
    email.document.set_viewport(Viewport::new(
        physical_width,
        physical_height,
        scale_factor,
        ColorScheme::Light,
    ));
    email.document.resolve(0.0);
    email.links = collect_email_links(&email.document, logical_width.max(1) as f32);
    let rendered_height = content_surface_height(&email.document);
    let rendered_physical_height = (rendered_height * scale_factor).ceil().max(1.0) as u32;
    let last_tile = ((rendered_height / EMAIL_TILE_HEIGHT).ceil() as u32).saturating_sub(1);
    let mut tiles = Vec::with_capacity(last_tile as usize + 1);
    for index in 0..=last_tile {
        tiles.push(render_cpu_tile(
            email,
            logical_width.max(1) as f32,
            rendered_height,
            index,
            scale_factor,
        )?);
    }

    Ok(RenderedEmail {
        tiles,
        width: physical_width,
        height: rendered_physical_height,
        links: email.links.clone(),
    })
}

#[cfg(test)]
fn render_cpu_tile(
    email: &mut PreparedEmail,
    logical_width: f32,
    content_height: f32,
    index: u32,
    scale_factor: f32,
) -> Result<RenderedEmailTile, String> {
    render_cpu_tile_cached(
        &mut None,
        &mut (0, 0),
        email,
        logical_width,
        content_height,
        index,
        scale_factor,
    )
}

fn render_cpu_tile_cached(
    painter: &mut Option<VelloCpuImageRenderer>,
    size: &mut (u32, u32),
    email: &mut PreparedEmail,
    logical_width: f32,
    content_height: f32,
    index: u32,
    scale_factor: f32,
) -> Result<RenderedEmailTile, String> {
    let logical_y = index as f32 * EMAIL_TILE_HEIGHT;
    let logical_tile_height = (content_height - logical_y).clamp(1.0, EMAIL_TILE_HEIGHT);
    let physical_width = (logical_width * scale_factor).ceil().max(1.0) as u32;
    let physical_tile_height = (logical_tile_height * scale_factor).ceil().max(1.0) as u32;
    if physical_width > 16384 || physical_tile_height > 8192 {
        return Err("Email surface exceeds rendering limits".into());
    }
    let mut pixels =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(physical_width, physical_tile_height);
    if painter.is_none() || *size != (physical_width, physical_tile_height) {
        *painter = Some(VelloCpuImageRenderer::new(
            physical_width,
            physical_tile_height,
        ));
        *size = (physical_width, physical_tile_height);
    }
    let renderer = painter.as_mut().unwrap();
    renderer.reset();

    email.document.set_viewport_scroll(DomPoint {
        x: 0.0,
        y: logical_y as f64,
    });
    renderer.render(
        |scene| {
            paint_scene(
                scene,
                &mut email.document,
                scale_factor as f64,
                physical_width,
                physical_tile_height,
                0,
                0,
            );
        },
        pixels.make_mut_bytes(),
    );
    email.document.set_viewport_scroll(DomPoint::ZERO);

    // Blitz may leave pixels beyond the document's own painted boxes
    // transparent. Every tile represents an opaque browser canvas.
    composite_over_white(pixels.make_mut_bytes());
    Ok(RenderedEmailTile {
        image: slint::Image::from_rgba8(pixels),
        y: logical_y / logical_width,
        height: logical_tile_height / logical_width,
    })
}

fn invalidate_selection_tiles(
    document: &HtmlDocument,
    previous: &[(NodeId, usize, usize)],
    tiles: &mut BTreeMap<u32, RenderedEmailTile>,
) {
    let current = document.get_text_selection_ranges();
    let changed = previous
        .iter()
        .filter(|r| !current.contains(r))
        .chain(current.iter().filter(|r| !previous.contains(r)));
    for &(id, _, _) in changed {
        let Some(node) = document.get_node(id) else {
            tiles.clear();
            return;
        };
        let y = node.absolute_position(0.0, 0.0).y;
        let end = y + node.final_layout().size.height;
        tiles.retain(|index, _| {
            let top = *index as f32 * EMAIL_TILE_HEIGHT;
            top + EMAIL_TILE_HEIGHT < y || top > end
        });
    }
}

fn desired_tile_range(
    scroll_y: f32,
    viewport_height: f32,
    content_height: f32,
) -> std::ops::RangeInclusive<u32> {
    let last = ((content_height.max(1.0) / EMAIL_TILE_HEIGHT).ceil() as u32).saturating_sub(1);
    let first_visible = (scroll_y.max(0.0) / EMAIL_TILE_HEIGHT).floor() as u32;
    let last_visible =
        ((scroll_y.max(0.0) + viewport_height.max(1.0)) / EMAIL_TILE_HEIGHT).floor() as u32;
    let first = first_visible.saturating_sub(EMAIL_TILE_OVERSCAN).min(last);
    let end = last_visible.saturating_add(EMAIL_TILE_OVERSCAN).min(last);
    first..=end.max(first)
}

/// Find the bottom-most real document box after Blitz has resolved layout.
///
/// The html/body boxes stretch to the visible viewport, so counting them would
/// add empty space to short messages. Descendant boxes retain the real content
/// extent; that extent determines the scroll range and tile count. Remote
/// resources invalidate the layout and recalculate this bound when their
/// intrinsic dimensions arrive.
fn content_surface_width(document: &HtmlDocument, viewport: f32) -> f32 {
    let mut width = viewport;
    document.visit(|_, node| {
        if node.stylo_element_data_opt().is_some() && visible(node) {
            let layout = node.final_layout();
            let right = node.absolute_position(0.0, 0.0).x
                + layout.size.width.max(layout.scrollable_overflow_rect.right);
            if right.is_finite() {
                width = width.max(right);
            }
        }
    });
    width.clamp(viewport, viewport.max(4096.0))
}

fn content_surface_height(document: &HtmlDocument) -> f32 {
    let mut content_bottom = 0.0_f32;

    document.visit(|_, node| {
        // Blitz beta.2 stores box geometry only on document, element, and
        // anonymous-block nodes. Text and comment nodes are represented by
        // their parent's inline layout and must not be queried directly.
        if node.stylo_element_data_opt().is_none() {
            return;
        }
        let is_viewport_box = node.data.is_element_with_tag_name(&local_name!("html"))
            || node.data.is_element_with_tag_name(&local_name!("body"));
        let layout = node.final_layout();
        let origin = node.absolute_position(0.0, 0.0);

        if !is_viewport_box && layout.size.height.is_finite() && origin.y.is_finite() {
            content_bottom = content_bottom
                .max(origin.y + layout.size.height.max(0.0) + layout.margin.bottom.max(0.0));
        }

        // Text directly inside body/html is represented by inline layout data
        // on the viewport box rather than by a separate child element.
        if let Some(inline) = node
            .element_data()
            .and_then(|element| element.inline_layout_data.as_deref())
        {
            let inline_origin = origin.y + layout.border.top + layout.padding.top;
            for line in inline.layout.lines() {
                content_bottom = content_bottom.max(
                    inline_origin + line.metrics().block_max_coord.max(0.0) / inline.layout.scale(),
                );
            }
        }
    });

    (content_bottom + EMAIL_SURFACE_BOTTOM_PAD)
        .clamp(MIN_EMAIL_SURFACE_HEIGHT, MAX_EMAIL_SURFACE_HEIGHT)
}

fn composite_over_white(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        if alpha < 255 {
            let inverse = 255 - alpha;
            for channel in &mut pixel[..3] {
                *channel = (u16::from(*channel) + inverse).min(255) as u8;
            }
            pixel[3] = 255;
        }
    }
}

/// Parse sanitized email HTML into a retained Blitz DOM. The DOM is painted
/// later by `GpuEmailRenderer` after Slint exposes its WGPU device and queue.
#[cfg(test)]
pub fn prepare_email_html(html: &str) -> Result<PreparedEmail, String> {
    prepare_email_html_with_provider(html, None)
}

/// Build the Parley `FontContext` used for email bodies.
///
/// On macOS, fontique maps the "sans-serif" generic family to "Helvetica".
/// But macOS's own `Helvetica.ttc` (distinct from `HelveticaNeue.ttc`)
/// digitizes "|" sitting flush on the baseline instead of dipping below it
/// like every other tested family — confirmed directly against CoreText, so
/// it's a property of that font file, not a bug in our stack. It renders
/// squashed, reading as "l". Prefer "Helvetica Neue" for the generic family
/// when it's installed: same type family, unaffected glyph.
///
/// The `FontContext`/`Collection`/`BULLET_FONT` bootstrap below mirrors the
/// default-construction branch in `BaseDocument::new`
/// (`crates/blitz-dom/src/document.rs`, the `unwrap_or_else` when
/// `DocumentConfig.font_ctx` is `None`) — that crate is a vendored fork
/// wholesale-replaced on updates, so the logic can't be shared by extracting
/// a helper there. Keep the two in sync by hand if either changes.
fn build_email_font_ctx() -> parley::FontContext {
    thread_local! { static FONTS: parley::FontContext = create_email_font_ctx(); }
    FONTS.with(Clone::clone)
}
fn create_email_font_ctx() -> parley::FontContext {
    use parley::fontique::{Blob, Collection, CollectionOptions, GenericFamily, SourceCache};

    let mut font_ctx = parley::FontContext {
        source_cache: SourceCache::new_shared(),
        collection: Collection::new(CollectionOptions {
            shared: false,
            system_fonts: !cfg!(target_arch = "wasm32"),
        }),
    };
    font_ctx
        .collection
        .register_fonts(Blob::new(Arc::new(blitz_dom::BULLET_FONT) as _), None);

    if let Some(helvetica_neue) = font_ctx.collection.family_id("Helvetica Neue") {
        let existing: Vec<_> = font_ctx
            .collection
            .generic_families(GenericFamily::SansSerif)
            .collect();
        font_ctx.collection.set_generic_families(
            GenericFamily::SansSerif,
            std::iter::once(helvetica_neue).chain(existing),
        );
    }

    font_ctx
}

#[cfg(test)]
fn prepare_email_html_with_provider(
    html: &str,
    net_provider: Option<Arc<dyn NetProvider>>,
) -> Result<PreparedEmail, String> {
    prepare_email_html_at(html, net_provider, INITIAL_WIDTH, INITIAL_HEIGHT, 1.0)
}
fn prepare_email_html_at(
    html: &str,
    net_provider: Option<Arc<dyn NetProvider>>,
    width: u32,
    height: u32,
    scale: f32,
) -> Result<PreparedEmail, String> {
    let (html, notice) = crate::email_document::bounded_html(html);
    let html = with_email_font_fallback(&html);
    struct PreparationAbort(Option<blitz_traits::net::AbortController>);
    impl Drop for PreparationAbort {
        fn drop(&mut self) {
            if let Some(abort) = self.0.take() {
                abort.abort();
            }
        }
    }
    let mut abort = PreparationAbort(Some(blitz_traits::net::AbortController::default()));
    let mut document = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(width, height, scale, ColorScheme::Light)),
            net_provider,
            abort_signal: Some(abort.0.as_ref().unwrap().signal.clone()),
            font_ctx: Some(build_email_font_ctx()),
            ..Default::default()
        },
    );

    let mut stack = vec![(document.root_node().id, 0usize)];
    let mut count = 0usize;
    while let Some((id, depth)) = stack.pop() {
        count += 1;
        if depth > 96 || count > 30_000 {
            return Err("Message structure exceeds rendering limits. Use plain text.".into());
        }
        if let Some(node) = document.get_node(id) {
            stack.extend(node.children.iter().map(|id| (*id, depth + 1)));
        }
    }
    document.resolve(0.0);
    let links = collect_email_links(&document, width as f32 / scale);
    let plain_text = collect_plain_text(&document);

    Ok(PreparedEmail {
        document,
        links,
        plain_text,
        notice,
        abort: abort.0.take(),
        resolved_size: (
            width,
            height,
            scale.to_bits(),
            (width as f32 / scale).to_bits(),
            (height as f32 / scale).to_bits(),
        ),
    })
}

fn collect_plain_text(document: &HtmlDocument) -> String {
    semantics::plain_text(document)
}

/// Detect network-backed image references without treating ordinary links as
/// blocked content. This drives the privacy banner; the provider remains the
/// authoritative enforcement boundary.
pub fn has_remote_images(html: &str) -> bool {
    crate::email_document::has_remote_images(html)
}

fn with_email_font_fallback(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    if let Some(head_end) = lower.find("</head>") {
        let mut output = String::with_capacity(html.len() + EMAIL_FONT_FALLBACK_STYLE.len());
        output.push_str(&html[..head_end]);
        output.push_str(EMAIL_FONT_FALLBACK_STYLE);
        output.push_str(&html[head_end..]);
        output
    } else if let Some(open_end) = lower.find("<body") {
        let mut output = String::with_capacity(html.len() + EMAIL_FONT_FALLBACK_STYLE.len());
        output.push_str(&html[..open_end]);
        output.push_str("<head>");
        output.push_str(EMAIL_FONT_FALLBACK_STYLE);
        output.push_str("</head>");
        output.push_str(&html[open_end..]);
        output
    } else if let Some(open_end) = lower
        .find("<html")
        .and_then(|start| lower[start..].find('>').map(|end| start + end + 1))
    {
        let mut output = String::with_capacity(html.len() + EMAIL_FONT_FALLBACK_STYLE.len());
        output.push_str(&html[..open_end]);
        output.push_str(EMAIL_FONT_FALLBACK_STYLE);
        output.push_str(&html[open_end..]);
        output
    } else {
        format!("<html><head>{EMAIL_FONT_FALLBACK_STYLE}</head><body>{html}</body></html>")
    }
}

fn browser_url(href: &str) -> Option<String> {
    let href = href.trim();
    if href.starts_with('#') {
        return Some(href.into());
    }
    let (scheme, _) = href.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" | "mailto" | "tel" => {
            url::Url::parse(href).ok().map(|url| url.to_string())
        }
        _ => None,
    }
}

fn anchor_url_for_node(document: &HtmlDocument, mut node_id: NodeId) -> Option<String> {
    loop {
        let node = document.get_node(node_id)?;
        if node.data.is_element_with_tag_name(&local_name!("a")) {
            return node.data.attr(local_name!("href")).and_then(browser_url);
        }
        node_id = node.parent?;
    }
}

fn collect_email_links(document: &HtmlDocument, logical_width: f32) -> Vec<EmailLink> {
    let mut boxes: BTreeMap<NodeId, (f32, f32, f32, f32)> = BTreeMap::new();
    let anchor = |mut id| -> Option<NodeId> {
        loop {
            let node = document.get_node(id)?;
            if !visible(node) {
                return None;
            }
            if node.data.is_element_with_tag_name(&local_name!("a"))
                && node
                    .data
                    .attr(local_name!("href"))
                    .and_then(browser_url)
                    .is_some()
            {
                return Some(id);
            }
            id = node.parent?;
        }
    };
    let mut add = |id: NodeId, source: NodeId, x: f32, y: f32, w: f32, h: f32| {
        if w <= 0.5 || h <= 0.5 {
            return;
        }
        let corners = [(x, y), (x + w, y), (x, y + h), (x + w, y + h)]
            .map(|(x, y)| painted_point(document, source, x, y));
        let mut rect = corners.iter().fold(
            (
                f32::INFINITY,
                f32::INFINITY,
                f32::NEG_INFINITY,
                f32::NEG_INFINITY,
            ),
            |(x0, y0, x1, y1), &(x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        );
        for ancestor in document.node_chain(source) {
            let Some(node) = document.get_node(ancestor) else {
                continue;
            };
            if !node.clips_content() {
                continue;
            }
            let size = node.final_layout().size;
            let a = painted_point(document, ancestor, 0.0, 0.0);
            let b = painted_point(document, ancestor, size.width, size.height);
            rect = (
                rect.0.max(a.0.min(b.0)),
                rect.1.max(a.1.min(b.1)),
                rect.2.min(a.0.max(b.0)),
                rect.3.min(a.1.max(b.1)),
            );
        }
        if rect.2 <= rect.0
            || rect.3 <= rect.1
            || ![rect.0, rect.1, rect.2, rect.3]
                .iter()
                .all(|n| n.is_finite())
        {
            return;
        }
        boxes
            .entry(id)
            .and_modify(|r| {
                r.0 = r.0.min(rect.0);
                r.1 = r.1.min(rect.1);
                r.2 = r.2.max(rect.2);
                r.3 = r.3.max(rect.3);
            })
            .or_insert(rect);
    };
    document.visit(|id, node| {
        if !visible(node)
            || document
                .node_chain(id)
                .iter()
                .any(|id| document.get_node(*id).is_some_and(|n| !visible(n)))
        {
            return;
        }
        let Some(element) = node.element_data() else {
            return;
        };
        if let Some(anchor_id) = anchor(id) {
            let size = node.final_layout().size;
            add(anchor_id, id, 0.0, 0.0, size.width, size.height);
        }
        if let Some(inline) = &element.inline_layout_data {
            let layout = node.final_layout();
            for line in inline.layout.lines() {
                for item in line.items() {
                    if let PositionedLayoutItem::GlyphRun(run) = item
                        && let Some(id) = anchor(run.style().brush.id)
                    {
                        add(
                            id,
                            node.id,
                            layout.padding.left
                                + layout.border.left
                                + run.offset() / inline.layout.scale(),
                            layout.padding.top
                                + layout.border.top
                                + line.metrics().block_min_coord / inline.layout.scale(),
                            run.advance() / inline.layout.scale(),
                            (line.metrics().block_max_coord - line.metrics().block_min_coord)
                                / inline.layout.scale(),
                        );
                    }
                }
            }
        }
    });
    boxes
        .into_iter()
        .filter_map(|(id, (x0, y0, x1, y1))| {
            let node = document.get_node(id)?;
            let url = node.data.attr(local_name!("href")).and_then(browser_url)?;
            let name = node
                .data
                .attr(local_name!("aria-label"))
                .map(str::to_owned)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| subtree_text(document, id));
            Some(EmailLink {
                x: x0 / logical_width,
                y: y0 / logical_width,
                width: (x1 - x0) / logical_width,
                height: (y1 - y0) / logical_width,
                name: if name.is_empty() { url.clone() } else { name },
                url,
            })
        })
        .collect()
}
fn painted_point(document: &HtmlDocument, mut id: NodeId, mut x: f32, mut y: f32) -> (f32, f32) {
    while let Some(node) = document.get_node(id) {
        if let Some(t) = node.transform() {
            let [a, b, c, d, e, f] = t.as_coeffs();
            let scale = document.viewport().scale() as f64;
            let px = x as f64;
            let py = y as f64;
            x = (a * px + c * py + e / scale) as f32;
            y = (b * px + d * py + f / scale) as f32;
        }
        if node.stylo_element_data_opt().is_some() {
            x += node.final_layout().location.x - node.scroll_offset().x as f32;
            y += node.final_layout().location.y - node.scroll_offset().y as f32;
        }
        let Some(parent) = node.layout_parent.get() else {
            break;
        };
        id = parent;
    }
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::{
        GpuEmailRenderer, InputModifiers, VelloCpuImageRenderer, composite_over_white, paint_scene,
        prepare_email_html, render_prepared_cpu, render_to_buffer,
    };
    use crate::mail::fixtures;

    #[test]
    fn email_font_ctx_prefers_helvetica_neue_for_sans_serif_when_installed() {
        use parley::fontique::GenericFamily;

        let mut font_ctx = super::build_email_font_ctx();
        let Some(helvetica_neue) = font_ctx.collection.family_id("Helvetica Neue") else {
            // Host has no Helvetica Neue installed (e.g. a minimal CI image); nothing to assert.
            return;
        };
        let first = font_ctx
            .collection
            .generic_families(GenericFamily::SansSerif)
            .next();
        assert_eq!(first, Some(helvetica_neue));
    }

    #[test]
    fn fixture_messages_prepare_as_retained_blitz_documents() {
        for email in fixtures() {
            let prepared = prepare_email_html(email.html).expect("fixture should parse");
            assert!(!prepared.plain_text.is_empty());
        }

        let linked = prepare_email_html(fixtures()[1].html).expect("linked fixture should parse");
        assert_eq!(linked.links.len(), 2);
    }

    #[test]
    fn html_links_expose_clickable_blitz_bounds() {
        let rendered = prepare_email_html(
            r#"<!doctype html><html><body><p><a href="https://example.com">Open example</a></p></body></html>"#,
        )
        .expect("link should parse");

        assert_eq!(rendered.links.len(), 1);
        assert_eq!(rendered.links[0].url, "https://example.com/");
        assert!(rendered.links[0].width > 0.0);
        assert!(rendered.links[0].height > 0.0);
        assert!(rendered.links[0].x >= 0.0 && rendered.links[0].x < 1.0);
    }

    #[test]
    fn copied_text_excludes_styles_and_scripts() {
        let rendered = prepare_email_html(
            r#"<html><head><style>.hidden { color: red }</style></head><body><p>Hello <b>world</b></p><script>alert('no')</script></body></html>"#,
        )
        .expect("email should parse");

        assert_eq!(rendered.plain_text, "Hello world");
    }

    #[test]
    fn display_none_content_does_not_generate_email_layout_or_pixels() {
        let html = flectar_mail_core::mime::sanitize_html(
            r#"<html><head><title>Metadata title</title><style>.content { height:40px;background:#ef3340 }</style></head><body style="margin:0">
                <span id="preheader" style="display:none;font-size:1px;line-height:1px;max-height:0;max-width:0;opacity:0;overflow:hidden">Hidden preview text</span>
                <div id="content" class="content">Visible content</div>
            </body></html>"#,
        );
        let mut prepared = prepare_email_html(&html).expect("email should parse");
        prepared.document.resolve(0.0);

        let preheader_id = prepared
            .document
            .query_selector("span")
            .expect("selector should parse")
            .expect("preheader should exist in the DOM");
        let preheader = prepared
            .document
            .get_node(preheader_id)
            .expect("preheader node should exist");
        assert_eq!(preheader.final_layout().size.height, 0.0);

        let content_id = prepared
            .document
            .query_selector("div")
            .expect("selector should parse")
            .expect("visible content should exist");
        let content = prepared
            .document
            .get_node(content_id)
            .expect("content node should exist");
        assert_eq!(content.absolute_position(0.0, 0.0).y, 0.0);
        assert!(!prepared.plain_text.contains("Metadata title"));
        assert!(prepared.plain_text.contains("Visible content"));
    }

    #[test]
    fn remote_image_detection_ignores_links_and_finds_image_sources() {
        assert!(!super::has_remote_images(
            r#"<p><a href="https://example.com">Open</a></p>"#
        ));
        assert!(super::has_remote_images(
            r#"<img alt="logo" src="https://cdn.example.com/logo.png">"#
        ));
        assert!(super::has_remote_images(
            r#"<div style="background-image: url( 'https://cdn.example.com/hero.jpg' )"></div>"#
        ));
    }

    #[test]
    fn software_canvas_is_composited_over_opaque_white() {
        let mut rgba = vec![0, 0, 0, 0, 200, 100, 50, 128];
        composite_over_white(&mut rgba);

        assert_eq!(&rgba[..4], &[255, 255, 255, 255]);
        assert_eq!(rgba[7], 255);
        assert!(rgba[4] > 200 && rgba[5] > 100 && rgba[6] > 50);
    }

    #[test]
    fn software_surface_crops_blank_space_and_tiles_long_mail() {
        let mut short = prepare_email_html(
            r#"<html><body style="margin:0"><p style="margin:0;font-size:16px">Short message</p></body></html>"#,
        )
        .expect("short email should parse");
        let short_frame =
            render_prepared_cpu(&mut short, 520, 900, 1.0).expect("short email should render");
        assert_eq!(short_frame.width, 520);
        assert!(
            (64..300).contains(&short_frame.height),
            "short mail should not retain the full canvas: {}px",
            short_frame.height
        );

        let mut long = prepare_email_html(
            r#"<html><body style="margin:0"><div style="height:1800px">Long message</div></body></html>"#,
        )
        .expect("long email should parse");
        let long_frame =
            render_prepared_cpu(&mut long, 520, 900, 1.0).expect("long email should render");
        assert_eq!(long_frame.height, 1812);
        assert_eq!(long_frame.tiles.len(), 4);
    }

    #[test]
    fn software_renderer_keeps_only_viewport_adjacent_tiles() {
        let prepared = prepare_email_html(
            r#"<html><body style="margin:0"><div style="height:5000px">Long message</div></body></html>"#,
        )
        .expect("long email should parse");
        let mut renderer = GpuEmailRenderer::default();
        renderer.set_email(prepared);
        renderer.set_visible_region(0.0, 400.0);
        renderer
            .render_cpu_if_needed(520, 400, 1.0)
            .expect("initial tiles should render")
            .expect("initial frame should exist");
        assert!(renderer.tiles.len() <= 3);

        renderer.set_visible_region(1_500.0, 400.0);
        renderer
            .render_cpu_if_needed(520, 400, 1.0)
            .expect("scrolled tiles should render")
            .expect("scrolled frame should exist");
        assert!(renderer.tiles.len() <= 4);
        assert!(!renderer.tiles.contains_key(&0));
    }

    #[test]
    fn passive_pointer_motion_does_not_dirty_email_tiles() {
        let prepared = prepare_email_html(r#"<html><body><p>Pointer test</p></body></html>"#)
            .expect("email should parse");
        let mut renderer = GpuEmailRenderer::default();
        renderer.set_email(prepared);
        renderer
            .render_cpu_if_needed(520, 400, 1.0)
            .expect("initial tile should render");

        assert!(!renderer.handle_pointer_event(20.0, 20.0, "move", InputModifiers::default()));
        assert!(!renderer.dirty);
    }

    #[test]
    fn inline_email_cta_background_is_painted() {
        let mut prepared = prepare_email_html(
            r#"<html><body style="margin:0"><p style="margin:0"><a href="https://example.com" style="background-color:#4b5fff;color:white;font-size:20px">Open mail</a></p></body></html>"#,
        )
        .expect("CTA should parse");
        prepared.document.resolve(0.0);
        let rgba = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, &mut prepared.document, 1.0, 240, 60, 0, 0),
            240,
            60,
        );

        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[2] > 180 && pixel[0] < 140 && pixel[3] > 200),
            "CTA background color should be visible behind its white label"
        );
    }

    #[test]
    fn instagram_table_cta_background_and_label_are_painted() {
        let mut prepared = prepare_email_html(
            r##"<html><body style="margin:0;padding:0" bgcolor="#ffffff">
            <table border="0" width="100%" cellspacing="0" cellpadding="0"><tr>
              <td style="min-width:394px"><a href="https://instagram.com" style="color:#1b74e4;text-decoration:none">
                <table border="0" width="100%" cellspacing="0" cellpadding="0" style="border-collapse:initial"><tr>
                  <td style="border-radius:12px;text-align:center;display:block;padding:10px 16px 14px 16px;margin:0 2px 0 auto;min-width:370px;background-color:#4A5DF9">
                    <a href="https://instagram.com" style="text-decoration:none;display:block"><center><font size="3"><span style="white-space:nowrap;font-weight:600;color:#fff;font-size:16px;line-height:16px">Abrir Instagram</span></font></center></a>
                  </td>
                </tr></table>
              </a></td>
            </tr></table></body></html>"##,
        )
        .expect("Instagram CTA should parse");
        prepared.document.resolve(0.0);
        let rgba = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, &mut prepared.document, 1.0, 520, 100, 0, 0),
            520,
            100,
        );

        let blue_pixels = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[2] > 180 && pixel[0] < 140 && pixel[3] > 200)
            .count();
        assert!(blue_pixels > 5_000, "full table CTA should paint blue");
        assert!(
            prepared.plain_text.contains("Abrir Instagram"),
            "CTA label should remain in the document"
        );
    }

    #[test]
    fn collapsed_email_table_keeps_a_cell_top_border() {
        let mut prepared = prepare_email_html(
            r#"<html><body style="margin:0;background:#f6f6f6">
              <table width="240" border="0" cellspacing="0" cellpadding="0"
                     style="border-collapse:collapse">
                <tr><td style="height:50px;border-top:10px solid #f38020;background:#fff">&nbsp;</td></tr>
              </table>
            </body></html>"#,
        )
        .expect("collapsed table should parse");
        prepared.document.resolve(0.0);
        let rgba = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, &mut prepared.document, 1.0, 260, 80, 0, 0),
            260,
            80,
        );

        let orange_pixels = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] > 220 && (80..170).contains(&pixel[1]) && pixel[2] < 80)
            .count();
        assert!(
            orange_pixels > 1_500,
            "a collapsed cell border spanning 240px should remain visible, got {orange_pixels} orange pixels"
        );
    }

    #[test]
    fn borderless_collapsed_table_has_no_latent_border_gaps() {
        let mut prepared = prepare_email_html(
            r#"<html><body style="margin:0">
              <table id="layout-table" width="200" border="0" cellspacing="0" cellpadding="0"
                     style="border-collapse:collapse">
                <tr><td id="first-row" style="height:20px"></td><td style="height:20px"></td></tr>
                <tr><td id="second-row" style="height:20px"></td><td style="height:20px"></td></tr>
              </table>
            </body></html>"#,
        )
        .expect("borderless table should parse");
        prepared.document.resolve(0.0);

        let table = prepared
            .document
            .get_node(
                prepared
                    .document
                    .get_element_by_id("layout-table")
                    .expect("table should exist"),
            )
            .expect("table node should exist");
        let first = prepared
            .document
            .get_node(
                prepared
                    .document
                    .get_element_by_id("first-row")
                    .expect("first cell should exist"),
            )
            .expect("first cell node should exist");
        let second = prepared
            .document
            .get_node(
                prepared
                    .document
                    .get_element_by_id("second-row")
                    .expect("second cell should exist"),
            )
            .expect("second cell node should exist");

        assert_eq!(
            table.final_layout().size.height,
            first.final_layout().size.height + second.final_layout().size.height,
            "the table must contain only its rows, without synthetic border gaps"
        );
        assert_eq!(
            second.absolute_position(0.0, 0.0).y - first.absolute_position(0.0, 0.0).y,
            first.final_layout().size.height,
            "CSS border widths with border-style:none must not become row spacing"
        );
    }

    #[test]
    fn css_table_with_direct_cells_gets_an_anonymous_row() {
        let html = flectar_mail_core::mime::sanitize_html(
            r#"<html><head><style>
              .inner-grid { display:table; width:320px; table-layout:fixed; }
              .column { float:left; display:table-cell; width:160px; vertical-align:top; }
            </style></head><body style="margin:0;background:#fff">
              <div class="inner-grid">
                <div id="css-table-cell-one" class="column" style="background:#fff">
                  <strong style="font-size:18px;color:#000">Visible content</strong>
                </div>
                <div id="css-table-cell-two" class="column" style="background:#fff">
                  <strong style="font-size:18px;color:#000">More content</strong>
                </div>
              </div>
            </body></html>"#,
        );
        let mut prepared = prepare_email_html(&html).expect("CSS table should parse");
        prepared.document.resolve(0.0);

        let cell = prepared
            .document
            .get_node(
                prepared
                    .document
                    .get_element_by_id("css-table-cell-one")
                    .expect("CSS table cell should exist"),
            )
            .expect("CSS table cell node should exist");
        let second_cell = prepared
            .document
            .get_node(
                prepared
                    .document
                    .get_element_by_id("css-table-cell-two")
                    .expect("second CSS table cell should exist"),
            )
            .expect("second CSS table cell node should exist");
        assert!(
            cell.final_layout().size.height > 10.0 && second_cell.final_layout().size.height > 10.0,
            "a direct table-cell child needs a browser-generated anonymous row"
        );
        let first_origin = cell.absolute_position(0.0, 0.0);
        let second_origin = second_cell.absolute_position(0.0, 0.0);
        assert!(
            second_origin.x > first_origin.x && second_origin.y == first_origin.y,
            "consecutive anonymous cells should share the generated row"
        );

        let rgba = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, &mut prepared.document, 1.0, 360, 60, 0, 0),
            360,
            60,
        );
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| { pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 80 && pixel[3] > 200 }),
            "content inside a CSS-generated table must be painted"
        );
    }

    #[test]
    fn inline_table_button_paints_its_text() {
        let html = flectar_mail_core::mime::sanitize_html(
            r##"<html><head><style>
              a { color:#ff6633 !important; }
              .button a { color:#000 !important; }
            </style></head><body style="margin:0;background:#fff">
              <table width="100%" border="0" cellspacing="0" cellpadding="0" style="border-collapse:collapse">
                <tr style="white-space:nowrap;background:#fff"><td style="white-space:normal;background:#fff;padding:0 40px">
                  <table class="button" border="0" cellspacing="0" cellpadding="0" bgcolor="#ff6633"
                         style="border-collapse:collapse;border-radius:8px;display:inline-block">
                    <tr><td style="font-family:Arial,sans-serif;font-size:18px;padding:7px 27px;text-align:center">
                      <div><a href="https://example.com" style="color:#000;text-decoration:none">Log in</a></div>
                    </td></tr>
                  </table>
                </td></tr>
              </table>
            </body></html>"##,
        );
        let mut prepared = prepare_email_html(&html).expect("inline table button should parse");
        prepared.document.resolve(0.0);
        let rgba = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, &mut prepared.document, 1.0, 180, 70, 0, 0),
            180,
            70,
        );

        let dark_pixels = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] < 70 && pixel[1] < 70 && pixel[2] < 70 && pixel[3] > 200)
            .count();
        assert!(
            dark_pixels > 20,
            "the inline table CTA label should be painted, got {dark_pixels} dark pixels"
        );
    }

    #[test]
    fn retained_document_selection_is_copiable() {
        let prepared =
            prepare_email_html(r#"<html><body><p>Hello selectable world</p></body></html>"#)
                .expect("email should parse");
        let mut renderer = GpuEmailRenderer::default();
        renderer.set_email(prepared);

        assert!(renderer.select_all());
        assert!(renderer.has_selection());
        assert_eq!(
            renderer.selected_text().as_deref(),
            Some("Hello selectable world")
        );
        assert_eq!(
            renderer.handle_key_event(
                "c",
                true,
                false,
                InputModifiers::new(true, false, false, false)
            ),
            Some("Hello selectable world".to_owned())
        );
    }

    #[test]
    fn pointer_drag_selects_rendered_email_text() {
        let prepared = prepare_email_html(
            r#"<html><body style="margin:0"><p style="margin:0;font-size:20px">Drag across this selectable line</p></body></html>"#,
        )
        .expect("email should parse");
        let mut renderer = GpuEmailRenderer::default();
        renderer.set_email(prepared);

        renderer.handle_pointer_event(2.0, 10.0, "down", InputModifiers::default());
        renderer.handle_pointer_event(210.0, 10.0, "move", InputModifiers::default());
        renderer.handle_pointer_event(210.0, 10.0, "up", InputModifiers::default());

        assert!(renderer.has_selection());
        assert!(
            renderer
                .selected_text()
                .is_some_and(|text| !text.is_empty())
        );
    }

    #[test]
    fn double_click_selects_a_word() {
        let prepared = prepare_email_html(
            r#"<html><body style="margin:0"><p style="margin:0;font-size:20px">Double click selects this word</p></body></html>"#,
        )
        .expect("email should parse");
        let mut renderer = GpuEmailRenderer::default();
        renderer.set_email(prepared);

        for _ in 0..2 {
            renderer.handle_pointer_event(85.0, 10.0, "down", InputModifiers::default());
            renderer.handle_pointer_event(85.0, 10.0, "up", InputModifiers::default());
        }

        let selected = renderer.selected_text().expect("word should be selected");
        assert!(!selected.is_empty());
        assert!(!selected.chars().any(char::is_whitespace));
    }

    #[test]
    fn blitz_resource_provider_loads_embedded_images() {
        use std::time::Duration;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        let mut renderer = GpuEmailRenderer::default();
        renderer
            .configure_resources(runtime.handle().clone(), false)
            .expect("resource provider should build");
        let prepared = renderer
            .prepare_email_html(
                r#"<html><body><img alt="pixel" src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="></body></html>"#,
                false,
            )
            .expect("email should parse");
        renderer.set_email(prepared);

        let mut loaded = false;
        for _ in 0..20 {
            runtime.block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
            renderer.poll_resources();
            let email = renderer.email.as_ref().expect("email retained");
            let image_id = email
                .document
                .query_selector("img")
                .expect("selector valid")
                .expect("image exists");
            loaded = email
                .document
                .get_node(image_id)
                .and_then(|node| node.element_data())
                .and_then(|element| element.raster_image_data())
                .is_some();
            if loaded {
                break;
            }
        }

        assert!(loaded, "data URI should decode into a raster image");
    }

    #[test]
    #[cfg(feature = "remote-content")]
    fn blitz_resource_provider_rejects_loopback_images() {
        use std::{
            io::{Cursor, Read, Write},
            net::TcpListener,
            thread,
            time::{Duration, Instant},
        };

        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("test PNG should encode");
        let png = png.into_inner();

        let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
        listener
            .set_nonblocking(true)
            .expect("test listener should be nonblocking");
        let address = listener
            .local_addr()
            .expect("test server should have an address");
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(200);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .expect("test stream timeout should apply");
                        let mut request = [0_u8; 1024];
                        let _ = stream.read(&mut request);
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            png.len()
                        )
                        .expect("test response header should write");
                        stream
                            .write_all(&png)
                            .expect("test response body should write");
                        return true;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("test server failed: {error}"),
                }
            }
            false
        });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime should build");
        let mut renderer = GpuEmailRenderer::default();
        renderer
            .configure_resources(runtime.handle().clone(), true)
            .expect("resource provider should build");
        let prepared = renderer
            .prepare_email_html(
                &format!(
                    r#"<html><body><img alt="remote pixel" src="http://{address}/pixel.png"></body></html>"#
                ),
                false,
            )
            .expect("email should parse");
        renderer.set_email(prepared);

        let mut loaded = false;
        for _ in 0..20 {
            runtime.block_on(async { tokio::time::sleep(Duration::from_millis(10)).await });
            renderer.poll_resources();
            let email = renderer.email.as_ref().expect("email retained");
            let image_id = email
                .document
                .query_selector("img")
                .expect("selector valid")
                .expect("image exists");
            loaded = email
                .document
                .get_node(image_id)
                .and_then(|node| node.element_data())
                .and_then(|element| element.raster_image_data())
                .is_some();
            if loaded {
                break;
            }
        }

        assert!(!server.join().expect("test server should finish"));
        assert!(
            !loaded,
            "Loopback images must remain blocked in production and tests"
        );
    }

    #[test]
    fn borderless_collapsed_email_table_does_not_paint_a_black_grid() {
        let mut prepared = prepare_email_html(
            r#"<html><body style="margin:0"><table style="border-collapse:collapse;width:200px;height:100px"><tr><td></td><td></td></tr><tr><td></td><td></td></tr></table></body></html>"#,
        )
        .expect("email should parse");
        prepared.document.resolve(0.0);
        let rgba = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| {
                paint_scene(scene, &mut prepared.document, 1.0, 220, 120, 0, 0);
            },
            220,
            120,
        );

        let dark_opaque_pixels = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[3] > 0 && pixel[0] < 32 && pixel[1] < 32 && pixel[2] < 32)
            .count();
        assert_eq!(dark_opaque_pixels, 0);
    }
}
