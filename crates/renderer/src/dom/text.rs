// https://html.spec.whatwg.org/multipage/dom.html#rendered-text-collection-steps

use blitz_dom::{BaseDocument, NodeData};
use markup5ever::{LocalName, QualName};

use crate::dom::tree::{BlitzId, NodeId, html_namespace};
use crate::dom_string::DomString;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WhiteSpace {
    Normal,
    Pre,
    PreLine,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Visible,
    Hidden,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Inline,
    Atomic,
    Block,
    Paragraph,
    Hidden,
    Contents,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Transform {
    None,
    Upper(bool),
    Lower(bool),
    Capitalize,
}

#[derive(Clone, Copy, Default)]
struct InlineStyle<'a> {
    display: Option<&'a str>,
    white_space: Option<&'a str>,
    visibility: Option<&'a str>,
    text_transform: Option<&'a str>,
    float: Option<&'a str>,
    position: Option<&'a str>,
}

impl<'a> InlineStyle<'a> {
    fn of(base: &'a BaseDocument, node: BlitzId) -> Self {
        let mut style = Self::default();
        let Some(attribute) = crate::dom::tree::attr(base, node, "style") else {
            return style;
        };
        for declaration in split_declarations(attribute) {
            let Some((name, value)) = declaration.split_once(':') else {
                continue;
            };
            let name = name.trim();
            let slot = if name.eq_ignore_ascii_case("display") {
                &mut style.display
            } else if name.eq_ignore_ascii_case("white-space") {
                &mut style.white_space
            } else if name.eq_ignore_ascii_case("visibility") {
                &mut style.visibility
            } else if name.eq_ignore_ascii_case("text-transform") {
                &mut style.text_transform
            } else if name.eq_ignore_ascii_case("float") {
                &mut style.float
            } else if name.eq_ignore_ascii_case("position") {
                &mut style.position
            } else {
                continue;
            };
            *slot = Some(strip_important(value.trim()));
        }
        style
    }

    fn is_flex(&self) -> bool {
        self.display.is_some_and(|value| {
            value.eq_ignore_ascii_case("flex")
                || value.eq_ignore_ascii_case("grid")
                || value.eq_ignore_ascii_case("inline-flex")
                || value.eq_ignore_ascii_case("inline-grid")
        })
    }

    fn display_none(&self) -> bool {
        self.display
            .is_some_and(|value| value.eq_ignore_ascii_case("none"))
    }
}

// https://html.spec.whatwg.org/multipage/interaction.html#the-hidden-attribute
fn not_rendered(base: &BaseDocument, node: BlitzId, style: &InlineStyle<'_>) -> bool {
    style.display_none() || html_hidden(base, node)
}

// https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableBox {
    Table,
    InlineTable,
    Row,
    Cell,
    Caption,
    RowGroup,
    Column,
    ColumnGroup,
}

fn table_box_of(base: &BaseDocument, node: BlitzId, style: &InlineStyle<'_>) -> Option<TableBox> {
    let keyword = match style.display {
        Some(display) => display.trim().to_ascii_lowercase(),
        None => base.resolved_style_value(node, "display"),
    };
    table_box_keyword(&keyword)
}

fn table_box_keyword(keyword: &str) -> Option<TableBox> {
    Some(match keyword {
        "table" => TableBox::Table,
        "inline-table" => TableBox::InlineTable,
        "table-row" => TableBox::Row,
        "table-cell" => TableBox::Cell,
        "table-caption" => TableBox::Caption,
        "table-row-group" | "table-header-group" | "table-footer-group" => TableBox::RowGroup,
        "table-column" => TableBox::Column,
        "table-column-group" => TableBox::ColumnGroup,
        _ => return None,
    })
}

fn html_hidden(base: &BaseDocument, node: BlitzId) -> bool {
    let Some(tree) = base.get_node(node) else {
        return false;
    };
    let Some(element) = tree.data.downcast_element() else {
        return false;
    };
    if element.name.ns != html_namespace() {
        return false;
    }
    crate::dom::tree::attr(base, node, "hidden")
        .is_some_and(|value| !value.eq_ignore_ascii_case("until-found"))
}

