// https://html.spec.whatwg.org/multipage/canvas.html#text-styles

use parley::{
    FontContext, FontFamily, FontStyle, FontWeight, Language, LayoutContext, PositionedLayoutItem,
    StyleProperty,
};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::types::Tag;
use skrifa::raw::{FontRead as _, TableProvider as _};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use tiny_skia::{Path, PathBuilder, Transform};

use super::state::TextBaseline;

pub(crate) struct TextCluster {
    pub(crate) text: String,
    pub(crate) utf16_start: u32,
    pub(crate) utf16_end: u32,
    pub(crate) offset: f32,
    pub(crate) advance: f32,
    pub(crate) path: Option<Path>,
    pub(crate) ink: Option<ClusterInk>,
}

#[derive(Clone, Copy)]
pub(crate) struct ClusterInk {
    pub(crate) min_x: f32,
    pub(crate) max_x: f32,
    pub(crate) ascent: f32,
    pub(crate) descent: f32,
}

impl ClusterInk {
    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            max_x: self.max_x.max(other.max_x),
            ascent: self.ascent.max(other.ascent),
            descent: self.descent.max(other.descent),
        }
    }
}

pub(crate) struct TextMetricsRecord {
    pub(crate) text: String,
    pub(crate) shaped: ShapedText,
    pub(crate) width: f64,
    pub(crate) align: ResolvedAlign,
    pub(crate) baseline: TextBaseline,
    pub(crate) rtl: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedAlign {
    Left,
    Right,
    Center,
}

impl ResolvedAlign {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Center => "center",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shaping {
    Glyphs,
    Clusters,
    Width,
}

pub(crate) struct ShapedText {
    pub(crate) path: Option<Path>,
    pub(crate) width: f32,
    pub(crate) ascent: f32,
    pub(crate) descent: f32,
    pub(crate) scale_x: f32,
    pub(crate) clusters: Vec<TextCluster>,
    // https://html.spec.whatwg.org/multipage/canvas.html#textmetrics
    pub(crate) ink: Option<ClusterInk>,
    pub(crate) em_ascent: f32,
    pub(crate) em_descent: f32,
    pub(crate) font_ascent: f32,
    pub(crate) font_descent: f32,
    pub(crate) hanging_baseline: f32,
    pub(crate) ideographic_baseline: f32,
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-filltext
#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "shaping consumes the full canvas text state in one pass, and a \
              UTF-16 offset beyond u32::MAX cannot fit in a JavaScript string"
)]
pub(crate) fn shape(
    text: &str,
    font: &str,
    max_width: Option<f32>,
    letter_spacing: f32,
    word_spacing: f32,
    language: Option<&str>,
    kerning: &str,
    font_ctx: &mut FontContext,
    shaping: Shaping,
) -> ShapedText {
    // https://html.spec.whatwg.org/multipage/canvas.html#text-preparation-algorithm
    let text: String = text
        .chars()
        .map(|ch| {
            if matches!(ch, '\n' | '\r' | '\t' | '\x0c') {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let parsed = parse_font(font);
    // https://html.spec.whatwg.org/multipage/canvas.html#text-preparation-algorithm
    let mut prepared = String::with_capacity(text.len());
    let mut byte_map = (shaping == Shaping::Clusters).then(|| Vec::with_capacity(text.len() + 1));
    for (byte, ch) in text.char_indices() {
        if ch == '\0' {
            continue;
        }
        let start = prepared.len();
        prepared.push(match ch {
            '\t' | '\n' | '\u{c}' | '\r' => ' ',
            _ => ch,
        });
        if let Some(map) = byte_map.as_mut() {
            map.extend(std::iter::repeat_n(byte, prepared.len() - start));
        }
    }
    if let Some(map) = byte_map.as_mut() {
        map.push(text.len());
    }
    if prepared.is_empty() {
        return ShapedText {
            path: None,
            width: 0.0,
            ascent: parsed.size,
            descent: parsed.size * 0.25,
            scale_x: 1.0,
            clusters: Vec::new(),
            ink: None,
            em_ascent: parsed.size,
            em_descent: 0.0,
            font_ascent: parsed.size,
            font_descent: 0.0,
            hanging_baseline: parsed.size * 0.8,
            ideographic_baseline: -parsed.size * 0.2,
        };
    }
    let shaped_text = prepared.as_str();
    let mut layout_ctx = LayoutContext::<[u8; 4]>::new();
    let mut builder = layout_ctx.ranged_builder(font_ctx, shaped_text, 1.0, true);
    builder.push_default(StyleProperty::FontSize(parsed.size));
    if let Some(family) = parsed.family {
        builder.push_default(StyleProperty::FontFamily(FontFamily::Source(family.into())));
    }
    if parsed.weight >= 600.0 {
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(parsed.weight)));
    }
    if parsed.italic {
        builder.push_default(StyleProperty::FontStyle(FontStyle::Italic));
    }
    if letter_spacing != 0.0 {
        builder.push_default(StyleProperty::LetterSpacing(letter_spacing));
    }
    if word_spacing != 0.0 {
        builder.push_default(StyleProperty::WordSpacing(word_spacing));
    }
    if let Some(language) = language.and_then(|language| Language::parse(language).ok()) {
        builder.push_default(StyleProperty::Locale(Some(language)));
    }
    let mut features = Vec::new();
    if parsed.small_caps {
        features.push("\"smcp\" 1");
    }
    if kerning == "none" {
        features.push("\"kern\" 0");
    }
    if !features.is_empty() {
        builder.push_default(StyleProperty::FontFeatures(parley::FontFeatures::from(
            features.join(", ").as_str(),
        )));
    }
    let mut layout = builder.build(shaped_text);
    layout.break_all_lines(None);
    let metrics = layout.root_span_metrics();
    let (ascent, descent) = metrics.map_or((parsed.size, parsed.size * 0.25), |metrics| {
        (metrics.ascent, metrics.descent)
    });
    let width = layout.full_width();
    let scale_x = max_width
        .filter(|max| *max > 0.0 && width > *max)
        .map_or(1.0, |max| max / width);
    let utf16_offsets = if shaping == Shaping::Clusters {
        cluster_utf16_offsets(&text)
    } else {
        Vec::new()
    };
    let mut clusters = Vec::new();
    if let Some(byte_map) = byte_map.as_ref() {
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                for cluster in run.run().clusters() {
                    let range = cluster.text_range();
                    let byte_start = byte_map[range.start];
                    let byte_end = byte_map[range.end];
                    debug_assert!(text.is_char_boundary(byte_start));
                    debug_assert!(text.is_char_boundary(byte_end));
                    clusters.push(TextCluster {
                        text: text[byte_start..byte_end].to_owned(),
                        utf16_start: utf16_offsets[byte_start] as u32,
                        utf16_end: utf16_offsets[byte_end] as u32,
                        offset: cluster.visual_offset().unwrap_or(0.0),
                        advance: cluster.advance(),
                        path: None,
                        ink: None,
                    });
                }
            }
        }
    }
    let font_metrics = layout_font_metrics(&layout, parsed.size);
    let path = match shaping {
        Shaping::Glyphs => glyph_path(&layout, None),
        Shaping::Clusters => {
            glyph_path(&layout, Some((&mut clusters, scale_x, ascent)));
            None
        }
        Shaping::Width => None,
    };
    let ink = if shaping == Shaping::Clusters {
        clusters
            .iter()
            .filter_map(|cluster| cluster.ink)
            .reduce(ClusterInk::union)
    } else {
        None
    };
    ShapedText {
        path,
        width,
        ascent,
        descent,
        scale_x,
        clusters,
        ink,
        em_ascent: font_metrics.em_ascent,
        em_descent: font_metrics.em_descent,
        font_ascent: font_metrics.font_ascent,
        font_descent: font_metrics.font_descent,
        hanging_baseline: font_metrics.hanging,
        ideographic_baseline: font_metrics.ideographic,
    }
}

