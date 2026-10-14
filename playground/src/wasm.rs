//! The functions of this crate as a page calls them. Each takes the
//! program's text; what is not a string or a list of numbers comes back
//! as JSON.

use std::sync::Mutex;

use wasm_bindgen::prelude::*;

use crate::{PageClock, STEP_BUDGET, TOKEN_KINDS};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(text: &str);
}

/// What the last panic said. A panic ends the call with a trap, which
/// tells the page nothing; the page reads the message here and loads the
/// module again.
static PANIC: Mutex<String> = Mutex::new(String::new());

#[wasm_bindgen(start)]
fn start() {
    std::panic::set_hook(Box::new(|info| {
        let text = info.to_string();
        console_error(&text);
        if let Ok(mut panic) = PANIC.lock() {
            *panic = text;
        }
    }));
}

/// The message of the panic that ended the last call; empty if none did.
#[wasm_bindgen]
pub fn panic_message() -> String {
    PANIC.lock().map(|text| text.clone()).unwrap_or_default()
}

fn json(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// The version of silt: `"0.16.0"`.
#[wasm_bindgen]
pub fn version() -> String {
    crate::version().to_string()
}

/// Run the program: `{"diagnostics", "output", "error", "ok"}`.
#[wasm_bindgen]
pub fn run(source: &str) -> String {
    json(&crate::run(
        source,
        PageClock::new(js_sys::Date::now),
        STEP_BUDGET,
    ))
}

/// Check the program: `[{"from", "to", "severity", "message"}]`.
#[wasm_bindgen]
pub fn check(source: &str) -> String {
    json(&crate::check(source))
}

/// Format the program: `{"Ok": text}`, or `{"Err": report}`.
#[wasm_bindgen]
pub fn format(source: &str) -> String {
    json(&crate::format(source))
}

/// The names to complete with: `[{"label", "kind"}]`.
#[wasm_bindgen]
pub fn complete(source: &str) -> String {
    json(&crate::complete(source))
}

/// The coloured tokens: start, end and kind of each, one after the
/// other.
#[wasm_bindgen]
pub fn tokens(source: &str) -> Vec<u32> {
    crate::tokens(source)
}

/// The names of the kinds `tokens` numbers: `["keyword", ...]`.
#[wasm_bindgen]
pub fn token_kinds() -> String {
    json(&TOKEN_KINDS)
}