fn split_declarations(style: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_u32;
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    for (index, char) in style.char_indices() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if char == '\\' {
                    escaped = true;
                } else if char == q {
                    quote = None;
                }
            }
            None => match char {
                '"' | '\'' => quote = Some(char),
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                ';' if depth == 0 => {
                    parts.push(&style[start..index]);
                    start = index + 1;
                }
                _ => {}
            },
        }
    }
    parts.push(&style[start..]);
    parts
}

fn strip_important(value: &str) -> &str {
    let trimmed = value.trim_end();
    if let Some(bang) = trimmed.rfind('!') {
        let marker: String = trimmed[bang + 1..]
            .chars()
            .filter(|char| !char.is_whitespace())
            .collect();
        if marker.eq_ignore_ascii_case("important") {
            return trimmed[..bang].trim_end();
        }
    }
    trimmed
}

fn is_html_tag(base: &BaseDocument, node: BlitzId, tags: &[&str]) -> bool {
    base.get_node(node).is_some_and(|tree| {
        tree.data.downcast_element().is_some_and(|element| {
            element.name.ns == crate::dom::tree::html_namespace()
                && tags.contains(&element.name.local.as_str())
        })
    })
}

fn white_space_of(
    base: &BaseDocument,
    node: BlitzId,
    style: &InlineStyle<'_>,
    inherited: WhiteSpace,
) -> WhiteSpace {
    if let Some(value) = style.white_space {
        return match value.to_ascii_lowercase().as_str() {
            "pre" | "pre-wrap" | "break-spaces" => WhiteSpace::Pre,
            "pre-line" => WhiteSpace::PreLine,
            _ => WhiteSpace::Normal,
        };
    }
    if is_html_tag(base, node, &["pre", "listing", "xmp", "plaintext"]) {
        return WhiteSpace::Pre;
    }
    inherited
}

fn level_of(base: &BaseDocument, node: BlitzId, style: &InlineStyle<'_>) -> Level {
    let Some(tree) = base.get_node(node) else {
        return Level::Hidden;
    };
    let Some(element) = tree.data.downcast_element() else {
        return Level::Inline;
    };
    let html = element.name.ns == crate::dom::tree::html_namespace();
    let local = element.name.local.as_str();
    if html && (local == "template" || local == "noscript") {
        return Level::Hidden;
    }
    let blockified = style
        .float
        .is_some_and(|value| !value.is_empty() && !value.eq_ignore_ascii_case("none"))
        || style.position.is_some_and(|value| {
            value.eq_ignore_ascii_case("absolute") || value.eq_ignore_ascii_case("fixed")
        });
    if let Some(display) = style.display {
        let display = display.to_ascii_lowercase();
        let display = display.as_str();
        if display == "none" {
            return Level::Hidden;
        }
        if display == "contents" {
            return Level::Contents;
        }
        if html && local == "p" {
            return Level::Paragraph;
        }
        if blockified
            && !matches!(
                display,
                "block"
                    | "flex"
                    | "grid"
                    | "table"
                    | "list-item"
                    | "inline-block"
                    | "inline-flex"
                    | "inline-grid"
                    | "inline-table"
            )
        {
            return Level::Block;
        }
        return match display {
            "block" | "flex" | "grid" | "table" | "list-item" => Level::Block,
            "inline-block" | "inline-flex" | "inline-grid" | "inline-table" => Level::Atomic,
            _ => Level::Inline,
        };
    }
    if blockified {
        return Level::Block;
    }
    if html && local == "p" {
        return Level::Paragraph;
    }
    if !html {
        return if matches!(
            local,
            "defs"
                | "desc"
                | "metadata"
                | "stop"
                | "title"
                | "mask"
                | "clipPath"
                | "pattern"
                | "symbol"
                | "use"
                | "script"
                | "style"
        ) {
            Level::Hidden
        } else {
            Level::Inline
        };
    }
    match local {
        "head" | "meta" | "link" | "base" | "title" | "script" | "style" | "source" | "track"
        | "frame" | "frameset" | "noframes" | "noembed" | "datalist" | "rp" => Level::Hidden,
        "address" | "article" | "aside" | "blockquote" | "details" | "dialog" | "div" | "dl"
        | "dt" | "dd" | "fieldset" | "legend" | "figcaption" | "figure" | "footer" | "form"
        | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header" | "hgroup" | "main" | "nav" | "ol"
        | "ul" | "li" | "dir" | "menu" | "section" | "summary" | "center" | "search" | "pre"
        | "listing" | "xmp" | "plaintext" | "table" => Level::Block,
        "img" | "input" | "textarea" | "iframe" | "audio" | "video" | "canvas" | "object"
        | "embed" | "param" | "map" | "area" | "applet" | "button" => Level::Atomic,
        "select" | "option" | "optgroup" => Level::Paragraph,
        _ => Level::Inline,
    }
}