fn cluster_utf16_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0; text.len() + 1];
    let mut utf16 = 0;
    for (byte, ch) in text.char_indices() {
        for slot in &mut offsets[byte..byte + ch.len_utf8()] {
            *slot = utf16;
        }
        utf16 += ch.len_utf16();
    }
    offsets[text.len()] = utf16;
    offsets
}

struct FontMetrics {
    em_ascent: f32,
    em_descent: f32,
    font_ascent: f32,
    font_descent: f32,
    hanging: f32,
    ideographic: f32,
}

fn base_baselines(font: &FontRef<'_>, size: f32) -> Option<(f32, f32)> {
    let data = font.table_data(Tag::new(b"BASE"))?;
    let base = skrifa::raw::tables::base::Base::read(data).ok()?;
    let axis = base.horiz_axis()?.ok()?;
    let tags = axis.base_tag_list()?.ok()?.baseline_tags();
    let script_list = axis.base_script_list().ok()?;
    let records = script_list.base_script_records();
    let script = records
        .first()?
        .base_script(script_list.offset_data())
        .ok()?;
    let values = script.base_values()?.ok()?;
    let units_per_em = font.head().ok()?.units_per_em();
    let mut hanging = None;
    let mut ideographic = None;
    for (tag, coordinate) in tags.iter().zip(values.base_coords().iter()) {
        let coordinate = coordinate.ok()?.coordinate();
        let coordinate = f32::from(coordinate) * size / f32::from(units_per_em);
        let tag = tag.get();
        if tag == Tag::new(b"hang") {
            hanging = Some(coordinate);
        } else if tag == Tag::new(b"ideo") {
            ideographic = Some(coordinate);
        }
    }
    Some((hanging?, ideographic?))
}

