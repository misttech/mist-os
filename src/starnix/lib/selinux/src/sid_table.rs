// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::policy::parser::ByValue;
use crate::policy::{Policy, SecurityContext};
use crate::{InitialSid, SecurityId, FIRST_UNUSED_SID};

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

/// Maintains the mapping between [`SecurityId`]s and [`SecurityContext`]s. Grows with use:
/// `SecurityId`s are minted when code asks a `SidTable` for the `SecurityId` associated
/// with a given `SecurityContext` and the `SidTable` doesn't happen to have seen that
/// `SecurityContext` before.
pub struct SidTable {
    /// The policy associated with this [`SidTable`].
    policy: Arc<Policy<ByValue<Vec<u8>>>>,

    /// Contexts keyed by SIDs.
    context_by_sid: HashMap<SecurityId, SecurityContext>,

    /// The next integer available to be used for a new SID.
    next_sid: NonZeroU32,
}

impl SidTable {
    pub fn new(policy: Arc<Policy<ByValue<Vec<u8>>>>) -> Self {
        Self::new_from(policy, NonZeroU32::new(FIRST_UNUSED_SID).unwrap(), HashMap::new())
    }

    pub fn new_from_previous(policy: Arc<Policy<ByValue<Vec<u8>>>>, previous: &Self) -> Self {
        let mut new_context_by_sid = HashMap::with_capacity(previous.context_by_sid.len());
        // Remap any existing Security Contexts to use Ids defined by the new policy.
        // TODO(b/330677360): Replace serialize/parse with an efficient implementation.
        for (sid, previous_context) in &previous.context_by_sid {
            let context_str = previous.policy.serialize_security_context(&previous_context);
            let new_context = policy.parse_security_context(context_str.as_slice().into());
            if let Some(new_context) = new_context.ok() {
                new_context_by_sid.insert(*sid, new_context);
            } else {
                // TODO: b/366925222 - stash the previous_context as a plain string.
            }
        }
        Self::new_from(policy, previous.next_sid, new_context_by_sid)
    }

    /// Looks up `security_context`, adding it if not found, and returns the SID.
    pub fn security_context_to_sid(&mut self, security_context: &SecurityContext) -> SecurityId {
        match self.context_by_sid.iter().find(|(_, sc)| **sc == *security_context) {
            Some((sid, _)) => *sid,
            None => {
                // Create and insert a new SID for `security_context`.
                let sid = SecurityId(self.next_sid);
                self.next_sid = increment_sid_integer(self.next_sid);
                assert!(
                    self.context_by_sid.insert(sid, security_context.clone()).is_none(),
                    "SID already exists."
                );
                sid
            }
        }
    }

    /// Returns the `SecurityContext` associated with `sid`.
    /// If `sid` was invalidated by a policy reload then the "unlabeled"
    /// context is returned instead.
    pub fn sid_to_security_context(&self, sid: SecurityId) -> &SecurityContext {
        self.context_by_sid.get(&sid).unwrap_or_else(|| {
            self.context_by_sid.get(&SecurityId::initial(InitialSid::Unlabeled)).unwrap()
        })
    }

    /// Returns the `SecurityContext` associated with `sid`, unless `sid` was invalidated by a
    /// policy reload. Query implementations should use `sid_to_security_context()`.
    pub fn try_sid_to_security_context(&self, sid: SecurityId) -> Option<&SecurityContext> {
        self.context_by_sid.get(&sid)
    }

    fn new_from(
        policy: Arc<Policy<ByValue<Vec<u8>>>>,
        next_sid: NonZeroU32,
        mut new_context_by_sid: HashMap<SecurityId, SecurityContext>,
    ) -> Self {
        // Replace the "initial" SID's associated Contexts.
        for id in InitialSid::all_variants() {
            let security_context = policy.initial_context(id);
            new_context_by_sid.insert(SecurityId::initial(id), security_context);
        }

        SidTable { policy, context_by_sid: new_context_by_sid, next_sid }
    }
}

fn increment_sid_integer(sid_integer: NonZeroU32) -> NonZeroU32 {
    sid_integer.checked_add(1).expect("exhausted SID namespace")
}

/// Test utilities shared `SidTable` users.
#[cfg(test)]
pub(crate) mod testing {
    use super::increment_sid_integer;
    use crate::sid_table::SidTable;
    use crate::SecurityId;

    /// Returns the first-unused (or next-to-be-allocated) SID integer.
    pub(crate) fn allocate_sid(sid_table: &mut SidTable) -> SecurityId {
        let sid_integer = sid_table.next_sid;
        sid_table.next_sid = increment_sid_integer(sid_integer);
        SecurityId(sid_integer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::policy::parse_policy_by_value;

    const TESTS_BINARY_POLICY: &[u8] =
        include_bytes!("../testdata/micro_policies/security_server_tests_policy.pp");

    fn test_policy() -> Arc<Policy<ByValue<Vec<u8>>>> {
        let (unvalidated, _binary) = parse_policy_by_value(TESTS_BINARY_POLICY.to_vec()).unwrap();
        Arc::new(unvalidated.validate().unwrap())
    }

    #[test]
    fn sid_to_security_context() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"unconfined_u:unconfined_r:unconfined_t:s0".into())
            .unwrap();
        let mut sid_table = SidTable::new(policy);
        let sid = sid_table.security_context_to_sid(&security_context);
        assert_eq!(*sid_table.sid_to_security_context(sid), security_context);
    }

    #[test]
    fn sids_for_different_security_contexts_differ() {
        let policy = test_policy();
        let mut sid_table = SidTable::new(policy.clone());
        let sid1 = sid_table.security_context_to_sid(
            &policy.parse_security_context(b"user0:object_r:type0:s0".into()).unwrap(),
        );
        let sid2 = sid_table.security_context_to_sid(
            &policy
                .parse_security_context(b"unconfined_u:unconfined_r:unconfined_t:s0".into())
                .unwrap(),
        );
        assert_ne!(sid1, sid2);
    }

    #[test]
    fn sids_for_same_security_context_are_equal() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"unconfined_u:unconfined_r:unconfined_t:s0".into())
            .unwrap();
        let mut sid_table = SidTable::new(policy);
        let sid_count_before = sid_table.context_by_sid.len();
        let sid1 = sid_table.security_context_to_sid(&security_context);
        let sid2 = sid_table.security_context_to_sid(&security_context);
        assert_eq!(sid1, sid2);
        assert_eq!(sid_table.context_by_sid.len(), sid_count_before + 1);
    }

    #[test]
    fn sids_allocated_outside_initial_range() {
        let policy = test_policy();
        let security_context = policy
            .parse_security_context(b"unconfined_u:unconfined_r:unconfined_t:s0".into())
            .unwrap();
        let mut sid_table = SidTable::new(policy);
        let sid_count_before = sid_table.context_by_sid.len();
        let sid = sid_table.security_context_to_sid(&security_context);
        assert_eq!(sid_table.context_by_sid.len(), sid_count_before + 1);
        assert!(sid.0.get() >= FIRST_UNUSED_SID);
    }
}
