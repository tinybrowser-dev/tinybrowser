//! Rendered-text collection for `innerText`/`outerText`.
//!
//! Approximation of the spec's rendered text collection steps
//! (<https://html.spec.whatwg.org/multipage/dom.html#rendered-text-collection-steps>):
//! `display`, `white-space`, `visibility`, `text-transform`, `float`, and
//! `position` come from the `style` attribute only, defaulting per tag.
//! Stylo computes the real cascade (see `Node::primary_styles`), but nothing
//! flushes it for DOM reads yet, so consulting it is a follow-up; until
//! then stylesheet rules, pseudo-elements, soft line breaks, and flex/grid
//! `order` are not visible, and those subtests fail honestly.

use blitz_dom::{BaseDocument, NodeData};
use markup5ever::{LocalName, QualName};

use crate::dom::tree::{BlitzId, NodeId, html_namespace};
use crate::dom_string::DomString;

/// The `white-space` modes that change collection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WhiteSpace {
    /// `normal`, `nowrap`: collapse runs, trim at breaks.
    Normal,
    /// `pre`, `pre-wrap`: preserve everything (`\r` still becomes `\n`).
    Pre,
    /// `pre-line`: collapse spaces, preserve newlines.
    PreLine,
}

/// One `visibility` value, inherited until overridden.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Visible,
    Hidden,
}

/// Block-level treatment of one element for break purposes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    /// Inline content, no breaks of its own.
    Inline,
    /// Atomic inline (`img`, `inline-block`, ...): children trimmed, and
    /// surrounding spaces survive on both sides.
    Atomic,
    /// One line break around content.
    Block,
    /// A blank line around content (`p`; `select`/`option`/`optgroup`
    /// share the level but collect structurally before `breaks` is read).
    Paragraph,
    /// Skipped entirely.
    Hidden,
    /// Children hoisted as if unwrapped (`display:contents`).
    Contents,
}

/// Effective `text-transform` with Turkish casing resolved.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Transform {
    None,
    Upper(bool),
    Lower(bool),
    Capitalize,
}

/// The `style` attribute declarations collection reads, parsed once per
/// element.
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
    /// Parses `node`'s `style` attribute: the last declaration of a property
    /// wins, `!important` (ASCII case-insensitive, optional whitespace) is
    /// stripped, names are case-insensitive. Declarations split on `;`
    /// outside quotes and parentheses, so `url("a;b")` survives.
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

    /// Whether the element establishes a flex/grid container (children
    /// blockify).
    fn is_flex(&self) -> bool {
        self.display.is_some_and(|value| {
            value.eq_ignore_ascii_case("flex")
                || value.eq_ignore_ascii_case("grid")
                || value.eq_ignore_ascii_case("inline-flex")
                || value.eq_ignore_ascii_case("inline-grid")
        })
    }

    /// Whether the element itself opts out of rendering with
    /// `display:none`. Unlike `visibility`, descendants cannot re-show
    /// through this.
    fn display_none(&self) -> bool {
        self.display
            .is_some_and(|value| value.eq_ignore_ascii_case("none"))
    }
}

/// Whether `node` is not being rendered for collection: inline
/// `display:none`, or an HTML `hidden` attribute other than `until-found`
/// (the UA sheet's `[hidden] { display: none }`).
/// <https://html.spec.whatwg.org/multipage/interaction.html#the-hidden-attribute>
fn not_rendered(base: &BaseDocument, node: BlitzId, style: &InlineStyle<'_>) -> bool {
    style.display_none() || html_hidden(base, node)
}

/// The table-internal box an element generates, decided by its used
/// `display` value.
///
/// The innerText steps dispatch on the box an element generates, not on its
/// tag, so a `div` with `display:table-cell` from a stylesheet contributes a
/// tab-separated cell exactly like a `td`
/// (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
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

/// The table box `node` generates, or `None` for an element outside table
/// layout.
///
/// The `style` attribute wins because it is the inline presentation hint;
/// otherwise the resolved `display` counts, which is what makes a class rule
/// such as `.cell { display: table-cell }` visible to the collection.
fn table_box_of(base: &BaseDocument, node: BlitzId, style: &InlineStyle<'_>) -> Option<TableBox> {
    let keyword = match style.display {
        Some(display) => display.trim().to_ascii_lowercase(),
        None => base.resolved_style_value(node, "display"),
    };
    table_box_keyword(&keyword)
}