// https://html.spec.whatwg.org/multipage/canvas.html#textmetrics
fn layout_font_metrics(layout: &parley::Layout<[u8; 4]>, size: f32) -> FontMetrics {
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(run) = item else {
                continue;
            };
            let run = run.run();
            let font = run.font();
            let Ok(font_ref) = FontRef::from_index(font.data.as_ref(), font.index) else {
                continue;
            };
            let metrics = font_ref.metrics(Size::new(run.font_size()), LocationRef::default());
            // https://html.spec.whatwg.org/multipage/canvas.html#textmetrics
            let raw_ascent = metrics.ascent.max(0.0);
            let raw_descent = (-metrics.descent).max(0.0);
            let em_sum = raw_ascent + raw_descent;
            let em_scale = if em_sum > 0.0 {
                run.font_size() / em_sum
            } else {
                1.0
            };
            // https://learn.microsoft.com/en-us/typography/opentype/spec/base
            let (hanging, ideographic) = base_baselines(&font_ref, run.font_size())
                .unwrap_or((raw_ascent * 0.8, -raw_descent));
            return FontMetrics {
                em_ascent: raw_ascent * em_scale,
                em_descent: raw_descent * em_scale,
                font_ascent: raw_ascent,
                font_descent: raw_descent,
                hanging,
                ideographic,
            };
        }
    }
    FontMetrics {
        em_ascent: size,
        em_descent: 0.0,
        font_ascent: size,
        font_descent: 0.0,
        hanging: size * 0.8,
        ideographic: 0.0,
    }
}

fn cluster_at(order: &[usize], clusters: &[TextCluster], x: f32) -> Option<usize> {
    let position = order.partition_point(|index| clusters[*index].offset <= x);
    order.get(position.saturating_sub(1)).copied()
}

fn glyph_path(
    layout: &parley::Layout<[u8; 4]>,
    mut ink: Option<(&mut [TextCluster], f32, f32)>,
) -> Option<Path> {
    let mut builder = PathBuilder::new();
    let mut placed_any = false;
    let mut cluster_builders: Vec<PathBuilder> = match ink.as_ref() {
        Some((clusters, _, _)) => (0..clusters.len()).map(|_| PathBuilder::new()).collect(),
        None => Vec::new(),
    };
    let root_ascent = layout
        .root_span_metrics()
        .map_or(0.0, |metrics| metrics.ascent);
    let cluster_order: Vec<usize> = match ink.as_ref() {
        Some((clusters, _, _)) => {
            let mut order: Vec<usize> = (0..clusters.len()).collect();
            order.sort_by(|left, right| clusters[*left].offset.total_cmp(&clusters[*right].offset));
            order
        }
        None => Vec::new(),
    };
    for line in layout.lines() {
        let leading = line.metrics().baseline - root_ascent;
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(run) = item else {
                continue;
            };
            let shape_run = run.run();
            let font = shape_run.font();
            let Ok(font_ref) = FontRef::from_index(font.data.as_ref(), font.index) else {
                continue;
            };
            let outlines = font_ref.outline_glyphs();
            for glyph in run.positioned_glyphs() {
                let Some(outline) = outlines.get(GlyphId::new(glyph.id)) else {
                    continue;
                };
                let settings = DrawSettings::unhinted(
                    Size::new(shape_run.font_size()),
                    LocationRef::default(),
                );
                let mut pen = GlyphPen::new();
                if outline.draw(settings, &mut pen).is_err() {
                    continue;
                }
                let Some(path) = pen.finish() else {
                    continue;
                };
                let placement = Transform::from_translate(glyph.x, glyph.y - leading);
                let Some(path) = path.transform(placement) else {
                    continue;
                };
                if let Some((clusters, scale_x, ascent)) = ink.as_mut()
                    && let Some(index) = cluster_at(&cluster_order, clusters, glyph.x)
                {
                    if let Some(bounds) = path
                        .clone()
                        .transform(
                            Transform::from_scale(*scale_x, 1.0)
                                .pre_concat(Transform::from_translate(0.0, -*ascent)),
                        )
                        .as_ref()
                        .map(Path::bounds)
                    {
                        let glyph_ink = ClusterInk {
                            min_x: bounds.left(),
                            max_x: bounds.right(),
                            ascent: (-bounds.top()).max(0.0),
                            descent: bounds.bottom().max(0.0),
                        };
                        let slot = &mut clusters[index].ink;
                        *slot = Some(match *slot {
                            None => glyph_ink,
                            Some(current) => current.union(glyph_ink),
                        });
                    }
                    let origin = clusters[index].offset;
                    if let Some(local) = path
                        .clone()
                        .transform(Transform::from_translate(-origin, 0.0))
                    {
                        cluster_builders[index].push_path(&local);
                    }
                } else {
                    builder.push_path(&path);
                    placed_any = true;
                }
            }
        }
    }
    if let Some((clusters, _, _)) = ink {
        for (cluster, builder) in clusters.iter_mut().zip(cluster_builders) {
            cluster.path = builder.finish();
        }
        return None;
    }
    placed_any.then(|| builder.finish()).flatten()
}

