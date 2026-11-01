//! Business word limits for acknowledgements/notes, including ordinary Chinese.
//! This is not a JS string/runtime implementation; no process or network is used.
use crate::{Error, Result};
pub fn words(text: &str) -> usize {
    icu_segmenter::WordSegmenter::new_auto(Default::default())
        .segment_str(text)
        .iter_with_word_type()
        .filter(|(_, kind)| kind.is_word_like())
        .count()
}
pub fn checked(text: &str, limit: usize, label: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Message(format!("{label} must not be empty")));
    }
    if words(text) > limit {
        return Err(Error::Message(format!(
            "{label} has at most {limit} words (Chinese uses word segmentation)"
        )));
    }
    Ok(text.into())
}
