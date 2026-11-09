//! Reading colors are a reversible presentation layer over the sender's DOM.
//!
//! Resolve the sender's CSS (including prefers-color-scheme) first. Adapt solid
//! surfaces and gradient stops, then choose text against its actual ancestral
//! background. Raster images, layout, links and the stored/exported HTML are
//! untouched. UA !important declarations also cover legacy bgcolor attributes
//! and inline !important styles without rewriting the message.
use blitz_dom::{
    QualName, UserAgentStylesheet, ns,
    util::{Color, ToColorColor},
};
use blitz_html::HtmlDocument;
use blitz_traits::shell::ColorScheme;
use std::fmt::Write;
use style::{
    color::{AbsoluteColor, ColorSpace},
    values::{
        computed::Image,
        generics::{
            color::GenericColor,
            image::{GenericGradient, GenericGradientItem, GenericImage},
        },
    },
};
use style_traits::ToCss;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct EmailAppearance {
    pub original: bool,
    pub dark: bool,
    pub background: Color,
    pub foreground: Color,
    pub link: Color,
}

impl Default for EmailAppearance {
    fn default() -> Self {
        Self {
            original: true,
            dark: false,
            background: Color::WHITE,
            foreground: Color::BLACK,
            link: Color::from_rgb8(9, 105, 218),
        }
    }
}

impl EmailAppearance {
    pub fn from_app(app: &crate::AppWindow) -> Self {
        use slint::ComponentHandle;
        let colors = app.global::<crate::MessageColors>();
        let convert = |value: slint::Color| {
            Color::from_rgba8(value.red(), value.green(), value.blue(), value.alpha())
        };
        Self {
            original: colors.get_original(),
            dark: colors.get_dark(),
            background: convert(colors.get_background()),
            foreground: convert(colors.get_foreground()),
            link: convert(colors.get_link()),
        }
    }

    pub fn scheme(self) -> ColorScheme {
        if self.dark && !self.original {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        }
    }

    pub fn canvas(self) -> Color {
        if self.original {
            Color::WHITE
        } else {
            self.background.with_alpha(1.0)
        }
    }

    fn background(self, color: Color) -> Color {
        if color.components[3] == 0.0 {
            return color;
        }
        let l = luminance(color);
        let target = luminance(self.background);
        if l > 0.98 {
            self.background.with_alpha(color.components[3])
        } else if self.dark && l > 0.18 {
            // White is the theme's exact surface. Tinted and inset surfaces
            // retain their hue and a little separation from that foundation.
            let level = target + (1.0 - l) * 0.055;
            blend_to_luminance(color, self.background, level)
        } else if !self.dark && l < 0.18 && neutral(color) {
            let level = (target - l * 0.45).max(0.6);
            blend_to_luminance(color, self.background, level)
        } else {
            color
        }
    }

    fn foreground(self, color: Color, background: Backdrop) -> Color {
        if color.components[3] == 0.0 || background.image {
            return color;
        }
        let mut candidate = color;
        // Keep authored white labels on colored buttons and native dark text
        // over images. Neutral prose on the reading surface uses the palette.
        if neutral(color)
            && ((neutral(background.low) && neutral(background.high))
                || (same_color(background.low, self.canvas())
                    && same_color(background.high, self.canvas())))
        {
            let l = luminance(color);
            if (self.dark && l < 0.45) || (!self.dark && l < 0.18) {
                candidate = self.foreground.with_alpha(color.components[3]);
                if self.dark && l > 0.08 {
                    candidate = mix(candidate, background.low, 0.2);
                }
            }
        } else if same_color(color, Color::from_rgb8(9, 105, 218)) {
            candidate = self.link.with_alpha(color.components[3]);
        }
        // Leave a margin above WCAG AA's 4.5:1 threshold for CSS serialization
        // and the renderer's conversion to eight-bit color channels.
        ensure_contrast(candidate, background, 4.55)
    }
}

#[derive(Clone, Copy)]
struct Backdrop {
    low: Color,
    high: Color,
    image: bool,
}

impl Backdrop {
    fn solid(color: Color) -> Self {
        Self {
            low: color,
            high: color,
            image: false,
        }
    }

    fn over(self, color: Color) -> Self {
        Self {
            low: composite(color, self.low),
            high: composite(color, self.high),
            image: self.image && color.components[3] < 1.0,
        }
    }

    fn contrast(self, color: Color) -> f32 {
        let low = luminance(self.low);
        let high = luminance(self.high);
        let value = luminance(composite(color, self.low));
        if value >= low.min(high) && value <= low.max(high) {
            return 1.0;
        }
        contrast(composite(color, self.low), self.low)
            .min(contrast(composite(color, self.high), self.high))
    }
}

#[derive(Default)]
pub(crate) struct AppearanceDocument {
    pub colors: EmailAppearance,
    base: String,
    base_sheet: Option<UserAgentStylesheet>,
    overrides: Option<UserAgentStylesheet>,
    indexed: bool,
}

