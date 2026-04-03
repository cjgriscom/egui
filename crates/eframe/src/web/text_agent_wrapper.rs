//! Unified interface for web text input.

use wasm_bindgen::prelude::*;

use super::{
    WebRunner,
    edit_context::{self, EditContextAgent},
    text_agent::TextAgent,
};

/// Dispatches text-input integration to a concrete implementation.
pub enum TextAgentWrapper {
    TextAgent(TextAgent),
    EditContext(EditContextAgent),
}

impl TextAgentWrapper {
    pub fn attach(
        runner_ref: &WebRunner,
        canvas: &web_sys::HtmlCanvasElement,
    ) -> Result<Self, JsValue> {
        if edit_context::supported() {
            Ok(Self::EditContext(EditContextAgent::attach(
                runner_ref, canvas,
            )?))
        } else {
            let root = canvas.get_root_node();
            Ok(Self::TextAgent(TextAgent::attach(runner_ref, root)?))
        }
    }

    pub fn move_to(
        &self,
        ime: Option<egui::output::IMEOutput>,
        canvas: &web_sys::HtmlCanvasElement,
        zoom_factor: f32,
    ) -> Result<(), JsValue> {
        match self {
            Self::TextAgent(agent) => agent.move_to(ime, canvas, zoom_factor),
            Self::EditContext(agent) => agent.move_to(ime, canvas, zoom_factor),
        }
    }

    pub fn set_focus(&self, on: bool) {
        match self {
            Self::TextAgent(agent) => agent.set_focus(on),
            Self::EditContext(agent) => agent.set_focus(on),
        }
    }

    pub fn has_focus(&self) -> bool {
        match self {
            Self::TextAgent(agent) => agent.has_focus(),
            Self::EditContext(agent) => agent.has_focus(),
        }
    }

    pub fn focus(&self) {
        match self {
            Self::TextAgent(agent) => agent.focus(),
            Self::EditContext(agent) => agent.focus(),
        }
    }

    pub fn blur(&self) {
        match self {
            Self::TextAgent(agent) => agent.blur(),
            Self::EditContext(agent) => agent.blur(),
        }
    }
}
