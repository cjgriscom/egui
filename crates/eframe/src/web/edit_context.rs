/// EditContext-backed text input: attaches an [`EditContext`] to the canvas,
/// providing text input and IME support via the modern EditContext API.
/// Falls back to [`TextAgent`](super::text_agent::TextAgent) on unsupported browsers.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::{Array, Reflect};
use wasm_bindgen::prelude::*;

use super::{AppRunner, WebRunner};

pub struct EditContextAgent {
    context: EditContext,
    canvas: web_sys::HtmlCanvasElement,
    is_active: Cell<bool>,
    is_composing: Rc<Cell<bool>>,
    prev_ime_output: RefCell<Option<egui::output::IMEOutput>>,
}

impl EditContextAgent {
    pub fn attach(
        runner_ref: &WebRunner,
        canvas: &web_sys::HtmlCanvasElement,
    ) -> Result<Self, JsValue> {
        let context = EditContext::new();
        let canvas = canvas.clone();
        let is_composing = Rc::new(Cell::new(false));

        {
            let context_ref = context.clone();
            let is_composing = is_composing.clone();
            runner_ref.add_event_listener(
                &context,
                "textupdate",
                move |event: web_sys::Event, runner: &mut AppRunner| {
                    if is_composing.get() {
                        // During composition, event.text is the current preedit
                        // replacement — forward it directly to egui.
                        let preedit = Reflect::get(&event, &JsValue::from_str("text"))
                            .ok()
                            .and_then(|v| v.as_string())
                            .unwrap_or_default();
                        runner
                            .input
                            .raw
                            .events
                            .push(egui::Event::Ime(egui::ImeEvent::Preedit(preedit)));
                    } else {
                        // Outside composition the EditContext is authoritative.
                        // Read the full buffer and send it as a TextChanged event
                        // so egui's TextEdit replaces its contents to match.
                        let full_text = context_ref.text();
                        let sel_start = Reflect::get(&event, &JsValue::from_str("selectionStart"))
                            .ok()
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0) as u32;
                        let cursor = utf16_to_char(&full_text, sel_start);
                        runner.input.raw.events.push(egui::Event::TextChanged {
                            text: full_text,
                            cursor,
                        });
                    }
                    runner.needs_repaint.repaint_asap();
                },
            )?;
        }

        {
            let is_composing = is_composing.clone();
            runner_ref.add_event_listener(
                &context,
                "compositionstart",
                move |_: web_sys::Event, runner: &mut AppRunner| {
                    is_composing.set(true);
                    runner
                        .input
                        .raw
                        .events
                        .push(egui::Event::Ime(egui::ImeEvent::Enabled));
                    runner.needs_repaint.repaint_asap();
                },
            )?;
        }

        {
            let is_composing = is_composing.clone();
            runner_ref.add_event_listener(
                &context,
                "compositionend",
                move |event: web_sys::Event, runner: &mut AppRunner| {
                    is_composing.set(false);
                    let committed = Reflect::get(&event, &JsValue::from_str("data"))
                        .ok()
                        .and_then(|v| v.as_string())
                        .unwrap_or_default();
                    if !committed.is_empty() {
                        runner
                            .input
                            .raw
                            .events
                            .push(egui::Event::Ime(egui::ImeEvent::Commit(committed)));
                    }
                    runner.needs_repaint.repaint_asap();
                },
            )?;
        }