fn subtree_skipped(base: &BaseDocument, node: BlitzId) -> bool {
    is_html_tag(
        base,
        node,
        &[
            "input", "textarea", "iframe", "audio", "video", "canvas", "object", "embed", "img",
            "param", "map", "area",
        ],
    )
}

fn visibility_of(style: &InlineStyle<'_>, inherited: Visibility) -> Visibility {
    if let Some(value) = style.visibility {
        if value.eq_ignore_ascii_case("visible") {
            return Visibility::Visible;
        }
        if value.eq_ignore_ascii_case("hidden") || value.eq_ignore_ascii_case("collapse") {
            return Visibility::Hidden;
        }
    }
    inherited
}

fn transform_of(
    base: &BaseDocument,
    node: BlitzId,
    style: &InlineStyle<'_>,
    inherited: Transform,
) -> Transform {
    if let Some(value) = style.text_transform {
        let mut turkish = false;
        let mut cursor = Some(node);
        while let Some(current) = cursor {
            if let Some(lang) = crate::dom::tree::attr(base, current, "lang") {
                let lang = lang.to_ascii_lowercase();
                turkish = lang == "tr" || lang.starts_with("tr-");
                break;
            }
            cursor = base.get_node(current).and_then(|tree| tree.parent);
        }
        return match value.to_ascii_lowercase().as_str() {
            "uppercase" => Transform::Upper(turkish),
            "lowercase" => Transform::Lower(turkish),
            "capitalize" => Transform::Capitalize,
            _ => Transform::None,
        };
    }
    inherited
}

fn apply_transform(text: &str, transform: Transform, word_open: &mut bool) -> String {
    match transform {
        Transform::None => text.to_owned(),
        Transform::Upper(turkish) => {
            if turkish {
                text.chars()
                    .map(|char| match char {
                        'i' => 'İ',
                        'ı' => 'I',
                        _ => char,
                    })
                    .collect::<String>()
                    .to_uppercase()
            } else {
                text.to_uppercase()
            }
        }
        Transform::Lower(turkish) => {
            if turkish {
                text.chars()
                    .map(|char| match char {
                        'I' => 'ı',
                        'İ' => 'i',
                        _ => char,
                    })
                    .collect::<String>()
                    .to_lowercase()
            } else {
                text.to_lowercase()
            }
        }
        Transform::Capitalize => {
            let mut out = String::with_capacity(text.len());
            for char in text.chars() {
                if char.is_alphabetic() {
                    if *word_open {
                        out.push(char);
                    } else {
                        out.extend(char.to_uppercase());
                    }
                    *word_open = true;
                } else {
                    out.push(char);
                    if matches!(char, ' ' | '\t' | '\n' | '\u{c}' | '\u{a0}') {
                        *word_open = false;
                    }
                }
            }
            out
        }
    }
}

fn process_text(text: &str, mode: WhiteSpace) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(char) = chars.next() {
        if char == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(char);
        }
    }
    if mode == WhiteSpace::Pre {
        return normalized;
    }
    let collapse_newlines = mode == WhiteSpace::Normal;
    let mut out = String::with_capacity(normalized.len());
    let mut pending_space = false;
    let mut after_newline = false;
    for char in normalized.chars() {
        if char == '\n' && !collapse_newlines {
            pending_space = false;
            after_newline = true;
            out.push('\n');
        } else if matches!(char, ' ' | '\t' | '\u{c}') || (collapse_newlines && char == '\n') {
            if after_newline {
                continue;
            }
            pending_space = true;
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            after_newline = false;
            out.push(char);
        }
    }
    if pending_space {
        out.push(' ');
    }
    out
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent whitespace and break flags of the rendered-text walk; folding them into a state machine is a refactor"
)]
struct Collector<'a> {
    base: &'a BaseDocument,
    out: String,
    pending_space: bool,
    force_space: bool,
    trailing_atomic: bool,
    word_open: bool,
    last_preserve: bool,
    trailing_breaks: usize,
    trailing_forced: bool,
}