// https://drafts.csswg.org/css-fonts/#font-prop
// https://drafts.csswg.org/css-values-4/#lengths
pub(crate) fn length_px(value: &str, font_size: f32) -> f32 {
    let split = value
        .find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+')))
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number = number.parse::<f32>().unwrap_or(0.0);
    let scale = match unit.trim() {
        "em" | "ic" => font_size,
        "ex" | "ch" => font_size * 0.5,
        "rem" | "pc" => 16.0,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 96.0 / 25.4,
        "pt" => 96.0 / 72.0,

        "q" => 96.0 / 101.6,
        "%" => font_size / 100.0,
        _ => 1.0,
    };
    number * scale
}

// https://html.spec.whatwg.org/multipage/canvas.html#dom-context-2d-font
pub(crate) fn canonical_font(value: &str, base_size: f32, line_height: f32) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "default"
    ) || value.contains(['{', '}', ';', '('])
    {
        return None;
    }
    if matches!(
        value,
        "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar"
    ) {
        return Some("10px sans-serif".to_owned());
    }

    let mut style = None;
    let mut variant = None;
    let mut weight = None;
    let mut stretch = None;
    let mut size = None;
    let mut family = None;
    let tokens: Vec<&str> = value.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        if let Some(px) = length_from_token(token, base_size, line_height) {
            size = Some(px);
            family = Some(tokens[index + 1..].join(" "));
            break;
        }
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "italic" | "oblique" => style = Some("italic"),
            "small-caps" => variant = Some("small-caps"),
            "bold" | "bolder" => weight = Some(700.0),
            "lighter" => weight = Some(300.0),
            "normal" => {}
            keyword if is_stretch(keyword) => stretch = Some(keyword.to_owned()),
            number if number.parse::<f32>().is_ok() => {
                let value = number.parse::<f32>().ok()?;
                weight = Some(value);
            }
            _ => return None,
        }
    }
    let size = size?;
    let family = family?;
    if family.is_empty() {
        return None;
    }
    let families = canonical_families(&family)?;
    let mut parts = Vec::new();
    if let Some(style) = style {
        parts.push(style.to_owned());
    }
    if let Some(variant) = variant {
        parts.push(variant.to_owned());
    }
    if let Some(stretch) = stretch.filter(|stretch| stretch != "normal") {
        parts.push(stretch);
    }
    if let Some(weight) = weight.filter(|weight| (weight - 400.0).abs() > f32::EPSILON) {
        parts.push(format_number(weight));
    }
    parts.push(format!("{}px", format_number(size)));
    parts.push(families);
    Some(parts.join(" "))
}

