//! Per-message feedback: ratings with optimistic concurrency.
//!
//! `put` carries `ifVersion`: the observed item's version when updating, and **`null` to
//! require that no item exists** when creating. Sending a version for a message that has
//! no feedback, or omitting one for a message that does, is how a concurrent edit gets
//! clobbered instead of refused.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Overall judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rating {
    Positive,
    Negative,
}

/// One current feedback value and its mutation token.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Item {
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub rating: Rating,
    #[serde(default)]
    pub note: Option<String>,
    /// Equality-only token, replaced by every material create or update.
    pub version: Value,
}

/// Build the `put` request for a rating change.
///
/// `current` is the feedback already held for the message, if any. Its absence produces
/// `ifVersion: null`, which asks the host to refuse the write if an item has appeared in
/// the meantime.
pub fn put_request(
    session_id: &str,
    message_id: &str,
    rating: Rating,
    note: Option<&str>,
    current: Option<&Item>,
) -> Value {
    let if_version = match current {
        Some(item) => item.version.clone(),
        None => Value::Null,
    };
    let mut request = serde_json::json!({
        "sessionId": session_id,
        "messageId": message_id,
        "rating": rating,
        "ifVersion": if_version,
    });
    // A blank note is not a note; sending one would store an empty explanation.
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        request["note"] = Value::String(note.to_string());
    }
    request
}

/// Build the `delete` request for clearing feedback.
///
/// Deleting requires the observed version: without it a clear could remove someone else's
/// newer rating.
pub fn delete_request(session_id: &str, item: &Item) -> Value {
    serde_json::json!({
        "sessionId": session_id,
        "messageId": item.message_id,
        "ifVersion": item.version,
    })
}

/// Toggle semantics: pressing the rating a message already carries clears it.
pub enum Intent {
    /// Write this rating.
    Set(Rating),
    /// Remove the existing feedback.
    Clear,
}

/// What pressing a rating key means, given what the message already carries.
pub fn intent(pressed: Rating, current: Option<&Item>) -> Intent {
    match current {
        Some(item) if item.rating == pressed => Intent::Clear,
        _ => Intent::Set(pressed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(rating: Rating) -> Item {
        Item {
            message_id: "m1".into(),
            rating,
            note: None,
            version: serde_json::json!("v7"),
        }
    }

    #[test]
    fn creating_feedback_requires_that_none_exists() {
        let request = put_request("s1", "m1", Rating::Positive, None, None);
        // `null` asks the host to refuse if an item appeared in the meantime.
        assert_eq!(request["ifVersion"], Value::Null);
        assert_eq!(request["rating"], "positive");
        assert!(request.get("note").is_none());
    }

    #[test]
    fn updating_feedback_carries_the_observed_version() {
        let existing = item(Rating::Positive);
        let request = put_request("s1", "m1", Rating::Negative, Some("wrong file"), Some(&existing));
        assert_eq!(request["ifVersion"], serde_json::json!("v7"));
        assert_eq!(request["rating"], "negative");
        assert_eq!(request["note"], "wrong file");
    }

    #[test]
    fn a_blank_note_is_not_stored_as_a_note() {
        let request = put_request("s1", "m1", Rating::Positive, Some("   "), None);
        assert!(request.get("note").is_none());
    }

    #[test]
    fn deleting_carries_the_version_so_it_cannot_clobber_a_newer_rating() {
        let existing = item(Rating::Negative);
        let request = delete_request("s1", &existing);
        assert_eq!(request["ifVersion"], serde_json::json!("v7"));
        assert_eq!(request["messageId"], "m1");
    }

    #[test]
    fn pressing_the_same_rating_clears_it() {
        let existing = item(Rating::Positive);
        assert!(matches!(intent(Rating::Positive, Some(&existing)), Intent::Clear));
        assert!(matches!(
            intent(Rating::Negative, Some(&existing)),
            Intent::Set(Rating::Negative)
        ));
        assert!(matches!(intent(Rating::Positive, None), Intent::Set(Rating::Positive)));
    }

    #[test]
    fn ratings_use_their_wire_spelling() {
        assert_eq!(serde_json::to_value(Rating::Positive).unwrap(), "positive");
        assert_eq!(serde_json::to_value(Rating::Negative).unwrap(), "negative");
        let parsed: Rating = serde_json::from_value(serde_json::json!("negative")).unwrap();
        assert_eq!(parsed, Rating::Negative);
    }
}