impl Collector<'_> {
    fn push_text(&mut self, text: &str, preserve: bool) {
        if text.is_empty() {
            return;
        }
        if preserve {
            self.pending_space = false;
            self.out.push_str(text);
            self.force_space = false;
            self.last_preserve = true;
            self.trailing_breaks = 0;
            self.trailing_forced = false;
            self.trailing_atomic = false;
            return;
        }
        self.last_preserve = false;
        if text.trim_matches(' ').is_empty() {
            if !(self.out.is_empty() || self.out.ends_with('\n')) {
                self.pending_space = true;
            }
            return;
        }
        self.trailing_breaks = 0;
        self.trailing_forced = false;
        let mut text = text;
        if text.starts_with(' ') {
            if self.force_space
                || self.pending_space
                || !(self.out.is_empty() || self.out.ends_with([' ', '\n', '\t']))
            {
                self.out.push(' ');
            }
            self.pending_space = false;
            self.force_space = false;
            text = text.trim_start_matches(' ');
            if text.is_empty() {
                self.pending_space = true;
                return;
            }
        } else if self.pending_space {
            if !(self.out.is_empty() || self.out.ends_with('\n')) {
                self.out.push(' ');
            }
            self.pending_space = false;
            self.force_space = false;
        } else {
            self.force_space = false;
        }
        if text.ends_with(' ') {
            self.pending_space = true;
            text = text.trim_end_matches(' ');
        } else {
            self.pending_space = false;
        }
        self.out.push_str(text);
        self.trailing_atomic = false;
    }

    fn push_atomic(&mut self, text: &str) {
        let flushed = self.pending_space;
        if flushed {
            self.out.push(' ');
            self.pending_space = false;
        }
        let trimmed = text.trim_matches([' ', '\n', '\t']);
        self.out.push_str(trimmed);
        self.word_open = false;
        self.force_space = true;
        self.last_preserve = false;
        self.trailing_breaks = 0;
        self.trailing_forced = false;
        self.trailing_atomic = !trimmed.is_empty() || flushed;
    }

    fn push_forced_break(&mut self) {
        self.word_open = false;
        self.pending_space = false;
        self.force_space = false;
        if !self.last_preserve {
            while self.out.ends_with(' ') {
                self.out.pop();
            }
        }
        self.last_preserve = false;
        self.out.push('\n');
        self.trailing_breaks += 1;
        self.trailing_forced = true;
        self.trailing_atomic = false;
    }

    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    fn push_tab(&mut self) {
        self.out.push('\t');
        self.pending_space = false;
        self.force_space = false;
        self.last_preserve = false;
        self.trailing_breaks = 0;
        self.trailing_forced = false;
        self.trailing_atomic = false;
    }

    fn ensure_breaks(&mut self, count: usize) {
        self.word_open = false;
        if self.out.is_empty() {
            self.pending_space = false;
            self.force_space = false;
            self.trailing_atomic = false;
            return;
        }
        self.pending_space = false;
        self.force_space = false;
        self.trailing_atomic = false;
        if !self.last_preserve {
            while self.out.ends_with(' ') {
                self.out.pop();
            }
        }
        self.last_preserve = false;
        for _ in self.trailing_breaks..count {
            self.out.push('\n');
        }
        if self.trailing_breaks < count {
            self.trailing_breaks = count;
        }
    }
}

