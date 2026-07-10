/// Extract human-readable text from a `textDocument/hover` JSON response.
pub fn extract_hover_text(val: &serde_json::Value) -> Option<String> {
    let contents = val.get("contents")?;
    let text = if let Some(s) = contents.as_str() {
        s.to_owned()
    } else if let Some(obj) = contents.as_object() {
        obj.get("value")?.as_str()?.to_owned()
    } else if let Some(arr) = contents.as_array() {
        arr.iter()
            .filter_map(|item| {
                if let Some(s) = item.as_str() {
                    Some(s.to_owned())
                } else {
                    item.get("value")?.as_str().map(|s| s.to_owned())
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        return None;
    };
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}
