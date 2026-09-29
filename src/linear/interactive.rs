use super::prelude::*;
use super::*;

// ---------------------------------------------------------------------------
// Interactive disambiguation
// ---------------------------------------------------------------------------

/// Prompt to pick a near-match when an exact name lookup failed.
///
/// `options` is ordered `(id, display)`; the display text is shown to the user.
pub fn select_option(
    data_name: &str,
    original_value: &str,
    options: &[(String, String)],
) -> Result<Option<String>> {
    if options.is_empty() {
        return Ok(None);
    }

    if options.len() == 1 {
        let (key, display) = &options[0];
        let message = format!(
            "{data_name} named {original_value} does not exist, but {display} exists. Is this what you meant?"
        );
        let labels = vec!["yes".to_string(), "no".to_string()];
        let selected = prompt::select(&message, &labels)?;
        return Ok(if selected == 0 {
            Some(key.clone())
        } else {
            None
        });
    }

    let message = format!(
        "{data_name} with {original_value} does not exist, but the following exist. Is any of these what you meant?"
    );
    let mut labels: Vec<String> = options.iter().map(|(_, display)| display.clone()).collect();
    labels.push("none of the above".to_string());
    let selected = prompt::select(&message, &labels)?;
    if selected >= options.len() {
        Ok(None)
    } else {
        Ok(Some(options[selected].0.clone()))
    }
}
