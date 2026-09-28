//! Small edits to TOML documents that keep the comments and layout people
//! wrote.
//!
//! Only what cubby needs: lists of strings at the top level (added to and
//! removed from one element at a time, so a comment above an element stays
//! with it) and a key moved to the top of the file.

use anyhow::{Result, bail};
use toml_edit::{Array, DocumentMut, Item, Key, Value};

/// The top-level array at `key`, created (one element per line) when the
/// document does not have it yet.
pub fn array_mut<'a>(doc: &'a mut DocumentMut, key: &str) -> Result<&'a mut Array> {
    let table = doc.as_table_mut();
    if !table.contains_key(key) {
        let mut array = Array::new();
        array.set_trailing("\n");
        array.set_trailing_comma(true);
        let mut k = Key::new(key);
        k.leaf_decor_mut().set_prefix("\n");
        table.insert_formatted(&k, Item::Value(Value::Array(array)));
    }
    match table.get_mut(key).and_then(Item::as_array_mut) {
        Some(a) => Ok(a),
        None => bail!("`{key}` must be a list"),
    }
}

/// Insert `s` at `index`, laid out like the array's other elements: one per
/// line when they are (or the array is empty), inline otherwise.
pub fn insert_str(array: &mut Array, index: usize, s: &str) {
    let index = index.min(array.len());
    if is_multiline(array) {
        let indent = indent_of(array);
        array.insert_formatted(index, Value::from(s).decorated(format!("\n{indent}"), ""));
        array.set_trailing_comma(true);
        if !array.trailing().as_str().is_some_and(|t| t.contains('\n')) {
            array.set_trailing("\n");
        }
    } else {
        array.insert_formatted(index, Value::from(s).decorated(" ", ""));
        tidy_inline(array);
    }
}

/// Remove every element for which `pred` is true. Returns how many went.
pub fn remove_where(array: &mut Array, pred: impl Fn(&Value) -> bool) -> usize {
    let mut removed = 0;
    let mut i = 0;
    while i < array.len() {
        if array.get(i).is_some_and(&pred) {
            array.remove(i);
            removed += 1;
        } else {
            i += 1;
        }
    }
    if !is_multiline(array) {
        tidy_inline(array);
    }
    removed
}

/// Set `key = value` as the first entry of the document, taking over the
/// comment block that starts the file so it stays at the top.
pub fn set_first(doc: &mut DocumentMut, key: &str, value: Item) {
    let table = doc.as_table_mut();
    if let Some(item) = table.get_mut(key) {
        *item = keep_decor(item, value);
        return;
    }
    let names: Vec<String> = table.iter().map(|(k, _)| k.to_owned()).collect();
    let mut entries: Vec<(Key, Item)> = names
        .iter()
        .filter_map(|n| table.get_key_value(n))
        .map(|(k, i)| (k.clone(), i.clone()))
        .collect();
    table.clear();
    let mut first = Key::new(key);
    if let Some((k, _)) = entries.first_mut()
        && let Some(prefix) = k.leaf_decor().prefix().and_then(|p| p.as_str())
    {
        let prefix = prefix.to_owned();
        first.leaf_decor_mut().set_prefix(prefix);
        k.leaf_decor_mut().set_prefix("\n");
    }
    table.insert_formatted(&first, value);
    for (k, item) in entries {
        table.insert_formatted(&k, item);
    }
}

/// `new`, with the comments and spacing that were around `old`.
fn keep_decor(old: &Item, new: Item) -> Item {
    match (old.as_value(), new) {
        (Some(o), Item::Value(mut v)) => {
            *v.decor_mut() = o.decor().clone();
            Item::Value(v)
        }
        (_, new) => new,
    }
}

fn is_multiline(array: &Array) -> bool {
    array.is_empty()
        || array
            .iter()
            .any(|v| prefix(v).is_some_and(|p| p.contains('\n')))
}

/// The indentation of the array's last element, or two spaces.
fn indent_of(array: &Array) -> String {
    array
        .iter()
        .filter_map(prefix)
        .filter_map(|p| p.rsplit_once('\n').map(|(_, indent)| indent))
        .filter(|indent| indent.chars().all(char::is_whitespace))
        .last()
        .unwrap_or("  ")
        .to_owned()
}

/// `["a", "b"]` rather than `[ "a","b"]` after elements moved around.
/// Leaves alone any spacing that holds a comment.
fn tidy_inline(array: &mut Array) {
    for i in 0..array.len() {
        if let Some(v) = array.get_mut(i)
            && prefix(v).is_none_or(|p| p.chars().all(char::is_whitespace))
        {
            v.decor_mut().set_prefix(if i == 0 { "" } else { " " });
        }
    }
}

fn prefix(v: &Value) -> Option<&str> {
    v.decor().prefix().and_then(|p| p.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> DocumentMut {
        text.parse().unwrap()
    }

    #[test]
    fn inserting_keeps_comments_and_layout() {
        let mut d = doc("# top\nlist = [\n  \"a\",\n  # about c\n  \"c\",\n]\n");
        let a = array_mut(&mut d, "list").unwrap();
        insert_str(a, 1, "b");
        assert_eq!(
            d.to_string(),
            "# top\nlist = [\n  \"a\",\n  \"b\",\n  # about c\n  \"c\",\n]\n"
        );
        let a = array_mut(&mut d, "list").unwrap();
        assert_eq!(remove_where(a, |v| v.as_str() == Some("c")), 1);
        assert_eq!(d.to_string(), "# top\nlist = [\n  \"a\",\n  \"b\",\n]\n");
    }

    #[test]
    fn inline_arrays_stay_inline() {
        let mut d = doc("list = [\"b\", \"c\"]\n");
        let a = array_mut(&mut d, "list").unwrap();
        insert_str(a, 0, "a");
        assert_eq!(d.to_string(), "list = [\"a\", \"b\", \"c\"]\n");
        let a = array_mut(&mut d, "list").unwrap();
        remove_where(a, |v| v.as_str() == Some("a"));
        assert_eq!(d.to_string(), "list = [\"b\", \"c\"]\n");
    }

    #[test]
    fn missing_and_empty_arrays_become_one_per_line() {
        let mut d = doc("x = 1\nempty = []\n");
        insert_str(array_mut(&mut d, "empty").unwrap(), 0, "a");
        insert_str(array_mut(&mut d, "new").unwrap(), 0, "b");
        assert_eq!(
            d.to_string(),
            "x = 1\nempty = [\n  \"a\",\n]\n\nnew = [\n  \"b\",\n]\n"
        );
        assert!(array_mut(&mut d, "x").is_err());
    }

    #[test]
    fn set_first_moves_the_header_comment() {
        let mut d = doc("# header\n# more\n\nlist = [\n]\n");
        set_first(&mut d, "version", toml_edit::value(2));
        assert_eq!(
            d.to_string(),
            "# header\n# more\n\nversion = 2\n\nlist = [\n]\n"
        );
        // An existing key keeps its place and its comment.
        let mut d = doc("list = []\n# the format\nversion = 1 # old\n");
        set_first(&mut d, "version", toml_edit::value(2));
        assert_eq!(
            d.to_string(),
            "list = []\n# the format\nversion = 2 # old\n"
        );
    }
}
