//! Form controls: forms, inputs, text areas, selects, options, buttons,
//! and their value/selection state.

use super::super::{
    CollectionKind, LegacyNullString, WebIdlCodeUnits, WebIdlString, WebIdlUnsignedLong,
    document_base_url_string, handler_target, host, live_collection, schedule_mutation_delivery,
    set_attribute_sync, throw_dom, with_node_data, world, world_for_node, wrap_node,
};

use blitz_dom::Dirtiness;
use rquickjs::{Ctx, Exception, Result, Value};

use crate::dom::insertion::replace_all_journaled;
use crate::dom::tree::{NodeId, attr, form_owner_of, html_namespace};
use std::str::FromStr;

use super::JsNode;
use super::element::{content_attr, reflect_ulong};
use super::media::is_image_input;

include!(concat!(env!("OUT_DIR"), "/HTMLFormElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLInputElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLTextAreaElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLSelectElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLOptionElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLButtonElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLFieldSetElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLOptGroupElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLOutputElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLDataListElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLLabelElement.rs"));
include!(concat!(env!("OUT_DIR"), "/HTMLLegendElement.rs"));

impl<'js> html_form_element_generated::HTMLFormElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/forms.html#dom-form-action
    fn get_action(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        Ok(self.action(ctx.clone())?.into())
    }

    // https://html.spec.whatwg.org/multipage/forms.html#dom-form-reset
    fn reset(&self, ctx: Ctx<'js>) -> Result<()> {
        self.reset_form(ctx)
    }
}