/// The box a `display` keyword names, or `None` for a non-table display.
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

/// HTML `hidden` other than `until-found`, which still renders.
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

/// Splits a `style` attribute into declarations on `;` outside single and
/// double quotes and parentheses. A backslash inside quotes escapes the
/// next character, so `content: "a\";b"` keeps its `;`.
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

/// Strips a trailing `!important` (ASCII case-insensitive, allowing
/// whitespace as in `! important`) from a declaration value.
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

/// Whether `node` is an HTML element with one of these local names.
fn is_html_tag(base: &BaseDocument, node: BlitzId, tags: &[&str]) -> bool {
    base.get_node(node).is_some_and(|tree| {
        tree.data.downcast_element().is_some_and(|element| {
            element.name.ns == crate::dom::tree::html_namespace()
                && tags.contains(&element.name.local.as_str())
        })
    })
}

/// The `white-space` mode of an element: the `style` attribute wins, then
/// `pre`-family tags, then inheritance.
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

/// The display treatment of an element: the `style` attribute wins, with
/// `float` (non-`none`) and absolute positioning blockifying, then tag
/// defaults. Stylesheet classes are invisible, so class-driven layouts
/// keep their tag default and fail honestly.
fn level_of(base: &BaseDocument, node: BlitzId, style: &InlineStyle<'_>) -> Level {
    let Some(tree) = base.get_node(node) else {
        return Level::Hidden;
    };
    let Some(element) = tree.data.downcast_element() else {
        return Level::Inline;
    };
    let html = element.name.ns == crate::dom::tree::html_namespace();
    let local = element.name.local.as_str();
    // `template` and `noscript` never render, regardless of styling: their
    // content is not elements (noscript) or lives in a separate fragment.
    if html && (local == "template" || local == "noscript") {
        return Level::Hidden;
    }
    // `float` and absolute positioning blockify.
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
        // `p` keeps its blank lines under any rendering display, even
        // `inline-block` (margins, not display, drive them).
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
            // Table-internal displays outside a table walk have no
            // anonymous-table repair, so their content collects inline
            // rather than vanishing.
            _ => Level::Inline,
        };
    }
    if blockified {
        return Level::Block;
    }
    // `p` without an explicit display (handled above) is blank-lined.
    if html && local == "p" {
        return Level::Paragraph;
    }
    if !html {
        // Foreign content without styling: known never-rendered SVG
        // containers are skipped, everything else is inline. SVG local
        // names are case-sensitive (`clipPath`, not `clippath`).
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
        // Never rendered (without an overriding `style` attribute, which
        // returned above for rendering displays; `source`/`track` join
        // `script`/`style` here; `template`/`noscript` returned earlier).
        // `rp` never renders (its parens are dropped); an explicit rendering
        // display above still wins.
        "head" | "meta" | "link" | "base" | "title" | "script" | "style" | "source" | "track"
        | "frame" | "frameset" | "noframes" | "noembed" | "datalist" | "rp" => Level::Hidden,
        // Block containers (`li` is `list-item`, block-level; `fieldset`,
        // `legend`, `center`, `search`, `dt`/`dd` are `block`).
        "address" | "article" | "aside" | "blockquote" | "details" | "dialog" | "div" | "dl"
        | "dt" | "dd" | "fieldset" | "legend" | "figcaption" | "figure" | "footer" | "form"
        | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header" | "hgroup" | "main" | "nav" | "ol"
        | "ul" | "li" | "dir" | "menu" | "section" | "summary" | "center" | "search" | "pre"
        | "listing" | "xmp" | "plaintext" | "table" => Level::Block,
        // Atomic inline: replaced elements and controls with ignored content
        // (`button` is `inline-block`).
        "img" | "input" | "textarea" | "iframe" | "audio" | "video" | "canvas" | "object"
        | "embed" | "param" | "map" | "area" | "applet" | "button" => Level::Atomic,
        "select" | "option" | "optgroup" => Level::Paragraph,
        // Transparent containers (`span`, `a`, `b`, table parts, ...) and
        // unknown and custom elements are inline by default.
        _ => Level::Inline,
    }
}