fn collect_children(
    collector: &mut Collector<'_>,
    node: BlitzId,
    mode: WhiteSpace,
    visibility: Visibility,
    transform: Transform,
    flex_parent: bool,
) {
    let base = collector.base;
    let Some(tree) = base.get_node(node) else {
        return;
    };
    for &child in &tree.children {
        collect_node(collector, child, mode, visibility, transform, flex_parent);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "per-node rendered-text steps kept in one place; splitting them is a refactor, not a lint fix"
)]
fn collect_node(
    collector: &mut Collector<'_>,
    node: BlitzId,
    inherited_mode: WhiteSpace,
    inherited_visibility: Visibility,
    inherited_transform: Transform,
    flex_parent: bool,
) {
    let base = collector.base;
    let Some(tree) = base.get_node(node) else {
        return;
    };
    match &tree.data {
        NodeData::Text(text) => {
            if inherited_visibility == Visibility::Hidden {
                return;
            }
            let processed = process_text(&text.content, inherited_mode);
            let transformed =
                apply_transform(&processed, inherited_transform, &mut collector.word_open);
            collector.push_text(&transformed, inherited_mode == WhiteSpace::Pre);
        }
        NodeData::Element(element) => {
            let local: &str = element.name.local.as_str();
            let html = element.name.ns == crate::dom::tree::html_namespace();
            let style = InlineStyle::of(base, node);
            let visibility = visibility_of(&style, inherited_visibility);
            if html_hidden(base, node) {
                return;
            }
            if html && local == "br" {
                collector.push_forced_break();
                return;
            }
            let mode = white_space_of(base, node, &style, inherited_mode);
            let transform = transform_of(base, node, &style, inherited_transform);
            if let Some(table_box) = table_box_of(base, node, &style) {
                match table_box {
                    TableBox::Table => {
                        collect_table(collector, node, mode, visibility, transform, true);
                        return;
                    }
                    TableBox::InlineTable => {
                        collect_table(collector, node, mode, visibility, transform, false);
                        return;
                    }
                    TableBox::Row => {
                        collect_row(collector, node, mode, visibility, transform);
                        return;
                    }
                    TableBox::Caption
                    | TableBox::RowGroup
                    | TableBox::Column
                    | TableBox::ColumnGroup
                    | TableBox::Cell => {
                        if subtree_skipped(base, node) {
                            return;
                        }
                        collect_children(collector, node, mode, visibility, transform, false);
                        return;
                    }
                }
            }
            if html && local == "hr" {
                collector.ensure_breaks(1);
                collect_children(collector, node, mode, visibility, transform, false);
                collector.ensure_breaks(1);
                return;
            }
            let mut level = level_of(base, node, &style);
            if flex_parent && level == Level::Inline {
                level = Level::Block;
            }
            match level {
                Level::Hidden => {}
                Level::Contents => {
                    collect_children(collector, node, mode, visibility, transform, flex_parent);
                }
                Level::Atomic => {
                    if subtree_skipped(base, node) {
                        collector.push_atomic("");
                        return;
                    }
                    let mut inner = Collector {
                        base,
                        out: String::new(),
                        pending_space: false,
                        force_space: false,
                        word_open: false,
                        last_preserve: false,
                        trailing_breaks: 0,
                        trailing_forced: false,
                        trailing_atomic: false,
                    };
                    collect_children(&mut inner, node, mode, visibility, transform, false);
                    collector.push_atomic(&inner.out);
                }
                Level::Block | Level::Paragraph => {
                    if html && local == "table" {
                        collect_table(collector, node, mode, visibility, transform, true);
                        return;
                    }
                    if html && local == "select" {
                        collect_select(collector, node, mode, visibility, transform);
                        return;
                    }
                    if html && local == "details" {
                        collect_details(collector, node, mode, visibility, transform);
                        return;
                    }
                    let breaks = if level == Level::Paragraph { 2 } else { 1 };
                    let transparent = visibility == Visibility::Hidden;
                    if html && (local == "option" || local == "optgroup") {
                        if not_rendered(base, node, &style) {
                            return;
                        }
                        if local == "optgroup" && is_optgroup_ancestor(base, node) {
                            return;
                        }
                        if !transparent {
                            collector.ensure_breaks(1);
                        }
                        collect_children(
                            collector,
                            node,
                            mode,
                            visibility,
                            transform,
                            style.is_flex(),
                        );
                        if !transparent {
                            collector.ensure_breaks(1);
                        }
                        return;
                    }
                    if !transparent {
                        collector.ensure_breaks(breaks);
                    }
                    if !subtree_skipped(base, node) {
                        collect_children(
                            collector,
                            node,
                            mode,
                            visibility,
                            transform,
                            style.is_flex(),
                        );
                    }
                    if !transparent {
                        if collector.trailing_atomic {
                            collector.trailing_atomic = false;
                        } else {
                            collector.ensure_breaks(breaks);
                        }
                    }
                }
                Level::Inline => {
                    if subtree_skipped(base, node) {
                        return;
                    }
                    collect_children(collector, node, mode, visibility, transform, false);
                }
            }
        }
        _ => {}
    }
}

fn is_optgroup_ancestor(base: &BaseDocument, node: BlitzId) -> bool {
    let mut cursor = base.get_node(node).and_then(|tree| tree.parent);
    while let Some(parent) = cursor {
        if is_html_tag(base, parent, &["optgroup"]) {
            return true;
        }
        if is_html_tag(base, parent, &["select"]) {
            return false;
        }
        cursor = base.get_node(parent).and_then(|tree| tree.parent);
    }
    false
}

