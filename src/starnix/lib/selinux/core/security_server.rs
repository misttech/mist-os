// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    access_vector_cache::{Manager as AvcManager, Query, QueryMut},
    security_context::SecurityContext,
    AccessVector, ObjectClass, SecurityId,
};

use anyhow;
use selinux_policy::{metadata::HandleUnknown, parse_policy_by_value, parser::ByValue, Policy};
use starnix_sync::Mutex;
use std::{collections::HashMap, sync::Arc};

/// Specifies whether the implementation should be fully functional, or provide
/// only hard-coded fake information.
#[derive(Copy, Clone, Debug)]
pub enum Mode {
    Enable,
    Fake,
}

struct LoadedPolicy {
    /// Parsed policy structure.
    parsed: Policy<ByValue<Vec<u8>>>,

    /// The binary policy that was previously passed to `load_policy()`.
    binary: Vec<u8>,
}

struct SecurityServerState {
    // TODO(http://b/308175643): reference count SIDs, so that when the last SELinux object
    // referencing a SID gets destroyed, the entry is removed from the map.
    sids: HashMap<SecurityId, SecurityContext>,

    /// Describes the currently active policy.
    policy: Option<Arc<LoadedPolicy>>,

    /// True if hooks should enforce policy-based access decisions.
    enforcing: bool,
}

pub struct SecurityServer {
    /// Determines whether the security server is enabled, or only provides
    /// a hard-coded set of fake responses.
    mode: Mode,

    /// Manager for any access vector cache layers that are shared between threads subject to access
    /// control by this security server. This [`AvcManager`] is also responsible for constructing
    /// thread-local caches for use by individual threads that subject to access control by this
    /// security server.
    avc_manager: AvcManager<SecurityServer>,

    /// The mutable state of the security server.
    state: Mutex<SecurityServerState>,
}