/// Whether the element's subtree contributes no rendered text: replaced
/// and content-ignored elements. `source`/`track`/`script`/`style` render
/// when given an explicit rendering display instead (handled by their
/// level), so they are not listed here.
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

/// The effective `visibility` of an element: an explicit `visible` or
/// hiding value wins, anything else (including invalid values) inherits.
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

/// The effective `text-transform` of an element: explicit style wins, else
/// inherited. Turkish casing resolves `lang` up the ancestor chain.
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

/// Applies an effective transform to already-whitespace-processed text.
/// `word_open` threads across text nodes: a word does not end at an inline
/// element boundary, so `<span style="text-transform:capitalize">f<b>oo</b></span>`
/// is `"Foo"`, not `"FOo"`.
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

/// Processes one text node under `white-space` `mode`: `\r\n` and lone `\r`
/// become `\n` in every mode, then spaces collapse unless preserved
/// (`&nbsp;` never collapses).
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
    // After a preserved newline, collapsible spaces open no run: they are
    // invisible at the line start.
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

/// Collector state for one rendered-text run.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent whitespace and break flags of the rendered-text walk; folding them into a state machine is a refactor"
)]
struct Collector<'a> {
    base: &'a BaseDocument,
    out: String,
    /// The buffer ends with a collapsible space ready to merge.
    pending_space: bool,
    /// The next space run emits even after trailing content (atomic inline
    /// on both sides keeps its spaces).
    force_space: bool,
    /// The last item collected was atomic content with nothing after it
    /// yet: a trailing space materializes at the very end.
    trailing_atomic: bool,
    /// Whether the last emitted character continues a word (for
    /// `text-transform: capitalize` across text-node boundaries). Breaks
    /// and atomic inlines end the word.
    word_open: bool,
    /// Whether the last pushed text preserved spaces (`pre`).
    last_preserve: bool,
    /// Trailing boundary breaks issued since the last content: content
    /// newlines (e.g. inside `pre`) never count, so a following block adds
    /// rather than collapses.
    trailing_breaks: usize,
    /// Whether the trailing break (if any) came from a forced break (`br`).
    trailing_forced: bool,
}