fn collect_table(
    collector: &mut Collector<'_>,
    node: BlitzId,
    mode: WhiteSpace,
    visibility: Visibility,
    transform: Transform,
    block: bool,
) {
    // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
    if block {
        collector.ensure_breaks(1);
    }
    let base = collector.base;
    let Some(table) = base.get_node(node) else {
        return;
    };
    let mut stack: Vec<(BlitzId, bool, Visibility, WhiteSpace, Transform)> = table
        .children
        .iter()
        .rev()
        .map(|&child| (child, true, visibility, mode, transform))
        .collect();
    let mut first_row = true;
    // https://drafts.csswg.org/css-tables-3/#anonymous-table-boxes
    let mut open_row = false;
    while let Some((current, top, inherited, inherited_mode, inherited_transform)) = stack.pop() {
        let style = InlineStyle::of(base, current);
        if not_rendered(base, current, &style) {
            continue;
        }
        let Some(tree) = base.get_node(current) else {
            continue;
        };
        let kind = tree.data.downcast_element().and_then(|element| {
            (element.name.ns == crate::dom::tree::html_namespace())
                .then_some(element.name.local.as_str())
        });
        // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
        let table_box = table_box_of(base, current, &style);
        let is_row = matches!(kind, Some("tr")) || table_box == Some(TableBox::Row);
        let is_cell = matches!(kind, Some("td" | "th")) || table_box == Some(TableBox::Cell);
        let is_caption = matches!(kind, Some("caption")) || table_box == Some(TableBox::Caption);
        let item_visibility = visibility_of(&style, inherited);
        let item_mode = white_space_of(base, current, &style, inherited_mode);
        let item_transform = transform_of(base, current, &style, inherited_transform);
        if is_row {
            open_row = false;
            if !first_row {
                collector.ensure_breaks(1);
            }
            first_row = false;
            collect_row(
                collector,
                current,
                item_mode,
                item_visibility,
                item_transform,
            );
        } else if is_caption && top {
            open_row = false;
            collector.ensure_breaks(1);
            first_row = false;
            collect_children(
                collector,
                current,
                item_mode,
                item_visibility,
                item_transform,
                false,
            );
        } else if is_cell {
            if open_row {
                collector.push_tab();
            } else {
                if !first_row {
                    collector.ensure_breaks(1);
                }
                first_row = false;
                open_row = true;
            }
            collect_children(
                collector,
                current,
                item_mode,
                item_visibility,
                item_transform,
                false,
            );
        } else if matches!(kind, Some("table")) || table_box == Some(TableBox::Table) {
            open_row = false;
        } else if table_box == Some(TableBox::InlineTable) {
            open_row = false;
            collect_table(
                collector,
                current,
                item_mode,
                item_visibility,
                item_transform,
                false,
            );
        } else if tree.data.downcast_element().is_none() {
            // https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
        } else {
            open_row = false;
            for &child in tree.children.iter().rev() {
                stack.push((child, false, item_visibility, item_mode, item_transform));
            }
        }
    }
    if block {
        collector.ensure_breaks(1);
    }
}

fn collect_row(
    collector: &mut Collector<'_>,
    row: BlitzId,
    mode: WhiteSpace,
    visibility: Visibility,
    transform: Transform,
) {
    let before = collector.out.len();
    let base = collector.base;
    let Some(tree) = base.get_node(row) else {
        return;
    };
    let mut cell_first = true;
    for &cell in &tree.children {
        let Some(cell_tree) = base.get_node(cell) else {
            continue;
        };
        let style = InlineStyle::of(base, cell);
        let is_cell = cell_tree.data.downcast_element().is_some_and(|element| {
            (element.name.ns == crate::dom::tree::html_namespace()
                && matches!(element.name.local.as_str(), "td" | "th"))
                || table_box_of(base, cell, &style) == Some(TableBox::Cell)
        });
        if !is_cell {
            continue;
        }
        if not_rendered(base, cell, &style) {
            continue;
        }
        if !cell_first {
            collector.push_tab();
        }
        cell_first = false;
        let cell_visibility = visibility_of(&style, visibility);
        let cell_mode = white_space_of(base, cell, &style, mode);
        let cell_transform = transform_of(base, cell, &style, transform);
        collect_children(
            collector,
            cell,
            cell_mode,
            cell_visibility,
            cell_transform,
            false,
        );
    }
    if collector.out.len() == before {
        collector.out.push('\n');
        collector.trailing_breaks += 1;
    }
}

