//! `ask_user_question`: the composer takeover and the plan-review card.
//!
//! Two rules from the contract carry real consequences:
//!
//! - A `plan-review` intent names **which option approves** the plan. It is named rather
//!   than positional precisely so no UI infers the verdict from option order — reading the
//!   first option as approval would approve a plan the user declined.
//! - An intent a build does not recognize renders the generic option list. The answer
//!   encoding is identical either way: an intent changes presentation only, never the
//!   protocol, so an unknown tag must never block answering.

use serde::Deserialize;
use serde_json::Value;

/// One selectable answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Option_ {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A caller-declared presentation intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// A plan submitted for review; the named label approves it.
    PlanReview { approve: String },
    /// A tag this build does not know: render the generic flow.
    Unrecognized(String),
}

/// One question in a request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Question {
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<Option_>,
    /// Defaults to single-select.
    #[serde(default, rename = "multiSelect")]
    pub multi_select: bool,
    #[serde(default)]
    intent: Option<Value>,
}

impl Question {
    /// The presentation intent, if the caller declared one.
    pub fn intent(&self) -> Option<Intent> {
        let intent = self.intent.as_ref()?;
        match intent.get("kind").and_then(Value::as_str)? {
            "plan-review" => {
                let approve = intent.get("approve")?.as_str()?.to_string();
                // An `approve` naming no option of its own question is rejected upstream
                // at `ask()`, but a client that trusted it blindly would still mislabel
                // the card, so the naming is verified here too.
                self.options
                    .iter()
                    .any(|option| option.label == approve)
                    .then_some(Intent::PlanReview { approve })
            }
            other => Some(Intent::Unrecognized(other.to_string())),
        }
    }

    /// Whether this question is a plan review this build can present as one.
    pub fn plan_review_approve(&self) -> Option<String> {
        match self.intent() {
            Some(Intent::PlanReview { approve }) => Some(approve),
            _ => None,
        }
    }

    /// Whether a given label approves the plan.
    ///
    /// Every option that is not the named one declines it.
    pub fn approves(&self, label: &str) -> bool {
        self.plan_review_approve().as_deref() == Some(label)
    }
}

/// The pending request.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub questions: Vec<Question>,
}

/// The human's in-progress answers.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    /// question id → selected labels.
    selections: Vec<(String, Vec<String>)>,
    /// question id → free-text answer.
    customs: Vec<(String, String)>,
}