impl<'js> html_input_element_generated::HTMLInputElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/input.html#dom-input-checked
    fn get_checked(&self, ctx: &Ctx<'js>) -> Result<bool> {
        self.checked(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-checked
    fn set_checked(&self, ctx: &Ctx<'js>, value: bool) -> Result<()> {
        self.set_checked(ctx.clone(), value)
    }

    /// <https://html.spec.whatwg.org/multipage/forms.html#dom-fae-form>
    fn get_form(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.form(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formaction
    fn get_form_action(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        Ok(self.form_action(ctx.clone())?.into())
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formmethod
    fn get_form_method(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let method = self.form_method(ctx.clone());
        rquickjs::String::from_str(ctx.clone(), method)
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formmethod
    fn set_form_method(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_form_method(ctx.clone(), WebIdlString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formenctype
    fn get_form_enctype(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let enctype = self.form_enctype(ctx.clone());
        rquickjs::String::from_str(ctx.clone(), enctype)
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formenctype
    fn set_form_enctype(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_form_enctype(ctx.clone(), WebIdlString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-indeterminate
    fn get_indeterminate(&self, ctx: &Ctx<'js>) -> Result<bool> {
        self.indeterminate_flag(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-indeterminate
    fn set_indeterminate(&self, ctx: &Ctx<'js>, value: bool) -> Result<()> {
        self.set_indeterminate_flag(ctx.clone(), value)
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-value
    fn get_value(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = self.value(ctx.clone())?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-value
    fn set_value(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_value(ctx.clone(), LegacyNullString(value.to_string()?))
    }

    // `size` defaults to 20; zero or unparseable reads 20, setting zero
    // throws
    // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-size>).
    fn get_size(&self, ctx: &Ctx<'js>) -> Result<usize> {
        let value = reflect_ulong(
            content_attr(ctx, self.handle.get().0, "size").as_deref(),
            20,
        );
        Ok(usize::try_from(if value == 0 { 20 } else { value }).unwrap_or(usize::MAX))
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-size
    fn set_size(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        if value == 0 {
            return Err(throw_dom(ctx, "IndexSizeError", "input size is zero"));
        }
        // Out-of-range values write the default rather than the raw value.
        host::reflect_set_ulong_defaulting(ctx, self.handle.get().0, "size", value, 20)
    }

    // Image-button dimensions default to zero, and read zero outside the
    // image state; the setters are generated from `[ReflectSetter]`
    // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-width>).
    fn get_width(&self, ctx: &Ctx<'js>) -> Result<u32> {
        if !is_image_input(ctx, self.handle.get().0) {
            return Ok(0);
        }
        Ok(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "width").as_deref(),
            0,
        ))
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-height
    fn get_height(&self, ctx: &Ctx<'js>) -> Result<u32> {
        if !is_image_input(ctx, self.handle.get().0) {
            return Ok(0);
        }
        Ok(reflect_ulong(
            content_attr(ctx, self.handle.get().0, "height").as_deref(),
            0,
        ))
    }

    // `autocomplete` reflects with a generated setter; the getter answers
    // the content attribute
    // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#attr-fe-autocomplete>).
    fn get_autocomplete(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        host::reflect_string(ctx, self.handle.get().0, "autocomplete")
    }

    // Upstream selection accessors are nullable, keeping the spec-prose
    // null for input types where selection does not apply. No selection state
    // is stored (a known cutover gap), so applicable inputs answer zero.
    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#textFieldSelection

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionstart
    fn get_selection_start(&self, ctx: &Ctx<'js>) -> Result<Option<u32>> {
        let world = world(ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(None);
        };
        if !crate::dom::forms::selection_applies(&parsed.document.base, self.handle.get().0.node) {
            return Ok(None);
        }
        drop(parsed);
        Ok(Some(selection_start_of(ctx, self.handle.get().0)?))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionstart
    fn set_selection_start(&self, ctx: &Ctx<'js>, value: Option<u32>) -> Result<()> {
        // Null converts as zero, matching the previous numeric conversion.
        self.set_selection_start(ctx.clone(), WebIdlUnsignedLong(value.unwrap_or(0)))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionend
    fn get_selection_end(&self, ctx: &Ctx<'js>) -> Result<Option<u32>> {
        let world = world(ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(None);
        };
        if !crate::dom::forms::selection_applies(&parsed.document.base, self.handle.get().0.node) {
            return Ok(None);
        }
        drop(parsed);
        Ok(Some(selection_end_of(ctx, self.handle.get().0)?))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionend
    fn set_selection_end(&self, ctx: &Ctx<'js>, value: Option<u32>) -> Result<()> {
        // Null converts as zero, matching the previous numeric conversion.
        self.set_selection_end(ctx.clone(), WebIdlUnsignedLong(value.unwrap_or(0)))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectiondirection
    fn get_selection_direction(&self, ctx: &Ctx<'js>) -> Result<Option<rquickjs::String<'js>>> {
        let world = world(ctx)?;
        let parsed = world.borrow();
        let Some(parsed) = parsed.document(self.handle.get().0) else {
            return Ok(None);
        };
        if !crate::dom::forms::selection_applies(&parsed.document.base, self.handle.get().0.node) {
            return Ok(None);
        }
        drop(parsed);
        let direction = selection_direction_of(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), direction_name(direction)).map(Some)
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectiondirection
    fn set_selection_direction(
        &self,
        ctx: &Ctx<'js>,
        value: Option<rquickjs::String<'js>>,
    ) -> Result<()> {
        // Null stringified to "null" before, which maps to no direction;
        // answer "none" directly.
        let value = match value {
            Some(value) => value.to_string()?,
            None => "none".into(),
        };
        self.set_selection_direction(ctx.clone(), WebIdlString(value))
    }
}

impl<'js> html_text_area_element_generated::HTMLTextAreaElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-value
    fn get_value(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = self.value(ctx.clone())?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-value
    fn set_value(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_value(ctx.clone(), LegacyNullString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-defaultvalue
    fn get_default_value(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = self.default_value(ctx.clone())?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-defaultvalue
    fn set_default_value(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_default_value(ctx.clone(), WebIdlString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-textlength
    fn get_text_length(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(self.text_length(ctx.clone())? as usize)
    }

    // Selection getters are non-nullable upstream, answering the stored
    // range clamped to the control value.
    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#textFieldSelection

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionstart
    fn get_selection_start(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(selection_start_of(ctx, self.handle.get().0)? as usize)
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionstart
    fn set_selection_start(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        self.set_selection_start(ctx.clone(), WebIdlUnsignedLong(value))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionend
    fn get_selection_end(&self, ctx: &Ctx<'js>) -> Result<usize> {
        Ok(selection_end_of(ctx, self.handle.get().0)? as usize)
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionend
    fn set_selection_end(&self, ctx: &Ctx<'js>, value: u32) -> Result<()> {
        self.set_selection_end(ctx.clone(), WebIdlUnsignedLong(value))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectiondirection
    fn get_selection_direction(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let direction = selection_direction_of(ctx, self.handle.get().0)?;
        rquickjs::String::from_str(ctx.clone(), direction_name(direction))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectiondirection
    fn set_selection_direction(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_selection_direction(ctx.clone(), WebIdlString(value.to_string()?))
    }

    /// <https://html.spec.whatwg.org/multipage/forms.html#dom-fae-form>
    fn get_form(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.form(ctx.clone())
    }

    // `autocomplete` reflects with a generated setter; the getter answers
    // the content attribute
    // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#attr-fe-autocomplete>).
    fn get_autocomplete(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        host::reflect_string(ctx, self.handle.get().0, "autocomplete")
    }
}

impl<'js> html_select_element_generated::HTMLSelectElement<'js> for JsNode {
    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-value
    fn get_value(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = self.value(ctx.clone())?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-value
    fn set_value(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_value(ctx.clone(), LegacyNullString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-selectedindex
    fn get_selected_index(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.selected_index(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-selectedindex
    fn set_selected_index(&self, ctx: &Ctx<'js>, value: i32) -> Result<()> {
        self.set_selected_index(ctx.clone(), value)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-options
    fn get_options(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.options(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-selectedoptions
    fn get_selected_options(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.selected_options(ctx.clone())
    }

    /// <https://html.spec.whatwg.org/multipage/forms.html#dom-fae-form>
    fn get_form(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.form(ctx.clone())
    }

    // `autocomplete` reflects with a generated setter; the getter answers
    // the content attribute
    // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#attr-fe-autocomplete>).
    fn get_autocomplete(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        host::reflect_string(ctx, self.handle.get().0, "autocomplete")
    }
}

impl<'js> html_option_element_generated::HTMLOptionElement<'js> for JsNode {
    /// <https://html.spec.whatwg.org/multipage/forms.html#dom-fae-form>
    fn get_form(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.form(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-label
    fn get_label(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let label = self.label(ctx.clone())?;
        rquickjs::String::from_str(ctx.clone(), &label)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-selected
    fn get_selected(&self, ctx: &Ctx<'js>) -> Result<bool> {
        self.selected(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-selected
    fn set_selected(&self, ctx: &Ctx<'js>, value: bool) -> Result<()> {
        self.set_selected(ctx.clone(), value)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-value
    fn get_value(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let value = self.value(ctx.clone())?;
        rquickjs::String::from_str(ctx.clone(), &value)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-text
    fn get_text(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let text = self.text(ctx.clone())?;
        rquickjs::String::from_str(ctx.clone(), &text)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-text
    fn set_text(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        // DOMString keeps every code unit, including lone surrogates.
        self.set_text(
            ctx.clone(),
            WebIdlCodeUnits(crate::dom_string::DomString::from_utf16(value.to_utf16()?)),
        )
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-index
    fn get_index(&self, ctx: &Ctx<'js>) -> Result<i32> {
        self.index(ctx.clone())
    }
}

impl<'js> html_button_element_generated::HTMLButtonElement<'js> for JsNode {
    /// <https://html.spec.whatwg.org/multipage/forms.html#dom-fae-form>
    fn get_form(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.form(ctx.clone())
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formaction
    fn get_form_action(&self, ctx: &Ctx<'js>) -> Result<crate::dom_string::DomString> {
        Ok(self.form_action(ctx.clone())?.into())
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formmethod
    fn get_form_method(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let method = self.form_method(ctx.clone());
        rquickjs::String::from_str(ctx.clone(), method)
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formmethod
    fn set_form_method(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_form_method(ctx.clone(), WebIdlString(value.to_string()?))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formenctype
    fn get_form_enctype(&self, ctx: &Ctx<'js>) -> Result<rquickjs::String<'js>> {
        let enctype = self.form_enctype(ctx.clone());
        rquickjs::String::from_str(ctx.clone(), enctype)
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formenctype
    fn set_form_enctype(&self, ctx: &Ctx<'js>, value: rquickjs::String<'js>) -> Result<()> {
        self.set_form_enctype(ctx.clone(), WebIdlString(value.to_string()?))
    }
}

impl<'js> html_field_set_element_generated::HTMLFieldSetElement<'js> for JsNode {
    /// <https://html.spec.whatwg.org/multipage/forms.html#dom-fae-form>
    fn get_form(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.form(ctx.clone())
    }
}

impl<'js> html_output_element_generated::HTMLOutputElement<'js> for JsNode {
    /// <https://html.spec.whatwg.org/multipage/forms.html#dom-fae-form>
    fn get_form(&self, ctx: &Ctx<'js>) -> Result<Value<'js>> {
        self.form(ctx.clone())
    }
}

impl JsNode {
    // https://html.spec.whatwg.org/multipage/input.html#dom-input-value
    //
    // The current value, default value, and selectedness live in Blitz's
    // control slots; this dispatches by element to the one the IDL attribute
    // returns.
    pub(super) fn value(&self, ctx: Ctx<'_>) -> Result<String> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let local = world.document(self.handle.get().0).and_then(|parsed| {
            parsed
                .document
                .base
                .get_node(self.handle.get().0.node)
                .and_then(|node| node.data.downcast_element())
                .filter(|element| element.name.ns == html_namespace())
                .map(|element| element.name.local.to_string())
        });
        let Some(local) = local else {
            return Ok(String::new());
        };
        if local == "select" {
            let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                return Err(Exception::throw_type(&ctx, "no document"));
            };
            return Ok(crate::dom::forms::select_value(
                &mut parsed,
                self.handle.get().0.node,
            ));
        }
        let Some(parsed) = world.document(self.handle.get().0) else {
            return Err(Exception::throw_type(&ctx, "no document"));
        };
        let base = &parsed.document.base;
        Ok(match local.as_str() {
            "textarea" => crate::dom::forms::textarea_api_value(base, self.handle.get().0.node),
            "option" => crate::dom::forms::option_value(base, self.handle.get().0.node),
            "input" => crate::dom::input::current_value(&parsed, self.handle.get().0.node),
            // Every other element reflects its `value` content attribute
            // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-button-value>).
            _ => attr(base, self.handle.get().0.node, "value")
                .unwrap_or_default()
                .to_owned(),
        })
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-value
    pub(super) fn set_value(&self, ctx: Ctx<'_>, value: LegacyNullString) -> Result<()> {
        let local = with_node_data(&ctx, self.handle.get().0, |data| {
            data.and_then(|data| data.downcast_element())
                .filter(|element| element.name.ns == html_namespace())
                .map(|element| element.name.local.to_string())
        })?;
        let Some(local) = local else {
            return Ok(());
        };
        let result = match local.as_str() {
            "textarea" => self.set_textarea_value(&ctx, value.0),
            "select" => self.set_select_value(&ctx, value.0),
            "input" => {
                let world_rc = world(&ctx)?;
                let (kind, sanitized, previous) = {
                    let world = world_rc.borrow();
                    let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                        return Ok(());
                    };
                    let base = &mut parsed.document.base;
                    let Some(kind) = crate::dom::input::input_type(base, self.handle.get().0.node)
                    else {
                        return Ok(());
                    };
                    if kind.value_mode() == crate::dom::input::ValueMode::Current {
                        let sanitized = crate::dom::input::sanitize_with_device(
                            base,
                            self.handle.get().0.node,
                            kind,
                            &value.0,
                        );
                        let previous =
                            crate::dom::input::current_value(&parsed, self.handle.get().0.node);
                        (kind, Some(sanitized), Some(previous))
                    } else {
                        (kind, None, None)
                    }
                };
                match kind.value_mode() {
                    crate::dom::input::ValueMode::Current => self.set_dirty_control_value(
                        &ctx,
                        sanitized.unwrap_or_default(),
                        previous.unwrap_or_default(),
                    ),
                    crate::dom::input::ValueMode::Default
                    | crate::dom::input::ValueMode::DefaultOn => {
                        set_attribute_sync(&ctx, self.handle.get().0, "value", &value.0)
                    }
                    crate::dom::input::ValueMode::Filename => {
                        if !value.0.is_empty() {
                            return Err(throw_dom(
                                &ctx,
                                "InvalidStateError",
                                "a file input's value can only be set to the empty string",
                            ));
                        }
                        world_rc
                            .borrow_mut()
                            .set_input_files(self.handle.get().0, Vec::new());
                        Ok(())
                    }
                }
            }
            _ => set_attribute_sync(&ctx, self.handle.get().0, "value", &value.0),
        };
        // A script assignment is not a user edit, and it ends any in-progress
        // edit buffer or bad-input state. Only a successful assignment gets
        // here: a rejected one leaves the control as it was
        // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#suffering-from-being-too-long>).
        if result.is_ok()
            && let Ok(world) = world(&ctx)
        {
            world
                .borrow_mut()
                .clear_control_edit_state(self.handle.get().0);
        }
        result
    }

    /// Sets a textarea's dirty value. The children stay the default value
    /// (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-value>).
    pub(super) fn set_textarea_value(&self, ctx: &Ctx<'_>, value: String) -> Result<()> {
        // The API value normalizes CRLF and CR to LF before the dirty-value
        // comparison, so setting the same normalized value leaves the
        // selection alone
        // (<https://html.spec.whatwg.org/multipage/form-elements.html#concept-textarea-api-value>).
        let value = crate::dom::text::normalize_newlines(&value);
        let previous = self.value(ctx.clone())?;
        self.set_dirty_control_value(ctx, value, previous)
    }

    /// Stores a text control's dirty value and collapses the caret to the end
    /// when the value actually changes
    /// (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#set-the-selection-range>).
    fn set_dirty_control_value(
        &self,
        ctx: &Ctx<'_>,
        value: String,
        previous: String,
    ) -> Result<()> {
        let changed = previous != value;
        let collapsed = u32::try_from(value.encode_utf16().count()).unwrap_or(u32::MAX);
        let world_rc = world(ctx)?;
        let applies = {
            let world = world_rc.borrow();
            world.document(self.handle.get().0).is_some_and(|parsed| {
                crate::dom::forms::selection_applies(
                    &parsed.document.base,
                    self.handle.get().0.node,
                )
            })
        };
        {
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                return Ok(());
            };
            parsed.document.base.mutate().set_value(
                self.handle.get().0.node,
                value,
                Dirtiness::Set,
            );
        }
        if changed && applies {
            world_rc
                .borrow_mut()
                .set_selection(self.handle.get().0, collapsed, collapsed, 0);
        }
        Ok(())
    }

    /// Replaces a textarea's children. `defaultValue` uses this.
    pub(super) fn set_textarea_default_value(&self, ctx: &Ctx<'_>, value: String) -> Result<()> {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        let added = if value.is_empty() {
            Vec::new()
        } else {
            let text = parsed.document.base.mutate().create_text_node(&value);
            vec![NodeId {
                document: parsed.id,
                node: text,
            }]
        };
        let iframes = replace_all_journaled(&mut parsed, self.handle.get().0, added);
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        schedule_mutation_delivery(ctx)
    }

    /// Selects the first option whose value is `value`, deselecting the rest;
    /// with no match every option is deselected.
    pub(super) fn set_select_value(&self, ctx: &Ctx<'_>, value: String) -> Result<()> {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Err(Exception::throw_type(ctx, "no document"));
        };
        crate::dom::forms::set_select_value_selectedness(
            &mut parsed,
            self.handle.get().0.node,
            &value,
        );
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-defaultvalue
    pub(super) fn default_value(&self, ctx: Ctx<'_>) -> Result<String> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(parsed) = world.document(self.handle.get().0) else {
            return Err(Exception::throw_type(&ctx, "no document"));
        };
        // The default value is the element's child text content
        // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-defaultvalue>).
        Ok(crate::dom::text::child_text_content(
            &parsed.document.base,
            self.handle.get().0.node,
        ))
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-defaultvalue
    pub(super) fn set_default_value(&self, ctx: Ctx<'_>, value: WebIdlString) -> Result<()> {
        self.set_textarea_default_value(&ctx, value.0)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-textarea-textlength
    pub(super) fn text_length(&self, ctx: Ctx<'_>) -> Result<u32> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(parsed) = world.document(self.handle.get().0) else {
            return Ok(0);
        };
        // The control's value (dirty value when set, otherwise the attribute
        // or descendant text), in UTF-16 code units
        // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-textlength>).
        Ok(u32::try_from(crate::dom::forms::control_value_len(
            &parsed,
            self.handle.get().0.node,
        ))
        .unwrap_or(u32::MAX))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionstart
    pub(super) fn set_selection_start(
        &self,
        ctx: Ctx<'_>,
        value: WebIdlUnsignedLong,
    ) -> Result<()> {
        let id = self.handle.get().0;
        self.require_selection(&ctx, "selectionStart")?;
        let (_, end, direction) = world(&ctx)?.borrow().selection_of(id);
        // `end` is raised to the given start before the range is set
        // (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionstart>).
        set_selection_range(&ctx, id, value.0, end.max(value.0), direction)
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionend
    pub(super) fn set_selection_end(&self, ctx: Ctx<'_>, value: WebIdlUnsignedLong) -> Result<()> {
        let id = self.handle.get().0;
        self.require_selection(&ctx, "selectionEnd")?;
        let (start, _, direction) = world(&ctx)?.borrow().selection_of(id);
        set_selection_range(&ctx, id, start, value.0, direction)
    }

    pub(super) fn set_selection_direction(&self, ctx: Ctx<'_>, value: WebIdlString) -> Result<()> {
        let id = self.handle.get().0;
        self.require_selection(&ctx, "selectionDirection")?;
        let (start, end, _) = world(&ctx)?.borrow().selection_of(id);
        set_selection_range(&ctx, id, start, end, direction_code(&value.0))
    }

    /// Rejects a selection write on a control the text selection APIs do not
    /// apply to
    /// (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-textarea/input-selectionstart>).
    fn require_selection(&self, ctx: &Ctx<'_>, name: &str) -> Result<()> {
        let world = world(ctx)?;
        let world = world.borrow();
        let Some(parsed) = world.document(self.handle.get().0) else {
            return Ok(());
        };
        if crate::dom::forms::selection_applies(&parsed.document.base, self.handle.get().0.node) {
            return Ok(());
        }
        Err(throw_dom(
            ctx,
            "InvalidStateError",
            &format!("{name} does not apply to this control"),
        ))
    }

    // https://html.spec.whatwg.org/multipage/forms.html#dom-form-reset
    pub(super) fn reset_form(&self, ctx: Ctx<'_>) -> Result<()> {
        let owner = world_for_node(&ctx, self.handle.get().0)?;
        let is_form = owner
            .borrow()
            .document(self.handle.get().0)
            .is_some_and(|parsed| {
                crate::dom::tree::is_html_element(
                    &parsed.document.base,
                    self.handle.get().0.node,
                    "form",
                )
            });
        if !is_form {
            return Err(throw_dom(
                &ctx,
                "InvalidStateError",
                "reset is only available on a form",
            ));
        }
        // The reset event is trusted and cancelable; a canceled event leaves
        // every control untouched
        // (<https://html.spec.whatwg.org/multipage/forms.html#concept-form-reset>).
        if !crate::js::events::fire_trusted_cancelable(
            &ctx,
            crate::js::world::EventTargetKey::Node(self.handle.get().0),
            "reset",
            true,
            true,
        )? {
            return Ok(());
        }
        let world_rc = world(&ctx)?;
        let (document, controls, outputs) = {
            let world = world_rc.borrow();
            let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
                return Ok(());
            };
            let controls =
                form_controls(&parsed.document.base, parsed.id, self.handle.get().0.node);
            {
                let mut mutator = parsed.document.base.mutate();
                for control in &controls {
                    mutator.reset_control(*control);
                }
            }
            let mut outputs = Vec::new();
            for control in &controls {
                if crate::dom::tree::is_html_element(&parsed.document.base, *control, "input") {
                    crate::dom::input::sync_sanitized_value(&mut parsed.document, *control);
                } else if crate::dom::tree::is_html_element(
                    &parsed.document.base,
                    *control,
                    "select",
                ) {
                    crate::dom::forms::reset_select_selectedness(&mut parsed, *control);
                } else if crate::dom::tree::is_html_element(
                    &parsed.document.base,
                    *control,
                    "output",
                ) {
                    // The reset algorithm replaces all of the output's
                    // children with its default value as text, which also
                    // drops any markup. A value-mode output restores its
                    // frozen default; a default-mode one keeps the text it
                    // has
                    // (<https://html.spec.whatwg.org/multipage/form-elements.html#the-output-element>).
                    let id = NodeId {
                        document: parsed.id,
                        node: *control,
                    };
                    let frozen = world
                        .control_state(id)
                        .filter(|state| state.output_value_mode)
                        .and_then(|state| state.output_default_value.clone());
                    let text = frozen.map_or_else(
                        || parsed.document.descendant_dom_text(*control),
                        |units| crate::dom_string::DomString::from(units.as_slice()),
                    );
                    outputs.push((*control, text));
                }
            }
            (parsed.id, controls, outputs)
        };
        // The reset algorithm restores each control's default value and clears
        // its dirty and edit state
        // (<https://html.spec.whatwg.org/multipage/input.html#the-input-element>).
        {
            let mut world = world_rc.borrow_mut();
            for control in controls {
                let id = NodeId {
                    document,
                    node: control,
                };
                world.clear_control_edit_state(id);
                if outputs.iter().any(|(output, _)| *output == control) {
                    // The output's value-mode flag is clear and its override
                    // is null, so its default value is now its text
                    // (<https://html.spec.whatwg.org/multipage/form-elements.html#the-output-element>).
                    let state = world.control(id);
                    state.output_value_mode = false;
                    state.output_default_value = None;
                }
            }
        }
        if outputs.is_empty() {
            return Ok(());
        }
        // The output's reset algorithm replaces its text with the restored
        // default
        // (<https://html.spec.whatwg.org/multipage/form-elements.html#the-output-element>).
        restore_outputs(&ctx, &world_rc, document, outputs)
    }

    // https://html.spec.whatwg.org/multipage/forms.html#dom-form-action
    pub(super) fn action(&self, ctx: Ctx<'_>) -> Result<String> {
        let world_rc = world(&ctx)?;
        let raw = world_rc
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| {
                attr(&parsed.document.base, self.handle.get().0.node, "action")
                    .map(ToOwned::to_owned)
            });
        let base = document_base_url_string(&ctx, self.handle.get().0);
        let Some(raw) = raw else {
            return Ok(base);
        };
        Ok(url::Url::parse(&base)
            .ok()
            .and_then(|base| base.join(&raw).ok())
            .map_or(raw, |url| url.to_string()))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formaction
    pub(super) fn form_action(&self, ctx: Ctx<'_>) -> Result<String> {
        let world = world(&ctx)?;
        let raw = world
            .borrow()
            .document(self.handle.get().0)
            .and_then(|parsed| {
                attr(
                    &parsed.document.base,
                    self.handle.get().0.node,
                    "formaction",
                )
                .map(ToOwned::to_owned)
            });
        let base = document_base_url_string(&ctx, self.handle.get().0);
        let Some(raw) = raw else {
            return Ok(base);
        };
        Ok(url::Url::parse(&base)
            .ok()
            .and_then(|base| base.join(&raw).ok())
            .map_or(raw, |url| url.to_string()))
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formmethod
    pub(super) fn form_method(&self, ctx: Ctx<'_>) -> &'static str {
        // Enumerated without whitespace stripping: only an exact ASCII
        // case-insensitive keyword matches
        // (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#limited-to-only-known-values>).
        // The form-submission plan and this getter share the engine's parser,
        // so they cannot disagree about a value such as `" post"`.
        match content_attr(&ctx, self.handle.get().0, "formmethod") {
            None => "",
            Some(raw) => blitz_dom::form::FormMethod::from_str(&raw)
                .unwrap_or(blitz_dom::form::FormMethod::Get)
                .as_str(),
        }
    }

    pub(super) fn set_form_method(&self, ctx: Ctx<'_>, value: WebIdlString) -> Result<()> {
        self.set_attribute(ctx, WebIdlString("formmethod".into()), value)
    }

    // https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#dom-fs-formenctype
    pub(super) fn form_enctype(&self, ctx: Ctx<'_>) -> &'static str {
        // Absent reads empty, like `formMethod`; no whitespace stripping
        // (<https://html.spec.whatwg.org/multipage/common-dom-interfaces.html#limited-to-only-known-values>).
        match content_attr(&ctx, self.handle.get().0, "formenctype") {
            None => "",
            Some(raw) => blitz_dom::form::RequestContentType::from_str(&raw)
                .unwrap_or(blitz_dom::form::RequestContentType::FormUrlEncoded)
                .as_str(),
        }
    }

    pub(super) fn set_form_enctype(&self, ctx: Ctx<'_>, value: WebIdlString) -> Result<()> {
        self.set_attribute(ctx, WebIdlString("formenctype".into()), value)
    }

    /// <https://html.spec.whatwg.org/multipage/forms.html#dom-fae-form>
    pub(super) fn form<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        let form = {
            let world = world(&ctx)?;
            let world = world.borrow();
            world.document(self.handle.get().0).and_then(|parsed| {
                form_owner_of(
                    &parsed.document.base,
                    self.handle.get().0.document,
                    self.handle.get().0.node,
                )
            })
        };
        match form {
            Some(form) => wrap_node(&ctx, form),
            None => Ok(Value::new_null(ctx)),
        }
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-checked
    pub(super) fn checked(&self, ctx: Ctx<'_>) -> Result<bool> {
        let world = world(&ctx)?;
        let world = world.borrow();
        Ok(world.document(self.handle.get().0).is_some_and(|parsed| {
            parsed
                .document
                .base
                .get_node(self.handle.get().0.node)
                .and_then(|node| node.data.downcast_element())
                .is_some_and(blitz_dom::ElementData::checkedness)
        }))
    }

    pub(super) fn set_checked(&self, ctx: Ctx<'_>, value: bool) -> Result<()> {
        // Setting a radio's checkedness unchecks its group. The content
        // attribute is the default and is left alone
        // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-checked>).
        let group = if value {
            let world = world(&ctx)?;
            let world = world.borrow();
            let Some(parsed) = world.document(self.handle.get().0) else {
                return Ok(());
            };
            let base = &parsed.document.base;
            let is_radio = base
                .get_node(self.handle.get().0.node)
                .and_then(|node| node.data.downcast_element())
                .is_some_and(|element| {
                    element.name.ns == html_namespace()
                        && element.name.local.as_str() == "input"
                        && attr(base, self.handle.get().0.node, "type")
                            .unwrap_or("text")
                            .trim()
                            .eq_ignore_ascii_case("radio")
                });
            if is_radio {
                let name = attr(base, self.handle.get().0.node, "name")
                    .unwrap_or_default()
                    .to_owned();
                crate::dom::forms::radio_group_members(
                    base,
                    self.handle.get().0.document,
                    &name,
                    self.handle.get().0.node,
                )
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        let mut mutator = parsed.document.base.mutate();
        for member in group {
            mutator.set_checkedness(member.node, false, Dirtiness::Keep);
        }
        // The checked IDL attribute also sets the dirty checkedness flag
        // (<https://html.spec.whatwg.org/multipage/input.html#dom-input-checked>).
        mutator.set_checkedness(self.handle.get().0.node, value, Dirtiness::Set);
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/input.html#dom-input-indeterminate
    pub(super) fn indeterminate_flag(&self, ctx: Ctx<'_>) -> Result<bool> {
        let world = world(&ctx)?;
        let world = world.borrow();
        Ok(world.document(self.handle.get().0).is_some_and(|parsed| {
            parsed
                .document
                .base
                .get_node(self.handle.get().0.node)
                .and_then(|node| node.data.downcast_element())
                .is_some_and(|element| element.form_state.indeterminate())
        }))
    }

    pub(super) fn set_indeterminate_flag(&self, ctx: Ctx<'_>, value: bool) -> Result<()> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        parsed
            .document
            .base
            .mutate()
            .set_indeterminate(self.handle.get().0.node, value);
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-selected
    pub(super) fn selected(&self, ctx: Ctx<'_>) -> Result<bool> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(false);
        };
        Ok(crate::dom::forms::option_effective_selected(
            &mut parsed,
            self.handle.get().0.node,
        ))
    }

    pub(super) fn set_selected(&self, ctx: Ctx<'_>, value: bool) -> Result<()> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        crate::dom::forms::set_option_selected(&mut parsed, self.handle.get().0.node, value);
        Ok(())
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-label
    pub(super) fn label(&self, ctx: Ctx<'_>) -> Result<String> {
        let world = world(&ctx)?;
        let world = world.borrow();
        Ok(world
            .document(self.handle.get().0)
            .map_or_else(String::new, |parsed| {
                let base = &parsed.document.base;
                crate::dom::tree::attr_no_namespace(base, self.handle.get().0.node, "label")
                    .map_or_else(
                        || crate::dom::forms::option_text(base, self.handle.get().0.node),
                        str::to_owned,
                    )
            }))
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-text
    pub(super) fn text(&self, ctx: Ctx<'_>) -> Result<String> {
        let world = world(&ctx)?;
        let world = world.borrow();
        Ok(world
            .document(self.handle.get().0)
            .map_or_else(String::new, |parsed| {
                crate::dom::forms::option_text(&parsed.document.base, self.handle.get().0.node)
            }))
    }

    pub(super) fn set_text(&self, ctx: Ctx<'_>, value: WebIdlCodeUnits) -> Result<()> {
        // An option's text setter replaces its children with one text node
        // (<https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-text>).
        let text = value.0.to_string_lossy().into_owned();
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        let added = if text.is_empty() {
            Vec::new()
        } else {
            let text = parsed.document.base.mutate().create_text_node(&text);
            vec![NodeId {
                document: parsed.id,
                node: text,
            }]
        };
        let iframes = replace_all_journaled(&mut parsed, self.handle.get().0, added);
        drop(parsed);
        world.destroy_child_navigables(&iframes);
        drop(world);
        schedule_mutation_delivery(&ctx)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-option-index
    pub(super) fn index(&self, ctx: Ctx<'_>) -> Result<i32> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(parsed) = world.document(self.handle.get().0) else {
            return Ok(0);
        };
        let base = &parsed.document.base;
        let Some(select) = crate::dom::forms::option_select_owner(base, self.handle.get().0.node)
            .map(|node| NodeId {
                document: self.handle.get().0.document,
                node,
            })
        else {
            return Ok(0);
        };
        for (index, option) in
            crate::dom::forms::select_options(base, self.handle.get().0.document, select.node)
                .into_iter()
                .enumerate()
        {
            if option == self.handle.get().0 {
                return Ok(i32::try_from(index).unwrap_or(i32::MAX));
            }
        }
        Ok(0)
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-selectedindex
    pub(super) fn selected_index(&self, ctx: Ctx<'_>) -> Result<i32> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(-1);
        };
        Ok(crate::dom::forms::selected_index_of(
            &mut parsed,
            self.handle.get().0.node,
        ))
    }

    pub(super) fn set_selected_index(&self, ctx: Ctx<'_>, value: i32) -> Result<()> {
        let world = world(&ctx)?;
        let world = world.borrow();
        let Some(mut parsed) = world.document_mut(self.handle.get().0) else {
            return Ok(());
        };
        crate::dom::forms::set_select_selected_index_value(
            &mut parsed,
            self.handle.get().0.node,
            value,
        );
        Ok(())
    }

    pub(super) fn options<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        live_collection(
            &ctx,
            self.handle.get().0,
            CollectionKind::SelectOptions,
            Some("HTMLOptionsCollection"),
        )
    }

    // https://html.spec.whatwg.org/multipage/form-elements.html#dom-select-selectedoptions
    pub(super) fn selected_options<'js>(&self, ctx: Ctx<'js>) -> Result<Value<'js>> {
        live_collection(
            &ctx,
            self.handle.get().0,
            CollectionKind::SelectedOptions,
            Some("HTMLCollection"),
        )
    }
}
/// Runs the form-control insertion fixups for a batch of inserted nodes: the
/// option insertion steps once per affected select, then the radio group's
/// checkedness rule. Both snapshot the state the batch arrived with, because
/// an earlier inserted control clears a later one before its own step runs;
/// the last inserted checked control is the one left checked
/// (<https://html.spec.whatwg.org/multipage/form-elements.html#the-select-element>,
/// <https://html.spec.whatwg.org/multipage/input.html#radio-button-group>).
pub(super) fn fixup_controls_on_insert(
    ctx: &Ctx<'_>,
    document: u32,
    controls: &[NodeId],
) -> Result<()> {
    let world_rc = world(ctx)?;
    let world = world_rc.borrow();
    let Some(mut parsed) = world.document_mut_by_id(document) else {
        return Ok(());
    };
    let mut ordered = controls.to_vec();
    ordered.sort_by(|a, b| {
        crate::dom::tree::tree_order(&parsed.document.base, document, a.node, b.node)
    });
    crate::dom::forms::options_inserted(&mut parsed, ordered.iter().map(|id| id.node));
    let mut radios = Vec::new();
    for id in &ordered {
        let group = crate::dom::forms::radios_to_uncheck(&parsed.document.base, document, id.node);
        if !group.is_empty() {
            radios.push((id.node, group));
        }
    }
    for (id, group) in radios {
        let mut mutator = parsed.document.base.mutate();
        // An earlier radio's step may have cleared this one; the state it
        // arrived with is what its own step acts on.
        mutator.set_checkedness(id, true, Dirtiness::Keep);
        for member in group {
            mutator.set_checkedness(member.node, false, Dirtiness::Keep);
        }
    }
    Ok(())
}

/// A control's stored selection start clamped to its value length.
pub(super) fn selection_start_of(ctx: &rquickjs::Ctx<'_>, id: NodeId) -> rquickjs::Result<u32> {
    let world_rc = world(ctx)?;
    let world = world_rc.borrow();
    let parsed = world.document(id);
    let Some(parsed) = parsed else {
        return Ok(0);
    };
    let (start, _, _) = world.selection_of(id);
    Ok(start.min(
        u32::try_from(crate::dom::forms::control_value_len(&parsed, id.node)).unwrap_or(u32::MAX),
    ))
}

/// A control's stored selection end clamped to its value length.
pub(super) fn selection_end_of(ctx: &rquickjs::Ctx<'_>, id: NodeId) -> rquickjs::Result<u32> {
    let world_rc = world(ctx)?;
    let world = world_rc.borrow();
    let Some(parsed) = world.document(id) else {
        return Ok(0);
    };
    let (_, end, _) = world.selection_of(id);
    Ok(end.min(
        u32::try_from(crate::dom::forms::control_value_len(&parsed, id.node)).unwrap_or(u32::MAX),
    ))
}

/// A control's stored selection direction code.
pub(super) fn selection_direction_of(ctx: &rquickjs::Ctx<'_>, id: NodeId) -> rquickjs::Result<u8> {
    let world_rc = world(ctx)?;
    let world = world_rc.borrow();
    let Some(parsed) = world.document(id) else {
        return Ok(0);
    };
    if !crate::dom::forms::selection_applies(&parsed.document.base, id.node) {
        return Ok(0);
    }
    Ok(world.selection_of(id).2)
}

/// The `SelectionMode` direction name for the stored code
/// (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#selection-direction>).
/// "Set the selection range"
/// (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#set-the-selection-range>).
///
/// Both edges clamp to the relevant value's length, `end` at or before `start`
/// collapses to a caret at `end`, an unknown direction is "none", and the
/// control's `select` event is queued only when the extent or direction
/// actually changed. The event itself is an element task, not a synchronous
/// dispatch.
fn set_selection_range(
    ctx: &Ctx<'_>,
    node: NodeId,
    start: u32,
    end: u32,
    direction: u8,
) -> Result<()> {
    let world_rc = world(ctx)?;
    let length = {
        let world = world_rc.borrow();
        let Some(parsed) = world.document(node) else {
            return Ok(());
        };
        u32::try_from(crate::dom::forms::control_value_len(&parsed, node.node)).unwrap_or(u32::MAX)
    };
    let start = start.min(length);
    let end = end.min(length);
    let (start, end) = if end <= start {
        (end, end)
    } else {
        (start, end)
    };
    let previous = world_rc.borrow().selection_of(node);
    if previous == (start, end, direction) {
        return Ok(());
    }
    let mut world = world_rc.borrow_mut();
    world.set_selection(node, start, end, direction);
    world.queue_selection_changed(node);
    Ok(())
}

/// `__tbSetSelectionRange`: the one entry point for the selection APIs the
/// JavaScript shim owns (`select()`, `setSelectionRange()`, `setRangeText()`).
/// The stored selection and its `select` task belong to the host, so the shim
/// only converts the Web IDL arguments
/// (<https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#textFieldSelection>).
pub(crate) fn set_selection_range_host<'js>(
    ctx: Ctx<'js>,
    node: Value<'js>,
    start: u32,
    end: u32,
    direction: Option<String>,
) -> Result<()> {
    let id = handler_target(&ctx, &node)?;
    let applies = {
        let world = world(&ctx)?;
        let world = world.borrow();
        world.document(id).is_some_and(|parsed| {
            crate::dom::forms::selection_applies(&parsed.document.base, id.node)
        })
    };
    if !applies {
        return Err(throw_dom(
            &ctx,
            "InvalidStateError",
            "the text selection APIs do not apply to this control",
        ));
    }
    set_selection_range(
        &ctx,
        id,
        start,
        end,
        direction.as_deref().map_or(0, direction_code),
    )
}

pub(super) fn direction_name(direction: u8) -> &'static str {
    match direction {
        1 => "forward",
        2 => "backward",
        _ => "none",
    }
}

/// Maps a `selectionDirection` string to its stored code; anything but the two
/// known keywords is "none".
pub(super) fn direction_code(direction: &str) -> u8 {
    match direction {
        "forward" => 1,
        "backward" => 2,
        _ => 0,
    }
}

/// Replaces each output's text with the default the reset algorithm restored
/// (<https://html.spec.whatwg.org/multipage/form-elements.html#the-output-element>).
fn restore_outputs(
    ctx: &Ctx<'_>,
    world_rc: &std::rc::Rc<std::cell::RefCell<crate::js::World>>,
    document: u32,
    outputs: Vec<(blitz_traits::node_id::NodeId, crate::dom_string::DomString)>,
) -> Result<()> {
    let iframes = {
        let world = world_rc.borrow();
        let Some(mut parsed) = world.document_mut_by_id(document) else {
            return Ok(());
        };
        let mut iframes = Vec::new();
        for (control, text) in &outputs {
            // "String replace all" with the empty string leaves no child, not
            // an empty Text node
            // (<https://html.spec.whatwg.org/multipage/form-elements.html#the-output-element>).
            let added = if text.is_empty() {
                Vec::new()
            } else {
                let node = parsed.document.create_text(text);
                vec![NodeId { document, node }]
            };
            iframes.extend(replace_all_journaled(
                &mut parsed,
                NodeId {
                    document,
                    node: *control,
                },
                added,
            ));
        }
        iframes
    };
    world_rc.borrow().destroy_child_navigables(&iframes);
    schedule_mutation_delivery(ctx)
}

/// Form-associated elements whose form owner is `form`.
fn form_controls(
    base: &blitz_dom::BaseDocument,
    document: u32,
    form: blitz_traits::node_id::NodeId,
) -> Vec<blitz_traits::node_id::NodeId> {
    let form_id = NodeId {
        document,
        node: form,
    };
    let mut root = form;
    while let Some(parent) = base.get_node(root).and_then(|node| node.parent) {
        root = parent;
    }
    let mut controls = Vec::new();
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let Some(node) = base.get_node(current) else {
            continue;
        };
        stack.extend(node.children.iter().rev().copied());
        let Some(element) = node.data.downcast_element() else {
            continue;
        };
        if element.name.ns != html_namespace() {
            continue;
        }
        if !matches!(
            element.name.local.as_str(),
            "input" | "textarea" | "select" | "button" | "option" | "output"
        ) {
            continue;
        }
        let owned = element.name.local.as_str() == "option"
            || form_owner_of(base, document, current) == Some(form_id);
        if owned {
            controls.push(current);
        }
    }
    controls
}

pub(super) fn install(ctx: &Ctx<'_>) -> Result<()> {
    crate::js::bridge::object(ctx)?.set(
        "__tbSetSelectionRange",
        rquickjs::prelude::Func::from(set_selection_range_host),
    )?;
    html_form_element_generated::install(ctx)?;
    html_input_element_generated::install(ctx)?;
    html_text_area_element_generated::install(ctx)?;
    html_select_element_generated::install(ctx)?;
    html_option_element_generated::install(ctx)?;
    html_button_element_generated::install(ctx)?;
    html_field_set_element_generated::install(ctx)?;
    html_opt_group_element_generated::install(ctx)?;
    html_output_element_generated::install(ctx)?;
    html_data_list_element_generated::install(ctx)?;
    html_label_element_generated::install(ctx)?;
    html_legend_element_generated::install(ctx)?;
    Ok(())
}