impl Collector<'_> {
    /// Pushes mode-processed text, merging boundary spaces unless preserved.
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
        // A space-only run merges without disturbing the break count, and
        // vanishes entirely against a preceding break: it is already
        // represented there.
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
            if self.force_space || self.pending_space {
                // Atomic boundaries keep their space; an owed space
                // materializes once against the run.
                self.out.push(' ');
            } else if !(self.out.is_empty() || self.out.ends_with([' ', '\n', '\t'])) {
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
            // An owed space materializes before content.
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

    /// Pushes atomic content: trimmed; a pending space flushes first and a
    /// following space run survives. The trailing flag survives when the
    /// atomic had real content or flushed a pending space (both keep their
    /// space at a block end); a bare replaced element with no adjacent
    /// space sets none.
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

    /// A forced break (`br`): always exactly one newline.
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

    /// A table-cell separator: a tab that starts a fresh cell, clearing the
    /// boundary state the previous cell left
    /// (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
    fn push_tab(&mut self) {
        self.out.push('\t');
        self.pending_space = false;
        self.force_space = false;
        self.last_preserve = false;
        self.trailing_breaks = 0;
        self.trailing_forced = false;
        self.trailing_atomic = false;
    }

    /// Ensures `count` block-boundary newlines when the buffer has content,
    /// absorbing a pending space and trailing spaces (unless preserved).
    /// Only breaks issued here count toward collapsing; content newlines
    /// (e.g. inside `pre`) never satisfy a following block. A forced break
    /// survives (a following block still collapses against it via the
    /// count).
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

/// Collects rendered text for `node`'s children.
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

/// Collects one node.
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
            // `visibility:hidden` still lays out: recurse so explicitly
            // visible descendants show, but hide this level's own text.
            // (The text arm skips hidden text; breaks still apply.)
            if html_hidden(base, node) {
                return;
            }
            if html && local == "br" {
                collector.push_forced_break();
                return;
            }
            let mode = white_space_of(base, node, &style, inherited_mode);
            let transform = transform_of(base, node, &style, inherited_transform);
            // A table box collects structurally whatever set its `display`:
            // the `style` attribute, a stylesheet rule, or the UA sheet. A
            // `tr` target (or nested row) collects its cells directly, and a
            // cell reached outside a table still shows its own content.
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
                    // A caption, row group, column, column group, or stray
                    // cell is a transparent container here: its content
                    // collects inline, and an enclosing table walk decides
                    // the separators.
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
                        // A `table` whose `display` a style attribute
                        // overrides still collects its rows structurally.
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
                    // Inside `visibility:hidden` subtrees blocks recurse
                    // transparently (only explicitly visible descendants
                    // show, without their usual blank lines).
                    let transparent = visibility == Visibility::Hidden;
                    // `option`/`optgroup` force breaks even when empty.
                    // A nested `optgroup` in `optgroup` renders nothing at
                    // all: skip it without forcing breaks of its own (the
                    // surrounding items break already).
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
                    // A display-overridden replaced element (e.g. block-level
                    // `img`) breaks without rendering its contents.
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
                    // A trailing atomic keeps its space instead of a break.
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

/// Whether `node` (`optgroup`) sits inside another `optgroup`.
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

/// Collects a `table` subtree: rows newline-separated, cells tab-separated
/// (including trailing empty cells), captions broken. Whitespace directly
/// under table structure is ignored.
///
/// Rows and captions collect in tree order (`tfoot` is not reordered, and a
/// caption after the rows comes after them), as the rendered text
/// collection steps walk child nodes in tree order. Browsers agree
/// (WPT `getter-tests.js` "Newline between cells and caption" expects
/// `abc\ndef` for a caption parsed after its row), even though the default
/// `caption-side: top` draws the caption box first.
fn collect_table(
    collector: &mut Collector<'_>,
    node: BlitzId,
    mode: WhiteSpace,
    visibility: Visibility,
    transform: Transform,
    block: bool,
) {
    // A block-level table gets its own lines; an `inline-table` joins the
    // surrounding text without boundary breaks
    // (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
    if block {
        collector.ensure_breaks(1);
    }
    let base = collector.base;
    let Some(table) = base.get_node(node) else {
        return;
    };
    // Captions count only as direct children; rows at any depth below
    // non-table structure. Row groups pass their `visibility` down so
    // collapsed sections hide their rows.
    let mut stack: Vec<(BlitzId, bool, Visibility, WhiteSpace, Transform)> = table
        .children
        .iter()
        .rev()
        .map(|&child| (child, true, visibility, mode, transform))
        .collect();
    let mut first_row = true;
    // Whether the previous item was a cell of the anonymous row that collects
    // cells which are direct children of the table
    // (<https://drafts.csswg.org/css-tables-3/#anonymous-table-boxes>).
    let mut open_row = false;
    while let Some((current, top, inherited, inherited_mode, inherited_transform)) = stack.pop() {
        let style = InlineStyle::of(base, current);
        // `display:none` and `hidden` subtrees never render, so they
        // contribute no rows.
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
        // A row, cell, or caption counts by its box, not its tag, so a
        // `display:table-row`/`table-cell` element joins the walk
        // (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
        let table_box = table_box_of(base, current, &style);
        let is_row = matches!(kind, Some("tr")) || table_box == Some(TableBox::Row);
        let is_cell = matches!(kind, Some("td" | "th")) || table_box == Some(TableBox::Cell);
        let is_caption = matches!(kind, Some("caption")) || table_box == Some(TableBox::Caption);
        // Rows and cells resolve their own box properties so `tbody`/`tr`
        // styles (e.g. `visibility:collapse`) apply to their cells.
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
            // Cells that are direct children of the table form an anonymous
            // row: tab-separated, with a line break before the row itself.
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
            // A nested table does not join this table's rows.
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
            // A text node between cells does not end the anonymous row, and
            // table whitespace renders nothing
            // (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
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

/// Collects one table row: cells tab-separated (including trailing empty
/// cells). An empty row still breaks: its own line survives the shared
/// separator.
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
        // `display:none` and `hidden` cells never render, not even their
        // tab stop.
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

/// Collects a `select` subtree: each `option`/`optgroup` on its own line,
/// direct text ignored. Other elements are transparent: their `option`
/// descendants still collect (getter-tests `select>div>optgroup` expects
/// `"one\ntwo"`).
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
        // `display:none` and `hidden` options never render.
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
            // Text directly under `select` is ignored.
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
            // Other elements are transparent here (not skipped): an
            // `option` nested inside them still collects.
            _ => {
                collect_node(collector, child, mode, visibility, transform, false);
            }
        }
    }
}