impl AppearanceDocument {
    pub fn new(colors: EmailAppearance) -> Self {
        Self {
            colors,
            ..Self::default()
        }
    }

    pub fn resolve(&mut self, document: &mut HtmlDocument) {
        if let Some(overrides) = self.overrides.take() {
            document.remove_user_agent_stylesheet(&overrides);
        }
        let base = if self.colors.original {
            String::new()
        } else {
            format!(
                "html {{ background-color: {}; }}",
                css_color(self.colors.canvas())
            )
        };
        if base != self.base {
            if let Some(sheet) = self.base_sheet.take() {
                document.remove_user_agent_stylesheet(&sheet);
            }
            if !base.is_empty() {
                let sheet = UserAgentStylesheet::parse(&base);
                document.add_user_agent_stylesheet(sheet.clone());
                self.base_sheet = Some(sheet);
            }
            self.base = base;
        }
        if !self.colors.original && !self.indexed {
            let mut elements = Vec::new();
            let mut stack = vec![document.root_node().id];
            while let Some(id) = stack.pop() {
                let Some(node) = document.get_node(id) else {
                    continue;
                };
                stack.extend(node.children.iter().copied());
                if node.element_data().is_some() {
                    elements.push(id);
                }
            }
            let mut mutation = document.mutate();
            for id in elements {
                mutation.set_attribute(
                    id,
                    QualName {
                        prefix: None,
                        ns: ns!(),
                        // Stylo buckets attribute selectors by name. A unique
                        // name keeps matching linear in the message size;
                        // one shared attribute with unique values is quadratic.
                        local: format!("data-flectar-appearance-{}", id.as_u64()).into(),
                    },
                    "",
                );
            }
            self.indexed = true;
        }
        document.resolve(0.0);
        if self.colors.original {
            return;
        }
        let overrides = self.stylesheet(document);
        if !overrides.is_empty() {
            let sheet = UserAgentStylesheet::parse(&overrides);
            document.add_user_agent_stylesheet(sheet.clone());
            self.overrides = Some(sheet);
            document.resolve(0.0);
        }
    }

    fn stylesheet(&self, document: &HtmlDocument) -> String {
        let mut css = String::new();
        let canvas = Backdrop::solid(self.colors.canvas());
        let mut stack = vec![(document.root_node().id, canvas, canvas)];
        while let Some((id, inherited, authored_inherited)) = stack.pop() {
            let Some(node) = document.get_node(id) else {
                continue;
            };
            let mut backdrop = inherited;
            let mut authored_backdrop = authored_inherited;
            if let Some(style) = node.element_data().and_then(|_| node.primary_styles()) {
                let foreground = style.clone_color();
                let background = style.get_background();
                let authored_bg = background
                    .background_color
                    .resolve_to_absolute(&foreground)
                    .as_color_color();
                // A photograph's brightness cannot be inferred from CSS. Keep
                // its surface and authored text together, including children.
                let url_image = background.background_image.0.iter().any(has_image);
                let mapped_bg = if url_image {
                    authored_bg
                } else {
                    self.colors.background(authored_bg)
                };
                backdrop = inherited.over(mapped_bg);
                authored_backdrop = authored_inherited.over(authored_bg);
                let (images, stops) = if url_image {
                    (None, Vec::new())
                } else {
                    self.images(&background.background_image.0, &foreground)
                };
                if url_image {
                    backdrop.image = true;
                }
                authored_backdrop.image |= url_image || !stops.is_empty();
                if !stops.is_empty() && !url_image {
                    let mut composed: Vec<_> = stops
                        .into_iter()
                        .map(|c| composite(c, backdrop.low))
                        .collect();
                    composed.sort_by(|a, b| luminance(*a).total_cmp(&luminance(*b)));
                    backdrop.low = composed[0];
                    backdrop.high = *composed.last().unwrap();
                }
                let authored_fg = foreground.as_color_color();
                let mapped_fg = if !authored_backdrop.image
                    && same_color(authored_fg.with_alpha(1.0), authored_backdrop.low)
                    && same_color(authored_fg.with_alpha(1.0), authored_backdrop.high)
                {
                    // Some preheaders use identical foreground and background
                    // colors to conceal their preview padding.
                    backdrop.low.with_alpha(authored_fg.components[3])
                } else {
                    self.colors.foreground(authored_fg, backdrop)
                };
                let _ = write!(
                    css,
                    "[data-flectar-appearance-{}] {{ color: {} !important;",
                    id.as_u64(),
                    css_color(mapped_fg)
                );
                if mapped_bg != authored_bg || !background.background_color.is_absolute() {
                    let _ = write!(
                        css,
                        "background-color: {} !important;",
                        css_color(mapped_bg)
                    );
                }
                if let Some(images) = images {
                    let _ = write!(css, "background-image: {images} !important;");
                }
                let border = style.get_border();
                for (name, color, width) in [
                    (
                        "border-top-color",
                        &border.border_top_color,
                        &border.border_top_width,
                    ),
                    (
                        "border-right-color",
                        &border.border_right_color,
                        &border.border_right_width,
                    ),
                    (
                        "border-bottom-color",
                        &border.border_bottom_color,
                        &border.border_bottom_width,
                    ),
                    (
                        "border-left-color",
                        &border.border_left_color,
                        &border.border_left_width,
                    ),
                ] {
                    if width.0.to_f32_px() <= 0.0 {
                        continue;
                    }
                    let color = color.resolve_to_absolute(&foreground).as_color_color();
                    if neutral(color) && color.components[3] > 0.0 && !backdrop.image {
                        let mapped = mix(self.colors.foreground, backdrop.low, 0.75)
                            .with_alpha(color.components[3]);
                        let _ = write!(css, "{name}: {} !important;", css_color(mapped));
                    }
                }
                css.push_str("}\n");
            }
            stack.extend(
                node.children
                    .iter()
                    .rev()
                    .map(|id| (*id, backdrop, authored_backdrop)),
            );
        }
        css
    }