fn collect_select(
    collector: &mut Collector<'_>,
    node: BlitzId,
    mode: WhiteSpace,
    visibility: Visibility,
    transform: Transform,
) {
    let base = collector.base;
    let Some(tree) = base.get_node(node) else {
        return;
    };
    for &child in &tree.children {
        let style = InlineStyle::of(base, child);
        if not_rendered(base, child, &style) {
            continue;
        }
        let kind = base.get_node(child).and_then(|tree| {
            tree.data.downcast_element().and_then(|element| {
                (element.name.ns == crate::dom::tree::html_namespace())
                    .then_some(element.name.local.as_str())
            })
        });
        match kind {
            None => {}
            Some("option" | "optgroup") => {
                if kind == Some("optgroup") && is_optgroup_ancestor(base, child) {
                    continue;
                }
                collector.ensure_breaks(1);
                let item_visibility = visibility_of(&style, visibility);
                let item_mode = white_space_of(base, child, &style, mode);
                let item_transform = transform_of(base, child, &style, transform);
                collect_children(
                    collector,
                    child,
                    item_mode,
                    item_visibility,
                    item_transform,
                    false,
                );
                collector.ensure_breaks(1);
            }
            _ => {
                collect_node(collector, child, mode, visibility, transform, false);
            }
        }
    }
}

fn collect_details(
    collector: &mut Collector<'_>,
    node: BlitzId,
    mode: WhiteSpace,
    visibility: Visibility,
    transform: Transform,
) {
    collector.ensure_breaks(1);
    if crate::dom::tree::attr(collector.base, node, "open").is_none() {
        let Some(tree) = collector.base.get_node(node) else {
            return;
        };
        if let Some(&summary) = tree
            .children
            .iter()
            .find(|&&child| is_html_tag(collector.base, child, &["summary"]))
        {
            collect_node(collector, summary, mode, visibility, transform, false);
        }
        collector.ensure_breaks(1);
        return;
    }
    collect_children(
        collector,
        node,
        mode,
        visibility,
        transform,
        InlineStyle::of(collector.base, node).is_flex(),
    );
    collector.ensure_breaks(1);
}

pub(crate) fn rendered_text(base: &BaseDocument, id: NodeId) -> String {
    let Some(tree) = base.get_node(id.node) else {
        return String::new();
    };
    let is_html_element = tree
        .data
        .downcast_element()
        .is_some_and(|element| element.name.ns == html_namespace());
    if !is_html_element {
        return String::new();
    }
    if subtree_skipped(base, id.node) || is_html_tag(base, id.node, &["noscript"]) {
        return String::new();
    }
    if !being_rendered(base, id.node) {
        return text_content(base, id.node);
    }
    if is_html_tag(base, id.node, &["br"]) {
        return String::new();
    }
    let mut collector = Collector {
        base,
        out: String::new(),
        pending_space: false,
        force_space: false,
        word_open: false,
        last_preserve: false,
        trailing_breaks: 0,
        trailing_forced: false,
        trailing_atomic: false,
    };
    let parent_flex = base
        .get_node(id.node)
        .and_then(|tree| tree.parent)
        .is_some_and(|parent| InlineStyle::of(base, parent).is_flex());
    let (top_mode, top_visibility, top_transform) = initial_styles(base, id.node);
    collect_node(
        &mut collector,
        id.node,
        top_mode,
        top_visibility,
        top_transform,
        parent_flex,
    );
    let mut out = collector.out;
    if collector.trailing_forced {
        while out.ends_with("\n\n") {
            out.pop();
        }
    } else {
        for _ in 0..collector.trailing_breaks {
            if out.ends_with('\n') {
                out.pop();
            }
        }
    }
    out
}