impl Draft {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&mut self, id: &str) -> &mut Vec<String> {
        if let Some(index) = self.selections.iter().position(|(key, _)| key == id) {
            return &mut self.selections[index].1;
        }
        self.selections.push((id.to_string(), Vec::new()));
        &mut self.selections.last_mut().unwrap().1
    }

    pub fn selected(&self, id: &str) -> &[String] {
        self.selections
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, labels)| labels.as_slice())
            .unwrap_or(&[])
    }

    pub fn custom(&self, id: &str) -> Option<&str> {
        self.customs
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, text)| text.as_str())
    }

    /// Choose an option.
    ///
    /// A single-select question replaces its selection; a multi-select one toggles, so the
    /// same key both adds and removes.
    pub fn choose(&mut self, question: &Question, label: &str) {
        let multi = question.multi_select;
        let slot = self.slot(&question.id);
        if !multi {
            slot.clear();
            slot.push(label.to_string());
            return;
        }
        match slot.iter().position(|held| held == label) {
            Some(index) => {
                slot.remove(index);
            }
            None => slot.push(label.to_string()),
        }
    }

    /// Set the free-text answer, clearing it when blank.
    pub fn set_custom(&mut self, id: &str, text: &str) {
        let trimmed = text.trim();
        self.customs.retain(|(key, _)| key != id);
        if !trimmed.is_empty() {
            self.customs.push((id.to_string(), trimmed.to_string()));
        }
    }

    /// Whether every question has an answer.
    ///
    /// A free-text answer counts: a question offering options can still be answered with
    /// custom text alone.
    pub fn is_complete(&self, request: &Request) -> bool {
        request.questions.iter().all(|question| {
            !self.selected(&question.id).is_empty() || self.custom(&question.id).is_some()
        })
    }

    /// Encode the answer.
    ///
    /// The encoding is identical whether a question was presented generically or through a
    /// recognized intent.
    pub fn encode(&self, request: &Request) -> Value {
        let answers: Vec<Value> = request
            .questions
            .iter()
            .map(|question| {
                let mut answer = serde_json::json!({
                    "id": question.id,
                    "selected": self.selected(&question.id),
                });
                if let Some(custom) = self.custom(&question.id) {
                    answer["custom"] = Value::String(custom.to_string());
                }
                answer
            })
            .collect();
        serde_json::json!({ "answers": answers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(value: Value) -> Request {
        serde_json::from_value(value).expect("request")
    }

    fn plan_review() -> Request {
        request(serde_json::json!({ "questions": [{
            "id": "q1",
            "question": "Approve this plan?",
            "detail": "1. Do the thing\n2. Test it",
            "options": [
                { "label": "Reject" },
                { "label": "Approve", "description": "Proceed as written" }
            ],
            "intent": { "kind": "plan-review", "approve": "Approve" }
        }] }))
    }

    #[test]
    fn approval_is_named_never_positional() {
        let request = plan_review();
        let question = &request.questions[0];
        // "Reject" is listed first; a UI reading option order would approve a decline.
        assert_eq!(question.plan_review_approve().as_deref(), Some("Approve"));
        assert!(question.approves("Approve"));
        assert!(!question.approves("Reject"));
    }

    #[test]
    fn an_approve_naming_no_option_is_not_presented_as_a_plan_review() {
        let request = request(serde_json::json!({ "questions": [{
            "id": "q1", "question": "?",
            "options": [{ "label": "Yes" }],
            "intent": { "kind": "plan-review", "approve": "Ship it" }
        }] }));
        // Rejected upstream at `ask()`, but trusting it blindly would mislabel the card.
        assert!(request.questions[0].plan_review_approve().is_none());
    }

    #[test]
    fn an_unknown_intent_falls_back_to_the_generic_list() {
        let request = request(serde_json::json!({ "questions": [{
            "id": "q1", "question": "?",
            "options": [{ "label": "A" }, { "label": "B" }],
            "intent": { "kind": "some-future-card", "extra": 1 }
        }] }));
        let question = &request.questions[0];
        assert_eq!(
            question.intent(),
            Some(Intent::Unrecognized("some-future-card".into()))
        );
        // An intent changes presentation only; answering must still work.
        assert!(question.plan_review_approve().is_none());
        let mut draft = Draft::new();
        draft.choose(question, "B");
        assert!(draft.is_complete(&request));
    }

    #[test]
    fn single_select_replaces_and_multi_select_toggles() {
        let single = request(serde_json::json!({ "questions": [{
            "id": "q1", "question": "?", "options": [{ "label": "A" }, { "label": "B" }]
        }] }));
        let mut draft = Draft::new();
        draft.choose(&single.questions[0], "A");
        draft.choose(&single.questions[0], "B");
        assert_eq!(draft.selected("q1"), ["B"]);

        let multi = request(serde_json::json!({ "questions": [{
            "id": "q1", "question": "?", "multiSelect": true,
            "options": [{ "label": "A" }, { "label": "B" }]
        }] }));
        let mut draft = Draft::new();
        draft.choose(&multi.questions[0], "A");
        draft.choose(&multi.questions[0], "B");
        assert_eq!(draft.selected("q1"), ["A", "B"]);
        // The same key removes it again.
        draft.choose(&multi.questions[0], "A");
        assert_eq!(draft.selected("q1"), ["B"]);
    }

    #[test]
    fn free_text_alone_answers_a_question() {
        let request = request(serde_json::json!({ "questions": [{
            "id": "q1", "question": "?", "options": [{ "label": "A" }]
        }] }));
        let mut draft = Draft::new();
        assert!(!draft.is_complete(&request));
        draft.set_custom("q1", "  something else  ");
        assert!(draft.is_complete(&request));
        assert_eq!(draft.custom("q1"), Some("something else"));
        // Blanking it removes the answer rather than storing an empty string.
        draft.set_custom("q1", "   ");
        assert!(!draft.is_complete(&request));
    }

    #[test]
    fn every_question_must_be_answered() {
        let request = request(serde_json::json!({ "questions": [
            { "id": "q1", "question": "?", "options": [{ "label": "A" }] },
            { "id": "q2", "question": "?", "options": [{ "label": "B" }] }
        ] }));
        let mut draft = Draft::new();
        draft.choose(&request.questions[0], "A");
        assert!(!draft.is_complete(&request));
        draft.choose(&request.questions[1], "B");
        assert!(draft.is_complete(&request));
    }

    #[test]
    fn the_encoding_is_the_same_for_both_flows() {
        let request = plan_review();
        let mut draft = Draft::new();
        draft.choose(&request.questions[0], "Approve");
        let encoded = draft.encode(&request);
        assert_eq!(encoded["answers"][0]["id"], "q1");
        assert_eq!(encoded["answers"][0]["selected"], serde_json::json!(["Approve"]));
        // No custom text means no key, not an empty one.
        assert!(encoded["answers"][0].get("custom").is_none());
    }

    #[test]
    fn multi_select_answers_carry_selections_and_custom_text_together() {
        let request = request(serde_json::json!({ "questions": [{
            "id": "q1", "question": "?", "multiSelect": true,
            "options": [{ "label": "A" }, { "label": "B" }]
        }] }));
        let mut draft = Draft::new();
        draft.choose(&request.questions[0], "A");
        draft.set_custom("q1", "and this");
        let encoded = draft.encode(&request);
        assert_eq!(encoded["answers"][0]["selected"], serde_json::json!(["A"]));
        assert_eq!(encoded["answers"][0]["custom"], "and this");
    }
}
