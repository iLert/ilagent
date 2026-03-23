use serde_json::Value;

pub fn get_nested_value<'a>(json: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = json;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    Some(current)
}