fn length_from_token(token: &str, base_size: f32, line_height: f32) -> Option<f32> {
    let token = token.split('/').next().unwrap_or(token);
    let split = token.find(|ch: char| !(ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+')))?;
    let (number, unit) = token.split_at(split);
    let number = number.parse::<f32>().ok()?;
    if !number.is_finite() {
        return None;
    }
    let scale = match unit.to_ascii_lowercase().as_str() {
        "px" => 1.0,
        "%" => base_size / 100.0,
        "em" | "ic" => base_size,
        "rem" | "pc" => 16.0,
        "ex" | "ch" => base_size * 0.5,
        "lh" => line_height,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 96.0 / 25.4,
        "pt" => 96.0 / 72.0,
        "q" => 96.0 / 101.6,
        _ => return None,
    };
    Some(number * scale)
}

fn is_stretch(value: &str) -> bool {
    matches!(
        value,
        "ultra-condensed"
            | "extra-condensed"
            | "condensed"
            | "semi-condensed"
            | "normal"
            | "semi-expanded"
            | "expanded"
            | "extra-expanded"
            | "ultra-expanded"
    )
}

fn canonical_families(value: &str) -> Option<String> {
    let mut families = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => {
                current.push(ch);
                escaped = true;
            }
            '"' if quoted => {
                quoted = false;
                current.push(ch);
            }
            '"' => {
                quoted = true;
                current.push(ch);
            }
            ',' if !quoted => {
                families.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    families.push(current);
    let mut output = Vec::new();
    for family in families {
        let family = family.trim();
        if family.is_empty() || family.contains(['{', '}', ';', '(', ')']) {
            return None;
        }
        if matches!(
            family.to_ascii_lowercase().as_str(),
            "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "default"
        ) {
            return None;
        }
        let lower = family.to_ascii_lowercase();
        if is_generic_family(&lower) {
            output.push(lower);
        } else if family.starts_with('"') {
            output.push(family.to_owned());
        } else if family.contains(' ') {
            output.push(format!("\"{family}\""));
        } else {
            output.push(family.to_owned());
        }
    }
    Some(output.join(", "))
}

fn is_generic_family(value: &str) -> bool {
    matches!(
        value,
        "serif"
            | "sans-serif"
            | "monospace"
            | "cursive"
            | "fantasy"
            | "system-ui"
            | "ui-serif"
            | "ui-sans-serif"
            | "ui-monospace"
            | "ui-rounded"
            | "math"
            | "emoji"
            | "fangsong"
    )
}

fn format_number(value: f32) -> String {
    if (value.fract()).abs() < 0.0001 {
        format!("{}", value.round())
    } else {
        format!("{value}")
    }
}

pub(crate) fn font_size(font: &str) -> f32 {
    parse_font(font).size
}

struct ParsedFont {
    size: f32,
    weight: f32,
    italic: bool,
    small_caps: bool,
    family: Option<String>,
}

fn parse_font(font: &str) -> ParsedFont {
    let mut size = 10.0;
    let mut weight = 400.0;
    let mut italic = false;
    let mut small_caps = false;
    let tokens: Vec<&str> = font.split_whitespace().collect();
    let mut family_start = None;
    for (index, token) in tokens.iter().enumerate() {
        let lower = token.to_ascii_lowercase();
        if lower == "italic" || lower == "oblique" {
            italic = true;
        } else if lower == "small-caps" {
            small_caps = true;
        } else if lower == "bold" || lower == "bolder" {
            weight = 700.0;
        } else if let Some(px) = lower.strip_suffix("px") {
            let px = px.split('/').next().unwrap_or(px);
            if let Ok(value) = px.parse::<f32>() {
                size = value;
                family_start = Some(index + 1);
                break;
            }
        } else if let Ok(value) = lower.parse::<f32>()
            && (1.0..=1000.0).contains(&value)
            && lower.len() <= 3
        {
            weight = value;
        }
    }
    let family = family_start
        .filter(|start| *start < tokens.len())
        .map(|start| tokens[start..].join(" "));
    ParsedFont {
        size: size.max(0.0),
        weight,
        italic,
        small_caps,
        family,
    }
}

struct GlyphPen {
    builder: PathBuilder,
}

impl GlyphPen {
    fn new() -> Self {
        Self {
            builder: PathBuilder::new(),
        }
    }

    fn finish(self) -> Option<Path> {
        self.builder.finish()
    }
}

impl OutlinePen for GlyphPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(x, -y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(x, -y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.builder.quad_to(cx0, -cy0, x, -y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.builder.cubic_to(cx0, -cy0, cx1, -cy1, x, -y);
    }

    fn close(&mut self) {
        self.builder.close();
    }
}
