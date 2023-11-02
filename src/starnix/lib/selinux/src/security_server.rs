// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{SecurityContext, SecurityId};
use starnix_lock::RwLock;
use std::collections::HashMap;

pub struct SecurityServerState {
    // TODO(http://b/308175643): reference count SIDs, so that when the last SELinux object
    // referencing a SID gets destroyed, the entry is removed from the map.
    sids: HashMap<SecurityId, SecurityContext>,
}

pub struct SecurityServer {
    /// The mutable state of the security server.
    state: RwLock<SecurityServerState>,
}

impl SecurityServer {
    pub fn new() -> SecurityServer {
        // TODO(http://b/304732283): initialize the access vector cache.
        SecurityServer { state: RwLock::new(SecurityServerState { sids: HashMap::new() }) }
    }

    /// Creates a security ID for a given `security_context`.
    ///
    /// SIDs are assigned incremental values. All objects belonging to a security context will have
    /// the same SID associated.
    pub fn create_sid(&self, security_context: &SecurityContext) -> SecurityId {
        let mut state = self.state.write();
        let existing_sid =
            state.sids.iter().find(|(_, sc)| sc == &security_context).map(|(sid, _)| *sid);
        existing_sid.map_or_else(
            || {
                // Create and insert a new SID for `security_context`.
                let sid = SecurityId::from(state.sids.len() as u64);
                if state.sids.insert(sid, security_context.clone()).is_some() {
                    panic!("impossible error: SID already exists.");
                }
                sid
            },
            // Return the SID associated with `security_context`.
            |sid| sid,
        )
    }

    /// Returns the security context mapped to `sid`.
    pub fn sid_to_security_context(&self, sid: &SecurityId) -> Option<SecurityContext> {
        self.state.read().sids.get(sid).map(Clone::clone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn sid_to_security_context() {
        let security_context = SecurityContext::from("u:unconfined_r:unconfined_t");
        let security_server = SecurityServer::new();
        let sid = security_server.create_sid(&security_context);
        assert_eq!(
            security_server.sid_to_security_context(&sid).expect("sid not found"),
            security_context
        );
    }

    #[fuchsia::test]
    fn sids_for_different_security_contexts_differ() {
        let security_context1 = SecurityContext::from("u:object_r:file_t");
        let security_context2 = SecurityContext::from("u:unconfined_r:unconfined_t");
        let security_server = SecurityServer::new();
        let sid1 = security_server.create_sid(&security_context1);
        let sid2 = security_server.create_sid(&security_context2);
        assert_ne!(sid1, sid2);
    }

    #[fuchsia::test]
    fn sids_for_same_security_context_are_equal() {
        let security_context_str = "u:unconfined_r:unconfined_t";
        let security_context1 = SecurityContext::from(security_context_str);
        let security_context2 = SecurityContext::from(security_context_str);
        let security_server = SecurityServer::new();
        let sid1 = security_server.create_sid(&security_context1);
        let sid2 = security_server.create_sid(&security_context2);
        assert_eq!(sid1, sid2);
        assert_eq!(security_server.state.read().sids.len(), 1);
    }
}