    fn images(&self, images: &[Image], current: &AbsoluteColor) -> (Option<String>, Vec<Color>) {
        let mut mapped = images.to_vec();
        let mut changed = false;
        let mut stops = Vec::new();
        for image in &mut mapped {
            let GenericImage::Gradient(gradient) = image else {
                continue;
            };
            match gradient.as_mut() {
                GenericGradient::Linear { items, .. } | GenericGradient::Radial { items, .. } => {
                    for item in items.iter_mut() {
                        self.gradient_item(item, current, &mut changed, &mut stops);
                    }
                }
                GenericGradient::Conic { items, .. } => {
                    for item in items.iter_mut() {
                        self.gradient_item(item, current, &mut changed, &mut stops);
                    }
                }
            }
        }
        let css = changed.then(|| {
            mapped
                .iter()
                .map(ToCss::to_css_string)
                .collect::<Vec<_>>()
                .join(",")
        });
        (css, stops)
    }

    fn gradient_item<T>(
        &self,
        item: &mut GenericGradientItem<GenericColor<style::values::computed::Percentage>, T>,
        current: &AbsoluteColor,
        changed: &mut bool,
        stops: &mut Vec<Color>,
    ) {
        let color = match item {
            GenericGradientItem::SimpleColorStop(color)
            | GenericGradientItem::ComplexColorStop { color, .. } => color,
            GenericGradientItem::InterpolationHint(_) => return,
        };
        let original = color.resolve_to_absolute(current).as_color_color();
        let mapped = self.colors.background(original);
        stops.push(mapped);
        if mapped != original || !color.is_absolute() {
            let [r, g, b, a] = mapped.components;
            *color = GenericColor::Absolute(AbsoluteColor::new(ColorSpace::Srgb, r, g, b, a));
            *changed = true;
        }
    }
}

fn has_image(image: &Image) -> bool {
    !matches!(image, GenericImage::None | GenericImage::Gradient(_))
}

fn neutral(color: Color) -> bool {
    let [r, g, b, _] = color.components;
    r.max(g).max(b) - r.min(g).min(b) < 0.13
}

fn same_color(a: Color, b: Color) -> bool {
    a.components
        .into_iter()
        .zip(b.components)
        .all(|(a, b)| (a - b).abs() < 0.00001)
}

fn luminance(color: Color) -> f32 {
    color.split().0.relative_luminance()
}

fn contrast(a: Color, b: Color) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn mix(a: Color, b: Color, amount: f32) -> Color {
    a.lerp_rect(b.with_alpha(a.components[3]), amount)
}

fn composite(color: Color, background: Color) -> Color {
    let alpha = color.components[3];
    let mut result = mix(background, color, alpha);
    result.components[3] = 1.0;
    result
}

fn blend_to_luminance(color: Color, target: Color, level: f32) -> Color {
    let decreasing = luminance(color) > luminance(target);
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if (luminance(mix(color, target, mid)) > level) == decreasing {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    mix(color, target, hi)
}

fn ensure_contrast(color: Color, backdrop: Backdrop, minimum: f32) -> Color {
    if backdrop.contrast(color) >= minimum {
        return color;
    }
    let black = Color::BLACK.with_alpha(color.components[3]);
    let white = Color::WHITE.with_alpha(color.components[3]);
    let pole = if backdrop.contrast(black) > backdrop.contrast(white) {
        black
    } else {
        white
    };
    if backdrop.contrast(pole) < minimum {
        // Preserve authored transparency; it may be a hidden preheader. Never
        // expose concealed content in an attempt to increase its contrast.
        return pole;
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if backdrop.contrast(mix(color, pole, mid)) < minimum {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    mix(color, pole, hi)
}

fn css_color(color: Color) -> String {
    let [r, g, b, a] = color.components;
    format!(
        "rgba({:.5}%,{:.5}%,{:.5}%,{:.5})",
        r * 100.0,
        g * 100.0,
        b * 100.0,
        a
    )
}
