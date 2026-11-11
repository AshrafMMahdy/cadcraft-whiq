//! The names given for `blocks_as_boxes`, checked: a list of names of blocks of the drawing.

use serde_json::Value;

use crate::Drawing;

/// The block names of a `blocks_as_boxes` argument: a LIST of strings (absent or null: none). Anything
/// else is an error naming what was given, the form expected with an example, and to call again.
pub fn boxed_names(v: Option<&Value>) -> Result<Vec<String>, String> {
    use Value;
    let wrong = |given: &Value| {
        format!(
            "`blocks_as_boxes` must be a list of block names, got {given}; give it as a list of names of blocks of the drawing, for example [\"Tree\", \"Chair\"], and call again"
        )
    };
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a.iter().map(|n| n.as_str().map(str::to_string).ok_or_else(|| wrong(n))).collect(),
        Some(other) => Err(wrong(other)),
    }
}

/// [`boxed_names`] checked against the drawing: every name must be a block of it (any case); the names
/// come back as the drawing spells them, each once. An unknown name is an error naming it, with the
/// nearest block names, where the full list is, and to call again.
pub fn checked_boxed_names(d: &Drawing, v: Option<&Value>) -> Result<Vec<String>, String> {
    let asked = boxed_names(v)?;
    let unknown: Vec<String> = asked.iter().filter(|n| d.block(n).is_none()).cloned().collect();
    if !unknown.is_empty() {
        return Err(unknown_blocks_message(d, &unknown));
    }
    let mut out: Vec<String> = Vec::new();
    for n in &asked {
        if let Some(b) = d.block(n)
            && !out.iter().any(|o| o.eq_ignore_ascii_case(&b.name))
        {
            out.push(b.name.clone());
        }
    }
    Ok(out)
}

/// The words for block names that are not blocks of `d`: which, the nearest names, where every name is
/// listed, and what to do.
pub fn unknown_blocks_message(d: &Drawing, unknown: &[String]) -> String {
    let mut seen = std::collections::HashSet::new();
    let list: Vec<String> = unknown.iter().filter(|n| seen.insert(n.as_str())).cloned().collect();
    let parts: Vec<String> = list
        .iter()
        .map(|n| {
            let near = nearest_blocks(d, n, 3);
            if near.is_empty() { format!("`{n}`") } else { format!("`{n}` (nearest block names: {})", near.join(", ")) }
        })
        .collect();
    format!(
        "`blocks_as_boxes` names {} that {} not a block of this drawing: {}; the read_drawing tool, section \"blocks\", lists every block name: call again with names taken from that list (any case)",
        if list.len() == 1 { "a name" } else { "names" },
        if list.len() == 1 { "is" } else { "are" },
        parts.join("; ")
    )
}

/// The `n` block names closest to `wanted` (edit distance on lower-case names).
fn nearest_blocks(d: &Drawing, wanted: &str, n: usize) -> Vec<String> {
    let w: Vec<char> = wanted.to_lowercase().chars().collect();
    let mut scored: Vec<(usize, String)> =
        d.blocks.values().map(|b| (distance(&w, &b.name.to_lowercase().chars().collect::<Vec<_>>()), b.name.clone())).collect();
    scored.sort();
    scored.into_iter().take(n).map(|(_, name)| name).collect()
}

fn distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev.get(j).copied().unwrap_or(0) + usize::from(ca != cb);
            let del = prev.get(j + 1).copied().unwrap_or(0) + 1;
            let ins = cur.get(j).copied().unwrap_or(0) + 1;
            cur.push(sub.min(del).min(ins));
        }
        prev = cur;
    }
    prev.last().copied().unwrap_or(0)
}
