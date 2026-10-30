//! Which sync account a profile belongs to (roadmap milestone 2).
//!
//! The backend has no identity endpoint, so the account carries a marker record under the
//! non-task key `account`, holding a random ID. A profile adopts the marker only after a full
//! replay with its original, already-verified credentials, and records the ID. From then on a
//! different token is accepted only if the marker it can read carries the same ID, which allows
//! token rotation and several device tokens without ever uploading into the wrong account.
//! A missing, deleted or different marker pauses uploads instead of rebinding.
//!
//! Clients from before the compatibility release cannot read non-task records, so this relies on
//! the rollout rule in milestone 0b: every device must run a compatible build first.
use crate::{Opaque, Result, Store};
use serde_json::{Value, json};

pub const MARKER_KEY: &str = "account";
const MARKER_TYPE: &str = "nothingtodo-account";

pub(crate) fn marker_value(id: &str) -> Value {
    json!({ "type": MARKER_TYPE, "id": id })
}

/// The account ID in a marker record, if it is a live, well-formed marker.
pub(crate) fn marker_id(record: Option<&Opaque>) -> Option<String> {
    let value = record?.value.as_ref()?;
    (value.get("type")?.as_str()? == MARKER_TYPE)
        .then(|| value.get("id")?.as_str().map(String::from))
        .flatten()
        .filter(|id| !id.is_empty())
}

const MISMATCH: &str = "The sync account's identity marker is missing or differs from this profile's, so uploads are paused to avoid writing into the wrong account. Reconnect with the account this profile came from, or copy your tasks into a new profile from a backup.";

pub(crate) enum Next {
    /// The account is confirmed; uploads may go ahead.
    Ready,
    /// No marker exists yet and this profile is verified by its original credentials: create one.
    Create,
}

impl Store {
    /// The account ID this profile has recorded, if any.
    pub fn account(&self) -> Option<&str> {
        self.state.account.as_deref()
    }

    /// Runs after a full replay with verified credentials, before any upload.
    pub(crate) fn check_account(&mut self) -> Result<Next> {
        let marker = marker_id(self.state.foreign.get(MARKER_KEY));
        match (self.state.account.clone(), marker) {
            (Some(expected), Some(found)) if expected == found => Ok(Next::Ready),
            (Some(_), _) => Err(MISMATCH.into()),
            (None, Some(found)) => {
                let mut state = self.snapshot();
                state.account = Some(found);
                self.commit(state)?;
                Ok(Next::Ready)
            }
            (None, None) => Ok(Next::Create),
        }
    }

    /// Accepts a new token or server for this profile once its marker matched the recorded
    /// account. Nothing from the new connection has been applied before this.
    pub(crate) fn rebind(&mut self, binding: &str, marker: Option<String>) -> Result<()> {
        if self.state.account.is_none() {
            return Err("This profile is linked to another token or server and hasn't recorded its account yet. Connect once with the original token; after that, other tokens for the same account work.".into());
        }
        if marker.as_deref() != self.state.account.as_deref() {
            return Err("This token belongs to a different account, or the account has no identity marker. Nothing was downloaded or uploaded.".into());
        }
        let mut state = self.snapshot();
        state.binding = Some(binding.into());
        self.commit(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(account: Option<&str>, marker: Option<Value>) -> Store {
        let mut store = Store::open(":memory:").unwrap();
        let mut state = store.snapshot();
        state.account = account.map(String::from);
        if let Some(value) = marker {
            state.foreign.insert(
                MARKER_KEY.into(),
                Opaque {
                    value: Some(value).filter(|v| !v.is_null()),
                    version: 1,
                },
            );
        }
        store.commit(state).unwrap();
        store
    }

    #[test]
    fn adopts_an_existing_marker_or_asks_to_create_one() {
        let mut store = store_with(None, Some(marker_value("a1")));
        assert!(matches!(store.check_account(), Ok(Next::Ready)));
        assert_eq!(store.account(), Some("a1"));
        let mut fresh = store_with(None, None);
        assert!(matches!(fresh.check_account(), Ok(Next::Create)));
        assert_eq!(fresh.account(), None);
    }

    #[test]
    fn missing_deleted_changed_or_malformed_markers_pause_uploads() {
        for marker in [
            None,
            Some(Value::Null),
            Some(marker_value("other")),
            Some(json!({ "type": "something-else", "id": "a1" })),
        ] {
            assert!(store_with(Some("a1"), marker).check_account().is_err());
        }
        assert!(matches!(
            store_with(Some("a1"), Some(marker_value("a1"))).check_account(),
            Ok(Next::Ready)
        ));
    }

    #[test]
    fn a_new_token_is_accepted_only_for_the_recorded_account() {
        let mut store = store_with(Some("a1"), Some(marker_value("a1")));
        assert!(store.rebind("new", Some("other".into())).is_err());
        assert!(store.rebind("new", None).is_err());
        store.rebind("new", Some("a1".into())).unwrap();
        assert_eq!(store.snapshot().binding.as_deref(), Some("new"));
        // A profile that never recorded an account cannot change tokens at all.
        assert!(
            store_with(None, None)
                .rebind("new", Some("a1".into()))
                .is_err()
        );
    }
}