        Ok(Self {
            context,
            canvas,
            is_active: Cell::new(false),
            is_composing,
            prev_ime_output: Default::default(),
        })
    }

    pub fn move_to(
        &self,
        ime: Option<egui::output::IMEOutput>,
        canvas: &web_sys::HtmlCanvasElement,
        zoom_factor: f32,
    ) -> Result<(), JsValue> {
        if *self.prev_ime_output.borrow() == ime {
            return Ok(());
        }
        self.prev_ime_output.replace(ime.clone());

        let Some(ime) = ime else { return Ok(()) };

        // Sync the EditContext text buffer with egui's current text (only
        // when not mid-composition — the OS owns the buffer during IME).
        if !self.is_composing.get() {
            let utf16_len = self.context.text().encode_utf16().count() as u32;
            self.context.update_text(0, utf16_len, &ime.text);
            let sel_start = char_to_utf16(&ime.text, ime.cursor_primary);
            let sel_end = char_to_utf16(&ime.text, ime.cursor_secondary);
            self.context.update_selection(sel_start, sel_end);
        }

        let canvas_rect = super::canvas_content_rect(canvas);
        let widget_rect = ime.rect.translate(canvas_rect.min.to_vec2());
        let cursor_rect = ime.cursor_rect.translate(canvas_rect.min.to_vec2());

        let control_bounds = web_sys::DomRect::new_with_x_and_y_and_width_and_height(
            (widget_rect.min.x * zoom_factor) as f64,
            (widget_rect.min.y * zoom_factor) as f64,
            (widget_rect.width() * zoom_factor) as f64,
            (widget_rect.height() * zoom_factor) as f64,
        )?;
        self.context.update_control_bounds(&control_bounds);

        let selection_bounds = web_sys::DomRect::new_with_x_and_y_and_width_and_height(
            (cursor_rect.min.x * zoom_factor) as f64,
            (cursor_rect.min.y * zoom_factor) as f64,
            (cursor_rect.width() * zoom_factor) as f64,
            (cursor_rect.height() * zoom_factor) as f64,
        )?;
        self.context.update_selection_bounds(&selection_bounds);

        Ok(())
    }

    pub fn set_focus(&self, on: bool) {
        if on {
            self.focus();
        } else {
            self.blur();
        }
    }

    /// Returns `true` when the EditContext is attached to the canvas
    /// (i.e., a text field is active and we're handling text input).
    pub fn has_focus(&self) -> bool {
        self.is_active.get()
    }

    /// Attach the EditContext to the canvas, making it editable and
    /// causing the virtual keyboard to appear on mobile.
    pub fn focus(&self) {
        if self.is_active.get() {
            return;
        }
        log::trace!("EditContext: activating");
        Reflect::set(
            &self.canvas,
            &JsValue::from_str("editContext"),
            &self.context,
        )
        .ok();
        self.is_active.set(true);

        // Re-populate the buffer from the last-known text so it isn't empty
        // if the user types before the next `move_to` sync (e.g. after the
        // focus-cycle hack in the touchend handler).
        if let Some(ime) = self.prev_ime_output.borrow().as_ref() {
            let utf16_len = self.context.text().encode_utf16().count() as u32;
            self.context.update_text(0, utf16_len, &ime.text);
            let sel_start = char_to_utf16(&ime.text, ime.cursor_primary);
            let sel_end = char_to_utf16(&ime.text, ime.cursor_secondary);
            self.context.update_selection(sel_start, sel_end);
        }

        self.canvas.focus().ok();
    }

    /// Detach the EditContext from the canvas, dismissing the virtual keyboard.
    pub fn blur(&self) {
        if !self.is_active.get() {
            return;
        }
        log::trace!("EditContext: deactivating");
        self.is_composing.set(false);
        clear_edit_context_buffer(&self.context);

        Reflect::set(
            &self.canvas,
            &JsValue::from_str("editContext"),
            &JsValue::NULL,
        )
        .ok();
        self.is_active.set(false);
    }
}

impl Drop for EditContextAgent {
    fn drop(&mut self) {
        Reflect::set(
            &self.canvas,
            &JsValue::from_str("editContext"),
            &JsValue::NULL,
        )
        .ok();
    }
}

fn clear_edit_context_buffer(context: &EditContext) {
    let utf16_len = context.text().encode_utf16().count() as u32;
    if utf16_len > 0 {
        context.update_text(0, utf16_len, "");
        context.update_selection(0, 0);
    }
}

/// Convert a UTF-16 code-unit offset to a character offset.
fn utf16_to_char(text: &str, utf16_offset: u32) -> usize {
    let mut units = 0u32;
    for (i, c) in text.chars().enumerate() {
        if units >= utf16_offset {
            return i;
        }
        units += c.len_utf16() as u32;
    }
    text.chars().count()
}

/// Convert a character offset to a UTF-16 code-unit offset.
fn char_to_utf16(text: &str, char_offset: usize) -> u32 {
    text.chars()
        .take(char_offset)
        .map(|c| c.len_utf16())
        .sum::<usize>() as u32
}

pub fn supported() -> bool {
    let global = js_sys::global();
    let edit_context = Reflect::get(&global, &JsValue::from_str("EditContext")).unwrap_or_default();
    edit_context.is_function()
}