/// Collects a `details` subtree: without `open`, only the first `summary`
/// shows.
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

/// The rendered text of `id`'s subtree: `textContent` when `id` is not being
/// rendered, otherwise the collected text with block-boundary trailing
/// breaks trimmed (a single forced break survives).
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
    // Content-ignored elements and `noscript` render nothing themselves.
    // (Checked before the not-rendered path, which would otherwise answer
    // `textContent`.)
    if subtree_skipped(base, id.node) || is_html_tag(base, id.node, &["noscript"]) {
        return String::new();
    }
    if !being_rendered(base, id.node) {
        return text_content(base, id.node);
    }
    // `br` itself renders nothing.
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
    // The target's own properties seed inheritance, and its own element
    // dispatch applies (tables, selects, and details collect structurally
    // even at the top level): collect the target itself rather than only
    // its children. Leading breaks never arise; trailing ones trim below.
    // The target's own `flex` value does not make it a flex child: the
    // flag means "my parent is a flex container", so resolve the parent.
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
    // Leading block breaks never arise (ensures skip empty buffers), but a
    // leading forced break (`<br>` first) survives.
    if collector.trailing_forced {
        // A forced break survives at the end, collapsed to one.
        while out.ends_with("\n\n") {
            out.pop();
        }
    } else {
        // Block-boundary breaks at the end are dropped; content newlines
        // (e.g. inside `pre`) survive.
        for _ in 0..collector.trailing_breaks {
            if out.ends_with('\n') {
                out.pop();
            }
        }
    }
    out
}

/// Resolves the inherited `white-space`, `visibility`, and
/// `text-transform` at `node` by folding the same per-element helpers
/// collection uses over the ancestor chain, root down. Nearest explicit
/// style wins; `pre`-family tags imply `pre`; otherwise defaults. Sharing
/// the helpers (instead of reimplementing the walk) keeps ancestor
/// resolution and element dispatch from diverging.
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

/// Whether the node is being rendered: no `display:none`, no `hidden`
/// (except `hidden=until-found`, which renders), and no never-rendered tag
/// on the path to the root. The tag list mirrors the `Hidden` arms of
/// `level_of` plus the content-ignored elements, so a target with no
/// rendering takes the `textContent` path instead of collecting nothing.
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
    // Only a connected tree (rooted at the document) renders: a detached
    // element has no box, so it takes the `textContent` path.
    base.get_node(root)
        .is_some_and(|tree| matches!(tree.data, NodeData::Document(_)))
}

/// Descendant text without any processing (the not-rendered path).
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
/// Splits a rendered-text setter value into text and `br` nodes: `\r\n`
/// and lone `\r` become `\n`, then every newline becomes a `br` with no
/// empty text around it. When `pad_empty` and the value is all-empty, yields
/// one empty text node: `outerText = ""` removes the element but the
/// sibling-merge step expects a node to merge (proven by
/// `outertext-setter.html`, fully passing); `innerText = ""` replaces with
/// nothing.
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

/// Whether `id` is a `Text` node strictly (not CDATA): the outerText merge
/// steps only merge `Text`
/// (<https://html.spec.whatwg.org/multipage/dom.html#the-innertext-idl-attribute>).
pub(crate) fn is_mergeable_text_node(base: &blitz_dom::BaseDocument, id: BlitzId) -> bool {
    matches!(
        base.get_node(id).map(|node| &node.data),
        Some(NodeData::Text(_))
    )
}

/// A node's child text content: the data of its Text children in tree order
/// (<https://dom.spec.whatwg.org/#concept-child-text-content>).
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

/// The API value of a text control: the raw value with CRLF and CR normalized
/// to LF (<https://html.spec.whatwg.org/multipage/form-elements.html#concept-textarea-api-value>).
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