impl SecurityServer {
    pub fn new(mode: Mode) -> Arc<SecurityServer> {
        let avc_manager = AvcManager::new();
        let state = Mutex::new(SecurityServerState {
            sids: HashMap::new(),
            policy: None,
            enforcing: false,
        });
        let security_server = Arc::new(SecurityServer { mode, avc_manager, state });

        // TODO(http://b/304776236): Consider constructing shared owner of `AvcManager` and
        // `SecurityServer` to eliminate weak reference.
        security_server.as_ref().avc_manager.set_security_server(Arc::downgrade(&security_server));

        security_server
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
    pub fn load_policy(&self, binary_policy: Vec<u8>) -> Result<(), anyhow::Error> {
        // TODO(b/318647906, b/319442240): Re-enabled policy parse-and-validate after investigating
        // flake in affected tests.
        if false {
            // Parse the supplied policy, and reject the load operation if it is
            // malformed or invalid.
            let (parsed, binary) = parse_policy_by_value(binary_policy)?;
            let parsed = parsed.validate()?;

            // Bundle the binary policy together with a parsed copy for the
            // [`SecurityServer`] to use to answer queries. This will fail if the
            // supplied policy cannot be parsed due to being malformed, or if the
            // parsed policy is not valid.
            let policy = Arc::new(LoadedPolicy { parsed, binary });

            // Replace any existing policy.
            // TODO(b/315531456): Update the policy load count for "status".
            self.state.lock().policy = Some(policy);
        }

        Ok(())
    }

    /// Set to enforcing mode if `enforce` is true, permissive mode otherwise.
    pub fn set_enforcing(&self, enforcing: bool) {
        self.state.lock().enforcing = enforcing;
    }

    /// Returns true if access decisions by the [`SecurityServer`] should be
    /// enforced by hooks.
    pub fn enforcing(&self) -> bool {
        self.state.lock().enforcing
    }

    /// Returns the active policy in binary form.
    pub fn get_binary_policy(&self) -> Vec<u8> {
        self.state.lock().policy.as_ref().map_or(Vec::new(), |p| p.binary.clone())
    }

    /// Returns the behaviour of unknown object class / permissions, according
    /// to the loaded policy. If no policy is loaded then unknown interactions
    /// are allowed.
    pub fn handle_unknown(&self) -> HandleUnknown {
        if self.is_fake() {
            return HandleUnknown::Allow;
        }
        match self.state.lock().policy.as_ref() {
            Some(policy) => policy.parsed.handle_unknown().clone(),
            None => HandleUnknown::Allow,
        }
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

    /// Returns a reference to the shared access vector cache that delebates cache misses to `self`.
    pub fn get_shared_avc(&self) -> impl Query {
        self.avc_manager.get_shared_cache()
    }

    /// Returns a newly constructed thread-local access vector cache that delegates cache misses to
    /// any shared caches owned by `self.avc_manager`, which ultimately delegate to `self`. The
    /// returned cache will be reset when this security server's policy is reset.
    pub fn new_thread_local_avc(&self) -> impl QueryMut {
        self.avc_manager.new_thread_local_cache()
    }
}

impl Query for SecurityServer {
    fn query(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> AccessVector {
        self.compute_access_vector(source_sid, target_sid, target_class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_BINARY_POLICY: &[u8] = include_bytes!("../testdata/policies/minimal");

    #[fuchsia::test]
    fn sid_to_security_context() {
        let security_context = SecurityContext::try_from("u:unconfined_r:unconfined_t")
            .expect("creating security context should succeed");
        let security_server = SecurityServer::new(Mode::Enable);
        let sid = security_server.security_context_to_sid(&security_context);
        assert_eq!(
            security_server.sid_to_security_context(&sid).expect("sid not found"),
            security_context
        );
    }

    #[fuchsia::test]
    fn sids_for_different_security_contexts_differ() {
        let security_context1 = SecurityContext::try_from("u:object_r:file_t")
            .expect("creating security context should succeed");
        let security_context2 = SecurityContext::try_from("u:unconfined_r:unconfined_t")
            .expect("creating security context should succeed");
        let security_server = SecurityServer::new(Mode::Enable);
        let sid1 = security_server.security_context_to_sid(&security_context1);
        let sid2 = security_server.security_context_to_sid(&security_context2);
        assert_ne!(sid1, sid2);
    }

    #[fuchsia::test]
    fn sids_for_same_security_context_are_equal() {
        let security_context_str = "u:unconfined_r:unconfined_t";
        let security_context1 = SecurityContext::try_from(security_context_str)
            .expect("creating security context should succeed");
        let security_context2 = SecurityContext::try_from(security_context_str)
            .expect("creating security context should succeed");
        let security_server = SecurityServer::new(Mode::Enable);
        let sid1 = security_server.security_context_to_sid(&security_context1);
        let sid2 = security_server.security_context_to_sid(&security_context2);
        assert_eq!(sid1, sid2);
        assert_eq!(security_server.state.lock().sids.len(), 1);
    }

    #[fuchsia::test]
    fn compute_access_vector_allows_all() {
        let security_context1 = SecurityContext::try_from("u:object_r:file_t")
            .expect("creating security context should succeed");
        let security_context2 = SecurityContext::try_from("u:unconfined_r:unconfined_t")
            .expect("creating security context should succeed");
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

    // TODO(b/318647906, b/319442240): Re-enabled policy parse-and-validate after investigating
    // flake in affected tests.
    #[ignore]
    #[fuchsia::test]
    fn loaded_policy_can_be_retrieved() {
        let policy_bytes = MINIMAL_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        assert!(security_server.load_policy(policy_bytes).is_ok());
        assert_eq!(MINIMAL_BINARY_POLICY, security_server.get_binary_policy().as_slice());
    }

    // TODO(b/318647906, b/319442240): Re-enabled policy parse-and-validate after investigating
    // flake in affected tests.
    #[ignore]
    #[fuchsia::test]
    fn loaded_policy_is_validated() {
        let not_really_a_policy = "not a real policy".as_bytes().to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        assert!(security_server.load_policy(not_really_a_policy.clone()).is_err());
    }

    #[fuchsia::test]
    fn enforcing_mode_is_reported() {
        let security_server = SecurityServer::new(Mode::Enable);
        assert!(!security_server.enforcing());

        security_server.set_enforcing(true);
        assert!(security_server.enforcing());
    }
}