/// Manual [`EditContext`](https://developer.mozilla.org/en-US/docs/Web/API/EditContext) bindings
/// until `web-sys` exposes this type.
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(extends = web_sys::EventTarget, js_name = EditContext)]
    #[derive(Clone)]
    pub type EditContext;

    #[wasm_bindgen(constructor)]
    pub fn new() -> EditContext;

    #[wasm_bindgen(method, getter)]
    pub fn text(this: &EditContext) -> String;

    #[wasm_bindgen(method, getter, js_name = selectionStart)]
    pub fn selection_start(this: &EditContext) -> u32;

    #[wasm_bindgen(method, getter, js_name = selectionEnd)]
    pub fn selection_end(this: &EditContext) -> u32;

    #[wasm_bindgen(method, getter, js_name = characterBoundsRangeStart)]
    pub fn character_bounds_range_start(this: &EditContext) -> u32;

    #[wasm_bindgen(method, getter, js_name = ontextupdate)]
    pub fn ontextupdate(this: &EditContext) -> JsValue;

    #[wasm_bindgen(method, setter, js_name = ontextupdate)]
    pub fn set_ontextupdate(this: &EditContext, value: &JsValue);

    #[wasm_bindgen(method, getter, js_name = ontextformatupdate)]
    pub fn ontextformatupdate(this: &EditContext) -> JsValue;

    #[wasm_bindgen(method, setter, js_name = ontextformatupdate)]
    pub fn set_ontextformatupdate(this: &EditContext, value: &JsValue);

    #[wasm_bindgen(method, getter, js_name = oncharacterboundsupdate)]
    pub fn oncharacterboundsupdate(this: &EditContext) -> JsValue;

    #[wasm_bindgen(method, setter, js_name = oncharacterboundsupdate)]
    pub fn set_oncharacterboundsupdate(this: &EditContext, value: &JsValue);

    #[wasm_bindgen(method, getter, js_name = oncompositionstart)]
    pub fn oncompositionstart(this: &EditContext) -> JsValue;

    #[wasm_bindgen(method, setter, js_name = oncompositionstart)]
    pub fn set_oncompositionstart(this: &EditContext, value: &JsValue);

    #[wasm_bindgen(method, getter, js_name = oncompositionend)]
    pub fn oncompositionend(this: &EditContext) -> JsValue;

    #[wasm_bindgen(method, setter, js_name = oncompositionend)]
    pub fn set_oncompositionend(this: &EditContext, value: &JsValue);

    #[wasm_bindgen(method, js_name = updateText)]
    pub fn update_text(this: &EditContext, range_start: u32, range_end: u32, text: &str);

    #[wasm_bindgen(method, js_name = updateSelection)]
    pub fn update_selection(this: &EditContext, start: u32, end: u32);

    #[wasm_bindgen(method, js_name = updateControlBounds)]
    pub fn update_control_bounds(this: &EditContext, control_bounds: &web_sys::DomRect);

    #[wasm_bindgen(method, js_name = updateSelectionBounds)]
    pub fn update_selection_bounds(this: &EditContext, selection_bounds: &web_sys::DomRect);

    /// `character_bounds` must be a JavaScript array of [`DomRect`](https://developer.mozilla.org/en-US/docs/Web/API/DOMRect) values.
    #[wasm_bindgen(method, js_name = updateCharacterBounds)]
    pub fn update_character_bounds(this: &EditContext, range_start: u32, character_bounds: &Array);

    #[wasm_bindgen(method, js_name = attachedElements)]
    pub fn attached_elements(this: &EditContext) -> Array;

    #[wasm_bindgen(method, js_name = characterBounds)]
    pub fn character_bounds(this: &EditContext) -> Array;
}

impl EditContext {
    /// `new EditContext(options)` — pass a plain object matching [`EditContextInit`](https://developer.mozilla.org/en-US/docs/Web/API/EditContext/EditContext#options).
    pub fn new_with_options(options: &JsValue) -> Result<Self, JsValue> {
        let global = js_sys::global();
        let ctor = Reflect::get(&global, &JsValue::from_str("EditContext"))?;
        let ctor = ctor
            .dyn_into::<js_sys::Function>()
            .map_err(|_e| JsValue::from_str("EditContext is not a constructor"))?;
        let args = Array::new();
        args.push(options);
        Reflect::construct(&ctor, &args).map(|v| v.unchecked_into())
    }
}

/// Backwards-compatible alias: pass a [`JsValue`] object (e.g. from `js_sys::Object`).
#[expect(dead_code)]
pub type EditContextInit = JsValue;
