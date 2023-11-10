// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{AccessVector, ObjectClass, SecurityContext, SecurityId};
use starnix_lock::Mutex;
use std::{collections::HashMap, sync::Arc};

/// Specifies whether the implementation should be fully functional, or provide
/// only hard-coded fake information.
#[derive(Copy, Clone, Debug)]
pub enum Mode {
    Enable,
    Fake,
}

/// Errors that may be returned when attempting to load a new policy.
#[derive(Copy, Clone, Debug)]
pub enum PolicyError {
    Invalid,
}

pub struct SecurityServerState {
    // TODO(http://b/308175643): reference count SIDs, so that when the last SELinux object
    // referencing a SID gets destroyed, the entry is removed from the map.
    sids: HashMap<SecurityId, SecurityContext>,

    // TODO(https://b/304734769): Replace this with the parsed policy state.
    binary_policy: Vec<u8>,
}

pub struct SecurityServer {
    /// Determines whether the security server is enabled, or only provides
    /// a hard-coded set of fake responses.
    mode: Mode,

    /// The mutable state of the security server.
    state: Mutex<SecurityServerState>,
}

impl SecurityServer {
    pub fn new(mode: Mode) -> Arc<SecurityServer> {
        // TODO(http://b/304732283): initialize the access vector cache.
        let state =
            Mutex::new(SecurityServerState { sids: HashMap::new(), binary_policy: Vec::new() });
        Arc::new(SecurityServer { mode, state })
    }

    /// Returns the security ID mapped to `security_context`, creating it if it does not exist.
    ///
    /// All objects with the same security context will have the same SID associated.
    pub fn security_context_to_sid(&self, security_context: &SecurityContext) -> SecurityId {
        let mut state = self.state.lock();
        let existing_sid =
            state.sids.iter().find(|(_, sc)| sc == &security_context).map(|(sid, _)| *sid);
        existing_sid.unwrap_or_else(|| {
            // Create and insert a new SID for `security_context`.
            let sid = SecurityId::from(state.sids.len() as u64);
            if state.sids.insert(sid, security_context.clone()).is_some() {
                panic!("impossible error: SID already exists.");
            }
            sid
        })
    }

    /// Returns the security context mapped to `sid`.
    pub fn sid_to_security_context(&self, sid: &SecurityId) -> Option<SecurityContext> {
        self.state.lock().sids.get(sid).map(Clone::clone)
    }

    /// Applies the supplied policy to the security server.
    pub fn load_policy(&self, binary_policy: Vec<u8>) -> Result<(), PolicyError> {
        // TODO(https://b/304734769): Parse the supplied policy, including
        // creating any newly-required SID mappings, and stash the resulting
        // structure instead.
        self.state.lock().binary_policy = binary_policy;
        Ok(())
    }

    /// Returns the active policy in binary form.
    pub fn get_binary_policy(&self) -> Vec<u8> {
        self.state.lock().binary_policy.clone()
    }

    pub fn compute_access_vector(
        &self,
        _source_sid: SecurityId,
        _target_sid: SecurityId,
        _target_class: ObjectClass,
    ) -> AccessVector {
        // TODO(http://b/305722921): implement access decision logic. For now, the security server
        // allows all permissions.
        AccessVector::ALL
    }

    /// Returns true if the `SecurityServer` is using hard-code fake policy.
    pub fn is_fake(&self) -> bool {
        match self.mode {
            Mode::Fake => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn sid_to_security_context() {
        let security_context = SecurityContext::from("u:unconfined_r:unconfined_t");
        let security_server = SecurityServer::new(Mode::Enable);
        let sid = security_server.security_context_to_sid(&security_context);
        assert_eq!(
            security_server.sid_to_security_context(&sid).expect("sid not found"),
            security_context
        );
    }

    #[fuchsia::test]
    fn sids_for_different_security_contexts_differ() {
        let security_context1 = SecurityContext::from("u:object_r:file_t");
        let security_context2 = SecurityContext::from("u:unconfined_r:unconfined_t");
        let security_server = SecurityServer::new(Mode::Enable);
        let sid1 = security_server.security_context_to_sid(&security_context1);
        let sid2 = security_server.security_context_to_sid(&security_context2);
        assert_ne!(sid1, sid2);
    }

    #[fuchsia::test]
    fn sids_for_same_security_context_are_equal() {
        let security_context_str = "u:unconfined_r:unconfined_t";
        let security_context1 = SecurityContext::from(security_context_str);
        let security_context2 = SecurityContext::from(security_context_str);
        let security_server = SecurityServer::new(Mode::Enable);
        let sid1 = security_server.security_context_to_sid(&security_context1);
        let sid2 = security_server.security_context_to_sid(&security_context2);
        assert_eq!(sid1, sid2);
        assert_eq!(security_server.state.lock().sids.len(), 1);
    }

    #[fuchsia::test]
    fn compute_access_vector_allows_all() {
        let security_context1 = SecurityContext::from("u:object_r:file_t");
        let security_context2 = SecurityContext::from("u:unconfined_r:unconfined_t");
        let security_server = SecurityServer::new(Mode::Enable);
        let sid1 = security_server.security_context_to_sid(&security_context1);
        let sid2 = security_server.security_context_to_sid(&security_context2);
        assert_eq!(
            security_server.compute_access_vector(sid1, sid2, ObjectClass::Process),
            AccessVector::ALL
        );
    }

    #[fuchsia::test]
    fn fake_security_server_is_fake() {
        let security_server = SecurityServer::new(Mode::Enable);
        assert_eq!(security_server.is_fake(), false);

        let fake_security_server = SecurityServer::new(Mode::Fake);
        assert_eq!(fake_security_server.is_fake(), true);
    }

    #[fuchsia::test]
    fn loaded_policy_can_be_retrieved() {
        let not_really_a_policy = "not a real policy".as_bytes().to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        assert!(security_server.load_policy(not_really_a_policy.clone()).is_ok());
        assert_eq!(security_server.get_binary_policy(), not_really_a_policy);
    }
}
