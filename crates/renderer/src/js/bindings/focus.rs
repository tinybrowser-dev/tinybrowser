use super::node::element::is_first_summary_child;
use super::{LegacyNullString, events, host_node_id, webdriver_element, world_for_node, wrap_node};
use enumerated::Enumerated;

use blitz_dom::Dirtiness;
use markup5ever::{LocalName, Namespace, QualName};
use rquickjs::{Ctx, Exception, Function, Result, Value, prelude::This};

use crate::documents::JournalEntry;
use crate::dom::input::InputType;
use crate::dom::tree::{BlitzId, NodeId, attr, form_owner_of, html_namespace, is_connected};
use crate::js::world::EventTargetKey;
use crate::js::{FrameNavigation, NavigationTarget, World};

// https://html.spec.whatwg.org/multipage/dom.html#node-remove-focus-fixup
pub(crate) fn fixup_focus_after_removal(ctx: &Ctx<'_>, removed: NodeId) -> Result<()> {
    let world = world_for_node(ctx, removed)?;
    let document = removed.document_id();
    let Some(active) = world.borrow().active_element(document) else {
        return Ok(());
    };
    let mut cursor = Some(active);
    while let Some(current) = cursor {
        if current == removed {
            world.borrow_mut().set_active_element(document, None);
            return Ok(());
        }
        cursor = world.borrow().node_parent(current);
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interaction.html#focusable-area
pub(crate) fn is_focusable(ctx: &Ctx<'_>, node: NodeId) -> Result<bool> {
    let world = world_for_node(ctx, node)?;
    let world = world.borrow();
    let Some(parsed) = world.document(node) else {
        return Ok(false);
    };
    Ok(is_focusable_in(&parsed.document.base, node.node))
}

fn is_focusable_in(base: &blitz_dom::BaseDocument, node: BlitzId) -> bool {
    let Some(tree) = base.get_node(node) else {
        return false;
    };
    let Some(element) = tree.data.downcast_element() else {
        return false;
    };
    if !is_connected(base, node) || is_actually_disabled(base, node) {
        return false;
    }
    // https://html.spec.whatwg.org/multipage/interaction.html#focusable-area
    if !is_rendered(base, node) {
        return false;
    }
    // https://html.spec.whatwg.org/multipage/interaction.html#attr-tabindex
    if attr(base, node, "tabindex")
        .is_some_and(|value| crate::dom::parse::parse_integer(value).is_some())
    {
        return true;
    }
    if element.name.ns != html_namespace() {
        return false;
    }
    // https://html.spec.whatwg.org/multipage/interaction.html#contenteditable
    if attr(base, node, "contenteditable").is_some_and(|value| {
        value.is_empty()
            || value.eq_ignore_ascii_case("true")
            || value.eq_ignore_ascii_case("plaintext-only")
    }) {
        return true;
    }
    match element.name.local.as_str() {
        "input" => !is_hidden_input(base, node),
        "a" => attr(base, node, "href").is_some(),
        "button" | "select" | "textarea" => true,
        _ => false,
    }
}

// https://drafts.csswg.org/css-display-3/#valdef-display-none
fn is_rendered(base: &blitz_dom::BaseDocument, node: BlitzId) -> bool {
    let mut cursor = Some(node);
    while let Some(current) = cursor {
        let display = base.resolved_style_value(current, "display");
        if display.eq_ignore_ascii_case("none") {
            return false;
        }
        cursor = base.get_node(current).and_then(|tree| tree.parent);
    }
    true
}

// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#concept-fe-disabled
fn is_actually_disabled(base: &blitz_dom::BaseDocument, node: BlitzId) -> bool {
    if attr(base, node, "disabled").is_some()
        && matches!(
            node_local_name(base, node).as_deref(),
            Some("button" | "input" | "select" | "textarea" | "optgroup" | "option" | "fieldset")
        )
    {
        return true;
    }
    let mut cursor = base.get_node(node).and_then(|tree| tree.parent);
    while let Some(parent) = cursor {
        if node_local_name(base, parent).as_deref() == Some("fieldset")
            && attr(base, parent, "disabled").is_some()
        {
            let first_legend = base.get_node(parent).and_then(|tree| {
                tree.children
                    .iter()
                    .copied()
                    .find(|&child| node_local_name(base, child).as_deref() == Some("legend"))
            });
            if let Some(legend) = first_legend {
                let mut inner = Some(node);
                while let Some(current) = inner {
                    if current == legend {
                        return false;
                    }
                    inner = base.get_node(current).and_then(|tree| tree.parent);
                }
            }
            return true;
        }
        cursor = base.get_node(parent).and_then(|tree| tree.parent);
    }
    false
}

fn is_hidden_input(base: &blitz_dom::BaseDocument, node: BlitzId) -> bool {
    attr(base, node, "type").is_some_and(|kind| kind.eq_ignore_ascii_case("hidden"))
}

fn node_local_name(base: &blitz_dom::BaseDocument, node: BlitzId) -> Option<String> {
    base.get_node(node)
        .and_then(|tree| tree.data.downcast_element())
        .and_then(|element| {
            (element.name.ns == html_namespace()).then(|| element.name.local.to_string())
        })
}

fn write_attr(parsed: &mut crate::Parsed, node: NodeId, local: &str, value: Option<&str>) {
    let (name, old_value) = {
        let base = &parsed.document.base;
        let Some(tree) = base.get_node(node.node) else {
            return;
        };
        let Some(element) = tree.data.downcast_element() else {
            return;
        };
        let name = element
            .attrs
            .iter()
            .find(|attribute| attribute.name.local.as_str() == local)
            .map_or_else(
                || QualName::new(None, Namespace::from(""), LocalName::from(local)),
                |attribute| attribute.name.clone(),
            );
        (name, attr(base, node.node, local).map(str::to_owned))
    };
    if old_value.as_deref() == value {
        return;
    }
    match value {
        Some(value) => {
            parsed
                .document
                .base
                .mutate()
                .set_attribute(node.node, name, value);
        }
        None => {
            parsed
                .document
                .base
                .mutate()
                .clear_attribute(node.node, name);
        }
    }
    parsed.document.record(JournalEntry::Attributes {
        target: node,
        name: local.to_owned(),
        namespace: String::new(),
        old_value,
    });
}

// https://html.spec.whatwg.org/multipage/input.html#radio-button-group
fn radio_group_checked(
    base: &blitz_dom::BaseDocument,
    document: u32,
    node: BlitzId,
) -> Option<NodeId> {
    if checkedness(base, node) {
        return Some(NodeId { document, node });
    }
    let name = attr(base, node, "name").unwrap_or_default();
    crate::dom::forms::radio_group_members(base, document, name, node)
        .into_iter()
        .find(|member| checkedness(base, member.node))
}

// https://html.spec.whatwg.org/multipage/input.html#concept-input-checked
fn checkedness(base: &blitz_dom::BaseDocument, node: BlitzId) -> bool {
    base.get_node(node)
        .and_then(|target| target.data.downcast_element())
        .is_some_and(blitz_dom::ElementData::checkedness)
}

// https://html.spec.whatwg.org/multipage/input.html#dom-input-indeterminate
fn indeterminate(base: &blitz_dom::BaseDocument, node: BlitzId) -> bool {
    base.get_node(node)
        .and_then(|target| target.data.downcast_element())
        .is_some_and(|element| element.form_state.indeterminate())
}

// https://dom.spec.whatwg.org/#concept-event-dispatch
pub(crate) fn activation_target(
    ctx: &Ctx<'_>,
    target: NodeId,
    bubbles: bool,
) -> Result<Option<NodeId>> {
    let world = world_for_node(ctx, target)?;
    let world = world.borrow();
    let Some(parsed) = world.document(target) else {
        return Ok(None);
    };
    let base = &parsed.document.base;
    let mut cursor = Some(target.node);
    while let Some(current) = cursor {
        if has_activation(base, current) {
            return Ok(Some(NodeId {
                document: target.document,
                node: current,
            }));
        }
        if !bubbles {
            break;
        }
        cursor = base.get_node(current).and_then(|node| node.parent);
    }
    Ok(None)
}

fn has_activation(base: &blitz_dom::BaseDocument, node: BlitzId) -> bool {
    let Some(tree) = base.get_node(node) else {
        return false;
    };
    let Some(element) = tree.data.downcast_element() else {
        return false;
    };
    // https://svgwg.org/svg2-draft/interact.html#SVGLinkElementEventHandling
    let svg_anchor =
        element.name.ns == crate::dom::tree::svg_namespace() && element.name.local.as_str() == "a";
    if element.name.ns != html_namespace() && !svg_anchor {
        return false;
    }
    match element.name.local.as_str() {
        "input" => matches!(
            crate::dom::input::input_type(base, node),
            Some(
                InputType::Submit
                    | InputType::Image
                    | InputType::Reset
                    | InputType::Checkbox
                    | InputType::Radio
                    | InputType::Color
                    | InputType::Date
                    | InputType::DatetimeLocal
                    | InputType::File
                    | InputType::Month
                    | InputType::Time
                    | InputType::Week
            )
        ),
        "button" | "label" => true,
        "a" | "area" => attr(base, node, "href").is_some(),
        // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-summary-element:activation-behavior
        "summary" => base
            .get_node(node)
            .and_then(|tree| tree.parent)
            .is_some_and(|parent| is_first_summary_child(base, parent, node)),
        _ => false,
    }
}

// https://html.spec.whatwg.org/multipage/interaction.html#focus-update-steps
pub(crate) fn focus_node(ctx: &Ctx<'_>, node: NodeId) -> Result<()> {
    let world = world_for_node(ctx, node)?;
    let document = node.document_id();
    let previous = world.borrow().active_element(document);
    if previous == Some(node) {
        return Ok(());
    }
    // https://html.spec.whatwg.org/multipage/interaction.html#focus-update-steps
    world.borrow_mut().set_active_element(document, None);
    if let Some(previous) = previous {
        events::fire_trusted_focus(
            ctx,
            EventTargetKey::Node(previous),
            "blur",
            false,
            Some(node),
        )?;
        if world.borrow().active_element(document).is_some() {
            events::fire_trusted_focus(
                ctx,
                EventTargetKey::Node(previous),
                "focusout",
                true,
                Some(node),
            )?;
            return Ok(());
        }
        events::fire_trusted_focus(
            ctx,
            EventTargetKey::Node(previous),
            "focusout",
            true,
            Some(node),
        )?;
    }
    if !is_focusable(ctx, node)? {
        return Ok(());
    }
    world.borrow_mut().set_active_element(document, Some(node));
    events::fire_trusted_focus(ctx, EventTargetKey::Node(node), "focus", false, previous)?;
    events::fire_trusted_focus(ctx, EventTargetKey::Node(node), "focusin", true, previous)?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interaction.html#dom-blur
pub(crate) fn blur_node(ctx: &Ctx<'_>, node: NodeId) -> Result<()> {
    let world = world_for_node(ctx, node)?;
    let document = node.document_id();
    if world.borrow().active_element(document) != Some(node) {
        return Ok(());
    }
    world.borrow_mut().set_active_element(document, None);
    events::fire_trusted_focus(ctx, EventTargetKey::Node(node), "blur", false, None)?;
    events::fire_trusted_focus(ctx, EventTargetKey::Node(node), "focusout", true, None)?;
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interaction.html#dom-click
pub(crate) fn element_click(ctx: &Ctx<'_>, node: NodeId) -> Result<()> {
    let world = world_for_node(ctx, node)?;
    {
        let world = world.borrow();
        let Some(parsed) = world.document(node) else {
            return Ok(());
        };
        if is_actually_disabled(&parsed.document.base, node.node) || world.click_in_progress(node) {
            return Ok(());
        }
    }
    world.borrow_mut().set_click_in_progress(node, true);
    let result = (|| {
        // https://html.spec.whatwg.org/multipage/interaction.html#dom-click
        if is_focusable(ctx, node)? {
            focus_node(ctx, node)?;
        }
        // https://html.spec.whatwg.org/multipage/input.html#the-input-element:legacy-pre-activation-behavior
        let previous = legacy_pre_activation(ctx, node)?;
        let event = events::synthetic_click_event(ctx)?;
        let not_canceled = events::dispatch_event(ctx, EventTargetKey::Node(node), &event)?;
        if not_canceled {
            complete_activation(ctx, node, &previous, node)?;
        } else {
            legacy_canceled_activation(ctx, node, &previous)?;
        }
        Ok(())
    })();
    world.borrow_mut().set_click_in_progress(node, false);
    result
}

pub(crate) enum PreActivation {
    None,
    Checkbox { checked: bool, indeterminate: bool },
    Radio(Option<NodeId>),
}

// https://html.spec.whatwg.org/multipage/input.html#concept-input-checked-dirty
pub(crate) fn legacy_pre_activation(ctx: &Ctx<'_>, node: NodeId) -> Result<PreActivation> {
    let world = world_for_node(ctx, node)?;
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(PreActivation::None);
    };
    let type_attr = {
        let base = &parsed.document.base;
        let Some(tree) = base.get_node(node.node) else {
            return Ok(PreActivation::None);
        };
        let Some(element) = tree.data.downcast_element() else {
            return Ok(PreActivation::None);
        };
        if element.name.ns != html_namespace() || element.name.local.as_str() != "input" {
            return Ok(PreActivation::None);
        }
        crate::dom::input::input_type(base, node.node).unwrap_or(InputType::Text)
    };
    Ok(match type_attr {
        InputType::Checkbox => {
            let checked = checkedness(&parsed.document.base, node.node);
            let indeterminate = indeterminate(&parsed.document.base, node.node);
            let previous = PreActivation::Checkbox {
                checked,
                indeterminate,
            };
            let mut mutator = parsed.document.base.mutate();
            mutator.set_checkedness(node.node, !checked, Dirtiness::Set);
            // https://html.spec.whatwg.org/multipage/input.html#checkbox-state-(type=checkbox
            mutator.set_indeterminate(node.node, false);
            previous
        }
        InputType::Radio => {
            // https://html.spec.whatwg.org/multipage/input.html#radio-button-state-(type=radio
            if checkedness(&parsed.document.base, node.node) {
                PreActivation::None
            } else {
                let previous = radio_group_checked(&parsed.document.base, node.document, node.node);
                let name = attr(&parsed.document.base, node.node, "name").unwrap_or_default();
                let members = crate::dom::forms::radio_group_members(
                    &parsed.document.base,
                    node.document,
                    name,
                    node.node,
                );
                let mut mutator = parsed.document.base.mutate();
                for member in members {
                    mutator.set_checkedness(member.node, false, Dirtiness::Keep);
                }
                mutator.set_checkedness(node.node, true, Dirtiness::Set);
                mutator.set_indeterminate(node.node, false);
                PreActivation::Radio(previous)
            }
        }
        _ => PreActivation::None,
    })
}

pub(crate) fn legacy_canceled_activation(
    ctx: &Ctx<'_>,
    node: NodeId,
    previous: &PreActivation,
) -> Result<()> {
    let world = world_for_node(ctx, node)?;
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(());
    };
    match previous {
        PreActivation::Checkbox {
            checked,
            indeterminate,
        } => {
            let mut mutator = parsed.document.base.mutate();
            mutator.set_checkedness(node.node, *checked, Dirtiness::Set);
            // https://html.spec.whatwg.org/multipage/input.html#canceled-activation-steps
            mutator.set_indeterminate(node.node, *indeterminate);
        }
        PreActivation::Radio(Some(other)) => {
            let mut mutator = parsed.document.base.mutate();
            mutator.set_checkedness(node.node, false, Dirtiness::Set);
            mutator.set_checkedness(other.node, true, Dirtiness::Keep);
        }
        PreActivation::Radio(None) => {
            parsed
                .document
                .base
                .mutate()
                .set_checkedness(node.node, false, Dirtiness::Set);
        }
        PreActivation::None => {}
    }
    Ok(())
}

pub(crate) fn complete_activation(
    ctx: &Ctx<'_>,
    node: NodeId,
    previous: &PreActivation,
    event_target: NodeId,
) -> Result<()> {
    // https://html.spec.whatwg.org/multipage/input.html#the-input-element:activation-behavior
    let still_checkable = {
        let world = world_for_node(ctx, node)?;
        let world = world.borrow();
        world.document(node).is_some_and(|parsed| {
            matches!(
                crate::dom::input::input_type(&parsed.document.base, node.node),
                Some(InputType::Checkbox | InputType::Radio)
            )
        })
    };
    if still_checkable
        && matches!(
            previous,
            PreActivation::Checkbox { .. } | PreActivation::Radio(_)
        )
    {
        fire_checkable_events(ctx, node)
    } else {
        run_activation(ctx, node, event_target)
    }
}

fn fire_checkable_events(ctx: &Ctx<'_>, node: NodeId) -> Result<()> {
    // https://html.spec.whatwg.org/multipage/input.html#checkbox-state-(type=checkbox
    let connected = world_for_node(ctx, node)?
        .borrow()
        .document(node)
        .is_some_and(|parsed| crate::dom::tree::is_connected(&parsed.document.base, node.node));
    if !connected {
        return Ok(());
    }
    events::fire_trusted_composed(ctx, EventTargetKey::Node(node), "input", true, false)?;
    events::fire_trusted(ctx, EventTargetKey::Node(node), "change", true, false)?;
    Ok(())
}

enum Activation {
    None,
    Submit,
    Reset,
    ToggleCheckedness,
    // https://html.spec.whatwg.org/multipage/text-level-semantics.html#the-a-element:activation-behaviour
    Link,
    // https://html.spec.whatwg.org/multipage/forms.html#the-label-element:activation-behavior
    Label,
    // https://html.spec.whatwg.org/multipage/interactive-elements.html#the-summary-element:activation-behavior
    Summary,
}

// https://html.spec.whatwg.org/multipage/input.html#checkbox-state-(type=checkbox
fn toggle_checkedness(ctx: &Ctx<'_>, node: NodeId) -> Result<()> {
    let world = world_for_node(ctx, node)?;
    let changed = {
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(node) else {
            return Ok(());
        };
        let type_attr = {
            let base = &parsed.document.base;
            let Some(tree) = base.get_node(node.node) else {
                return Ok(());
            };
            let Some(element) = tree.data.downcast_element() else {
                return Ok(());
            };
            if element.name.ns != html_namespace() || element.name.local.as_str() != "input" {
                return Ok(());
            }
            crate::dom::input::input_type(base, node.node).unwrap_or(InputType::Text)
        };
        match type_attr {
            InputType::Checkbox => {
                let checked = checkedness(&parsed.document.base, node.node);
                let mut mutator = parsed.document.base.mutate();
                mutator.set_checkedness(node.node, !checked, Dirtiness::Set);
                mutator.set_indeterminate(node.node, false);
                true
            }
            InputType::Radio => {
                if checkedness(&parsed.document.base, node.node) {
                    false
                } else {
                    // https://html.spec.whatwg.org/multipage/input.html#radio-button-state
                    let name = attr(&parsed.document.base, node.node, "name")
                        .unwrap_or_default()
                        .to_owned();
                    let members = crate::dom::forms::radio_group_members(
                        &parsed.document.base,
                        node.document,
                        &name,
                        node.node,
                    );
                    let mut mutator = parsed.document.base.mutate();
                    for member in members {
                        mutator.set_checkedness(member.node, false, Dirtiness::Keep);
                    }
                    mutator.set_checkedness(node.node, true, Dirtiness::Set);
                    mutator.set_indeterminate(node.node, false);
                    true
                }
            }
            _ => false,
        }
    };
    if changed {
        fire_checkable_events(ctx, node)?;
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/links.html#cannot-navigate
pub(super) fn cannot_navigate(world: &World, node: NodeId) -> bool {
    if !world.is_main_document(node) {
        return true;
    }
    let Some(parsed) = world.document(node) else {
        return true;
    };
    let base = &parsed.document.base;
    let anchor = node_local_name(base, node.node).as_deref() == Some("a");
    !anchor && !is_connected(base, node.node)
}

// https://html.spec.whatwg.org/multipage/links.html#hyperlink-referrer-policy
fn hyperlink_referrer_policy(
    base: &blitz_dom::BaseDocument,
    node: BlitzId,
) -> Option<net::ReferrerPolicy> {
    if attr(base, node, "rel")
        .unwrap_or_default()
        .split_ascii_whitespace()
        .any(|token| token.eq_ignore_ascii_case("noreferrer"))
    {
        return Some(net::ReferrerPolicy::NoReferrer);
    }
    attr(base, node, "referrerpolicy").and_then(net::ReferrerPolicy::parse)
}

// https://html.spec.whatwg.org/multipage/forms.html#the-label-element:activation-behavior
fn run_label_activation(ctx: &Ctx<'_>, label: NodeId, event_target: NodeId) -> Result<()> {
    let world = world_for_node(ctx, label)?;
    {
        let world = world.borrow();
        let Some(parsed) = world.document(label) else {
            return Ok(());
        };
        let base = &parsed.document.base;
        let mut cursor = Some(event_target.node);
        while let Some(current) = cursor {
            if current == label.node {
                break;
            }
            if is_interactive_content(base, current) {
                return Ok(());
            }
            cursor = base.get_node(current).and_then(|target| target.parent);
        }
    }
    // https://html.spec.whatwg.org/multipage/forms.html#dom-label-control
    let label_value = wrap_node(ctx, label)?;
    let Some(label_object) = label_value.as_object() else {
        return Ok(());
    };
    let control: Value<'_> = label_object.get("control")?;
    if let Some(control) = host_node_id(ctx, &control) {
        let inside_control = {
            let world = world_for_node(ctx, label)?;
            let world = world.borrow();
            let Some(parsed) = world.document(label) else {
                return Ok(());
            };
            let base = &parsed.document.base;
            let mut cursor = Some(event_target.node);
            let mut inside = false;
            while let Some(current) = cursor {
                if current == control.node {
                    inside = true;
                    break;
                }
                if current == label.node {
                    break;
                }
                cursor = base.get_node(current).and_then(|target| target.parent);
            }
            inside
        };
        if !inside_control {
            element_click(ctx, control)?;
        }
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interactive-elements.html#the-summary-element:activation-behavior
fn toggle_details(ctx: &Ctx<'_>, summary: NodeId) -> Result<()> {
    let world = world_for_node(ctx, summary)?;
    let details = {
        let world = world.borrow();
        let Some(parsed) = world.document(summary) else {
            return Ok(());
        };
        let base = &parsed.document.base;
        let Some(parent) = base.get_node(summary.node).and_then(|tree| tree.parent) else {
            return Ok(());
        };
        if !is_first_summary_child(base, parent, summary.node) {
            return Ok(());
        }
        NodeId {
            document: summary.document,
            node: parent,
        }
    };
    let open = world
        .borrow()
        .document(details)
        .is_some_and(|parsed| attr(&parsed.document.base, details.node, "open").is_some());
    if open {
        super::remove_attribute_sync(ctx, details, "", "open", false)?;
    } else {
        super::set_attribute_sync(ctx, details, "open", "")?;
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/dom.html#interactive-content
fn is_interactive_content(base: &blitz_dom::BaseDocument, node: BlitzId) -> bool {
    let Some(target) = base.get_node(node) else {
        return false;
    };
    let Some(element) = target.data.downcast_element() else {
        return false;
    };
    if element.name.ns != html_namespace() {
        return false;
    }
    match element.name.local.as_str() {
        "a" => attr(base, node, "href").is_some(),
        "audio" | "video" => attr(base, node, "controls").is_some(),
        "button" | "details" | "iframe" | "embed" | "select" | "textarea" => true,
        "input" => {
            crate::dom::input::input_type(base, node) != Some(crate::dom::input::InputType::Hidden)
        }
        "img" => attr(base, node, "usemap").is_some(),
        _ => false,
    }
}

// https://html.spec.whatwg.org/multipage/form-elements.html#the-button-element:activation-behavior
fn activation_behavior(world: &World, node: NodeId) -> Option<(Activation, Option<NodeId>, bool)> {
    let parsed = world.document(node)?;
    let base = &parsed.document.base;
    let tree = base.get_node(node.node)?;
    let element = tree.data.downcast_element()?;
    // https://svgwg.org/svg2-draft/interact.html#SVGLinkElementEventHandling
    let svg_anchor =
        element.name.ns == crate::dom::tree::svg_namespace() && element.name.local.as_str() == "a";
    if element.name.ns != html_namespace() && !svg_anchor {
        return None;
    }
    let mut consumes_activation = false;
    let activation = match element.name.local.as_str() {
        "input" => {
            let kind = crate::dom::input::input_type(base, node.node).unwrap_or(InputType::Text);
            // https://html.spec.whatwg.org/multipage/input.html#input-activation-behavior
            consumes_activation = matches!(
                kind,
                InputType::Color
                    | InputType::Date
                    | InputType::DatetimeLocal
                    | InputType::File
                    | InputType::Month
                    | InputType::Time
                    | InputType::Week
            );
            match kind {
                // https://html.spec.whatwg.org/multipage/input.html#image-button-state-(type=image
                InputType::Submit | InputType::Image => Activation::Submit,
                InputType::Reset => Activation::Reset,
                InputType::Checkbox | InputType::Radio => Activation::ToggleCheckedness,
                _ => Activation::None,
            }
        }
        "button" => {
            // https://html.spec.whatwg.org/multipage/form-elements.html#the-button-element:activation-behavior
            let explicit = attr(base, node.node, "type");
            let has_command = attr(base, node.node, "command").is_some()
                || attr(base, node.node, "commandfor").is_some();
            if explicit.is_none() && has_command {
                Activation::None
            } else {
                // https://html.spec.whatwg.org/multipage/form-elements.html#attr-button-type
                match explicit {
                    Some(value) if value.eq_ignore_ascii_case("reset") => Activation::Reset,
                    Some(value) if value.eq_ignore_ascii_case("button") => Activation::None,
                    _ => Activation::Submit,
                }
            }
        }
        "a" | "area" if attr(base, node.node, "href").is_some() => Activation::Link,
        // https://html.spec.whatwg.org/multipage/forms.html#the-label-element:activation-behavior
        "label" => Activation::Label,
        "summary" => Activation::Summary,
        _ => Activation::None,
    };
    let form = form_owner_of(base, node.document, node.node);
    Some((activation, form, consumes_activation))
}

// https://html.spec.whatwg.org/multipage/form-elements.html#the-button-element:activation-behavior
fn run_activation(ctx: &Ctx<'_>, node: NodeId, event_target: NodeId) -> Result<()> {
    let world = world_for_node(ctx, node)?;
    let Some((activation, form, consumes_activation)) = activation_behavior(&world.borrow(), node)
    else {
        return Ok(());
    };
    if consumes_activation {
        world.borrow_mut().consume_user_activation();
    }
    match activation {
        Activation::None => Ok(()),
        Activation::ToggleCheckedness => toggle_checkedness(ctx, node),
        Activation::Link => follow_link(ctx, node),
        Activation::Label => run_label_activation(ctx, node, event_target),
        Activation::Summary => toggle_details(ctx, node),
        Activation::Submit | Activation::Reset => {
            // https://html.spec.whatwg.org/multipage/input.html#the-input-element:activation-behaviour
            // https://html.spec.whatwg.org/multipage/form-elements.html#the-button-element:activation-behavior
            let disabled = world
                .borrow()
                .document(node)
                .is_some_and(|parsed| is_actually_disabled(&parsed.document.base, node.node));
            if disabled {
                return Ok(());
            }
            let Some(form) = form else {
                return Ok(());
            };
            let form_value = wrap_node(ctx, form)?;
            let button_value = wrap_node(ctx, node)?;
            // https://html.spec.whatwg.org/multipage/input.html#submit-button-state-(type=submit
            // https://html.spec.whatwg.org/multipage/input.html#reset-button-state-(type=reset
            let host = crate::js::bridge::object(ctx)?;
            match activation {
                Activation::Submit => {
                    if let Ok(function) = host.get::<_, Function>("__tbRequestSubmit")
                        && function
                            .call::<_, ()>((This(host.clone()), form_value, button_value))
                            .is_err()
                    {
                        let _ = ctx.catch();
                    }
                }
                Activation::Reset => {
                    if let Ok(function) = host.get::<_, Function>("__tbResetForm")
                        && function.call::<_, ()>((This(host), form_value)).is_err()
                    {
                        let _ = ctx.catch();
                    }
                }
                Activation::None
                | Activation::ToggleCheckedness
                | Activation::Link
                | Activation::Label
                | Activation::Summary => {}
            }
            Ok(())
        }
    }
}

// https://html.spec.whatwg.org/multipage/links.html#following-hyperlinks-2
fn follow_link(ctx: &Ctx<'_>, node: NodeId) -> Result<()> {
    let world = world_for_node(ctx, node)?;
    if cannot_navigate(&world.borrow(), node) {
        return Ok(());
    }
    let (spec, target, referrer_policy) = {
        let world = world.borrow();
        let Some(parsed) = world.document(node) else {
            return Ok(());
        };
        let base = &parsed.document.base;
        (
            crate::dom::tree::attr(base, node.node, "href").map(str::to_owned),
            crate::dom::tree::attr(base, node.node, "target").map(str::to_owned),
            hyperlink_referrer_policy(base, node.node),
        )
    };
    let Some(spec) = spec else {
        return Ok(());
    };
    let navigation = FrameNavigation::get(
        NavigationTarget::Target(crate::protocol::TargetName::parse(
            target.as_deref().unwrap_or_default(),
        )),
        spec,
    )
    .with_referrer_policy(referrer_policy);
    world.borrow_mut().queue_frame_navigation(navigation);
    Ok(())
}

pub(crate) fn install_webdriver_bridge(ctx: &Ctx<'_>) -> Result<()> {
    let globals = crate::js::bridge::object(ctx)?;
    globals.set(
        "__tb_webdriver_click",
        rquickjs::prelude::Func::from(webdriver_click),
    )?;
    globals.set(
        "__tb_webdriver_element",
        rquickjs::prelude::Func::from(webdriver_element),
    )?;
    globals.set(
        "__tbActivate",
        rquickjs::prelude::Func::from(activate_element),
    )?;
    globals.set(
        "__tbSetNativeValue",
        rquickjs::prelude::Func::from(set_native_value),
    )?;
    globals.set(
        "__tbUserActivationActive",
        rquickjs::prelude::Func::from(user_activation_active),
    )?;
    globals.set(
        "__tbUserActivationSticky",
        rquickjs::prelude::Func::from(user_activation_sticky),
    )?;
    globals.set(
        "__tbConsumeUserActivation",
        rquickjs::prelude::Func::from(consume_user_activation),
    )?;
    globals.set("__tbSetActive", rquickjs::prelude::Func::from(set_active))?;
    globals.set(
        "__tbWebDriverSetActiveElement",
        rquickjs::prelude::Func::from(webdriver_set_active_element),
    )?;
    globals.set("__tbFocusNext", rquickjs::prelude::Func::from(focus_next))?;
    Ok(())
}

// https://w3c.github.io/webdriver/#element-send-keys
fn webdriver_set_active_element<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<()> {
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    let world = world_for_node(&ctx, node)?;
    world
        .borrow_mut()
        .set_active_element(node.document_id(), Some(node));
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interaction.html#the-tabindex-attribute
fn sequential_tabindex(base: &blitz_dom::BaseDocument, node: BlitzId) -> Option<i64> {
    if !is_focusable_in(base, node) {
        return None;
    }
    let Some(raw) = attr(base, node, "tabindex") else {
        return Some(0);
    };
    let value = raw.trim_ascii().parse::<i64>().ok()?;
    (value >= 0).then_some(value)
}

// https://html.spec.whatwg.org/multipage/interaction.html#sequential-focus-navigation
fn focus_next(ctx: Ctx<'_>, backward: bool) -> Result<()> {
    let world_rc = super::world(&ctx)?;
    let (focusables, representative_of, current) = {
        let world = world_rc.borrow();
        let Some(parsed) = world.main_document() else {
            return Ok(());
        };
        let base = &parsed.document.base;
        let document = parsed.id;
        let mut order = Vec::new();
        let mut stack = vec![base.root_node().id];
        while let Some(id) = stack.pop() {
            order.push(NodeId { document, node: id });
            if let Some(node) = base.get_node(id) {
                stack.extend(node.children.iter().rev().copied());
            }
        }
        let mut positions = std::collections::HashMap::new();
        for (index, node) in order.iter().enumerate() {
            positions.insert(*node, index);
        }
        let mut tab_stops = Vec::new();
        let mut representative_of = std::collections::HashMap::new();
        for (order_index, node) in order.into_iter().enumerate() {
            let Some(tabindex) = sequential_tabindex(base, node.node) else {
                continue;
            };
            if crate::dom::input::input_type(base, node.node) == Some(InputType::Radio) {
                let name = attr(base, node.node, "name").unwrap_or_default();
                let mut group =
                    crate::dom::forms::radio_group_members(base, document, name, node.node);
                group.push(node);
                group.sort_by_key(|member| positions.get(member).copied().unwrap_or(usize::MAX));
                let checked = group
                    .iter()
                    .find(|member| checkedness(base, member.node))
                    .copied();
                let representative = checked
                    .filter(|member| sequential_tabindex(base, member.node).is_some())
                    .or_else(|| {
                        group
                            .iter()
                            .copied()
                            .find(|member| sequential_tabindex(base, member.node).is_some())
                    });
                if let Some(representative) = representative {
                    for member in &group {
                        representative_of.insert(*member, representative);
                    }
                    // https://html.spec.whatwg.org/multipage/interaction.html#sequential-focus-navigation
                    if representative == node {
                        tab_stops.push((tabindex, order_index, node));
                    }
                }
                continue;
            }
            tab_stops.push((tabindex, order_index, node));
        }
        tab_stops.sort_by_key(|(tabindex, order_index, _)| {
            // https://html.spec.whatwg.org/multipage/interaction.html#sequential-focus-navigation-starting-point
            let positive = *tabindex > 0;
            (
                i32::from(!positive),
                if positive { *tabindex } else { 0 },
                *order_index,
            )
        });
        let focusables: Vec<NodeId> = tab_stops.into_iter().map(|(_, _, node)| node).collect();
        (
            focusables,
            representative_of,
            world.active_element(document),
        )
    };
    if focusables.is_empty() {
        return Ok(());
    }
    let index = current.and_then(|active| focusables.iter().position(|node| *node == active));
    if index.is_none()
        && !backward
        && let Some(representative) = current.and_then(|active| representative_of.get(&active))
        && focusables.contains(representative)
    {
        return focus_node(&ctx, *representative);
    }
    let next = if backward {
        index.map_or(focusables.len() - 1, |at| {
            if at == 0 {
                focusables.len() - 1
            } else {
                at - 1
            }
        })
    } else {
        index.map_or(0, |at| (at + 1) % focusables.len())
    };
    focus_node(&ctx, focusables[next])
}

// https://drafts.csswg.org/selectors/#the-active-pseudo
fn set_active<'js>(ctx: Ctx<'js>, element: Value<'js>, active: bool) -> Result<()> {
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    let world_rc = world_for_node(&ctx, node)?;
    let world = world_rc.borrow_mut();
    let Some(mut parsed) = world.document_mut(node) else {
        return Ok(());
    };
    let Some(tree) = parsed.document.base.get_node_mut(node.node) else {
        return Ok(());
    };
    if active {
        tree.active();
    } else {
        tree.unactive();
    }
    Ok(())
}

// https://html.spec.whatwg.org/multipage/interaction.html#dom-useractivation-isactive
fn user_activation_active(ctx: Ctx<'_>) -> Result<bool> {
    Ok(super::world(&ctx)?.borrow().user_activation_active())
}

// https://html.spec.whatwg.org/multipage/interaction.html#dom-useractivation-hasbeenactive
fn user_activation_sticky(ctx: Ctx<'_>) -> Result<bool> {
    Ok(super::world(&ctx)?.borrow().user_activation_sticky())
}

// https://html.spec.whatwg.org/multipage/interaction.html#consume-user-activation
fn consume_user_activation(ctx: Ctx<'_>) -> Result<bool> {
    Ok(super::world(&ctx)?.borrow_mut().consume_user_activation())
}

// https://html.spec.whatwg.org/multipage/interaction.html#activation-behavior
fn activate_element<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<()> {
    let Some(node) = host_node_id(&ctx, &element) else {
        return Ok(());
    };
    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#concept-fe-disabled
    {
        let world = world_for_node(&ctx, node)?;
        let world = world.borrow();
        if let Some(parsed) = world.document(node)
            && is_actually_disabled(&parsed.document.base, node.node)
        {
            return Ok(());
        }
    }
    run_activation(&ctx, node, node)
}

// https://html.spec.whatwg.org/multipage/interaction.html#input-events
fn set_native_value<'a>(ctx: Ctx<'a>, element: Value<'a>, value: LegacyNullString) -> Result<()> {
    let Some(node) = host_node_id(&ctx, &element) else {
        return Err(Exception::throw_type(&ctx, "argument is not an element"));
    };
    let world = world_for_node(&ctx, node)?;
    let world = world.borrow();
    let Some(mut parsed) = world.document_mut(node) else {
        return Err(Exception::throw_type(&ctx, "no document"));
    };
    if parsed
        .document
        .base
        .get_node(node.node)
        .is_none_or(|tree| tree.data.downcast_element().is_none())
    {
        return Err(Exception::throw_type(&ctx, "argument is not an element"));
    }
    let local = parsed
        .document
        .base
        .get_node(node.node)
        .and_then(|tree| tree.data.downcast_element())
        .filter(|element| element.name.ns == crate::dom::tree::html_namespace())
        .map(|element| element.name.local.to_string());
    match local.as_deref() {
        Some("input") => {
            let Some(kind) = crate::dom::input::input_type(&parsed.document.base, node.node) else {
                return Ok(());
            };
            if kind.value_mode() == crate::dom::input::ValueMode::Current {
                let sanitized = crate::dom::input::sanitize_with_device(
                    &mut parsed.document.base,
                    node.node,
                    kind,
                    &value.0,
                );
                parsed
                    .document
                    .base
                    .mutate()
                    .set_value(node.node, sanitized, Dirtiness::Set);
            } else {
                write_attr(&mut parsed, node, "value", Some(&value.0));
            }
        }
        Some("textarea") => {
            parsed
                .document
                .base
                .mutate()
                .set_value(node.node, value.0, Dirtiness::Set);
        }
        _ => write_attr(&mut parsed, node, "value", Some(&value.0)),
    }
    Ok(())
}

fn webdriver_click<'js>(ctx: Ctx<'js>, element: Value<'js>) -> Result<()> {
    let Some(node) = host_node_id(&ctx, &element) else {
        return Err(Exception::throw_type(&ctx, "not an element"));
    };
    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#concept-fe-disabled
    {
        let world = world_for_node(&ctx, node)?;
        let world = world.borrow();
        if let Some(parsed) = world.document(node)
            && is_actually_disabled(&parsed.document.base, node.node)
        {
            return Ok(());
        }
    }
    // https://html.spec.whatwg.org/multipage/interaction.html#activation-notification
    world_for_node(&ctx, node)?.borrow_mut().activate_user();
    if is_focusable(&ctx, node)? {
        focus_node(&ctx, node)?;
    }
    events::fire_trusted_click(&ctx, EventTargetKey::Node(node))?;
    Ok(())
}