fn initial_styles(base: &BaseDocument, node: BlitzId) -> (WhiteSpace, Visibility, Transform) {
    let mut chain = Vec::new();
    let mut cursor = Some(node);
    while let Some(current) = cursor {
        chain.push(current);
        cursor = base.get_node(current).and_then(|tree| tree.parent);
    }
    let mut mode = WhiteSpace::Normal;
    let mut visibility = Visibility::Visible;
    let mut transform = Transform::None;
    for ancestor in chain.into_iter().rev() {
        let style = InlineStyle::of(base, ancestor);
        mode = white_space_of(base, ancestor, &style, mode);
        visibility = visibility_of(&style, visibility);
        transform = transform_of(base, ancestor, &style, transform);
    }
    (mode, visibility, transform)
}

fn being_rendered(base: &BaseDocument, node: BlitzId) -> bool {
    let mut cursor = Some(node);
    let mut root = node;
    while let Some(current) = cursor {
        let Some(tree) = base.get_node(current) else {
            return false;
        };
        root = current;
        if let Some(element) = tree.data.downcast_element() {
            let html = element.name.ns == crate::dom::tree::html_namespace();
            let local = element.name.local.as_str();
            if not_rendered(base, current, &InlineStyle::of(base, current)) {
                return false;
            }
            if html
                && matches!(
                    local,
                    "template"
                        | "noscript"
                        | "head"
                        | "meta"
                        | "link"
                        | "base"
                        | "title"
                        | "script"
                        | "style"
                        | "source"
                        | "track"
                        | "frame"
                        | "frameset"
                        | "noframes"
                        | "noembed"
                        | "datalist"
                        | "rp"
                )
            {
                return false;
            }
            if !html
                && matches!(
                    local,
                    "defs"
                        | "desc"
                        | "metadata"
                        | "stop"
                        | "title"
                        | "mask"
                        | "clipPath"
                        | "pattern"
                        | "symbol"
                        | "use"
                        | "script"
                        | "style"
                )
            {
                return false;
            }
        }
        cursor = tree.parent;
    }
    base.get_node(root)
        .is_some_and(|tree| matches!(tree.data, NodeData::Document(_)))
}

fn text_content(base: &BaseDocument, id: BlitzId) -> String {
    let mut text = String::new();
    let mut stack = base
        .get_node(id)
        .map_or_default(|node| node.children.iter().rev().copied().collect::<Vec<_>>());
    while let Some(current) = stack.pop() {
        let Some(node) = base.get_node(current) else {
            continue;
        };
        match &node.data {
            NodeData::Text(data) => text.push_str(&data.content),
            NodeData::Element(_) | NodeData::Fragment { .. } | NodeData::AnonymousBlock(_) => {
                stack.extend(node.children.iter().rev());
            }
            _ => {}
        }
    }
    text
}
pub(crate) fn rendered_text_fragment(
    parsed: &mut crate::Parsed,
    value: &str,
    pad_empty: bool,
) -> Vec<NodeId> {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
        if !pad_empty {
            return Vec::new();
        }
        let node = parsed.document.create_text(&DomString::default());
        return vec![NodeId {
            document: parsed.id,
            node,
        }];
    }
    let mut added = Vec::new();
    for (index, part) in normalized.split('\n').enumerate() {
        if index > 0 {
            let br = parsed.document.base.mutate().create_element(
                QualName::new(None, html_namespace(), LocalName::from("br")),
                Vec::new(),
            );
            added.push(NodeId {
                document: parsed.id,
                node: br,
            });
        }
        if !part.is_empty() {
            let node = parsed.document.create_text(&DomString::from(part));
            added.push(NodeId {
                document: parsed.id,
                node,
            });
        }
    }
    added
}

// https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute
pub(crate) fn is_mergeable_text_node(base: &blitz_dom::BaseDocument, id: BlitzId) -> bool {
    matches!(
        base.get_node(id).map(|node| &node.data),
        Some(NodeData::Text(_))
    )
}

// https://dom.spec.whatwg.org/#concept-child-text-content
pub(crate) fn child_text_content(base: &blitz_dom::BaseDocument, id: BlitzId) -> String {
    let mut text = String::new();
    let Some(target) = base.get_node(id) else {
        return text;
    };
    for child in &target.children {
        match base.get_node(*child).map(|child| &child.data) {
            Some(NodeData::Text(data)) => text.push_str(&data.content),
            Some(NodeData::CDataSection { contents }) => text.push_str(contents),
            _ => {}
        }
    }
    text
}

// https://html.spec.whatwg.org/multipage/form-elements.html#concept-textarea-api-value
pub(crate) fn normalize_newlines(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(character);
        }
    }
    normalized
}
