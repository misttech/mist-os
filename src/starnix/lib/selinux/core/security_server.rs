// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    access_vector_cache::{Manager as AvcManager, Query, QueryMut},
    permission_check::{PermissionCheck, PermissionCheckImpl},
    seq_lock::SeqLock,
    SecurityId,
};

use anyhow::Context as _;
use fuchsia_zircon::{self as zx};
use selinux_common::{
    AbstractObjectClass, ClassPermission, FileClass, InitialSid, Permission, FIRST_UNUSED_SID,
};
use selinux_policy::{
    metadata::HandleUnknown, parse_policy_by_value, parser::ByValue, AccessVector,
    AccessVectorComputer, Policy, SecurityContext,
};
use starnix_sync::Mutex;
use std::{collections::HashMap, num::NonZeroU32, ops::DerefMut, sync::Arc};
use zerocopy::{AsBytes, NoCell};

/// The version of the SELinux "status" file this implementation implements.
const SELINUX_STATUS_VERSION: u32 = 1;

/// Specifies whether the implementation should be fully functional, or provide
/// only hard-coded fake information.
#[derive(Copy, Clone, Debug)]
pub enum Mode {
    Enable,
    Fake,
}

pub trait SecurityServerStatus {
    /// Returns true if access decisions by the security server should be enforced by hooks.
    fn is_enforcing(&self) -> bool;

    /// Returns true if the security server is using a hard-coded fake policy.
    fn is_fake(&self) -> bool;
}

struct LoadedPolicy {
    /// Parsed policy structure.
    parsed: Policy<ByValue<Vec<u8>>>,

    /// The binary policy that was previously passed to `load_policy()`.
    binary: Vec<u8>,
}

type SeLinuxStatus = SeqLock<SeLinuxStatusHeader, SeLinuxStatusValue>;

#[derive(Default)]
struct SeLinuxBooleans {
    /// Active values for all of the booleans defined by the policy.
    /// Entries are created at policy load for each policy-defined conditional.
    active: HashMap<String, bool>,
    /// Pending values for any booleans modified since the last commit.
    pending: HashMap<String, bool>,
}

impl SeLinuxBooleans {
    fn reset(&mut self, booleans: Vec<(String, bool)>) {
        self.active = HashMap::from_iter(booleans);
        self.pending.clear();
    }
    fn names(&mut self) -> Vec<String> {
        self.active.keys().cloned().collect()
    }
    fn set_pending(&mut self, name: &str, value: bool) -> Result<(), ()> {
        if !self.active.contains_key(name) {
            return Err(());
        }
        self.pending.insert(name.into(), value);
        Ok(())
    }
    fn get(&mut self, name: &str) -> Result<(bool, bool), ()> {
        let active = self.active.get(name).ok_or(())?;
        let pending = self.pending.get(name).unwrap_or(active);
        Ok((*active, *pending))
    }
    fn commit_pending(&mut self) {
        self.active.extend(self.pending.drain());
    }
}

struct SecurityServerState {
    /// Cache of SecurityIds (SIDs) used to refer to Security Contexts.
    // TODO(http://b/308175643): reference count SIDs, so that when the last SELinux object
    // referencing a SID gets destroyed, the entry is removed from the map.
    sids: HashMap<SecurityId, SecurityContext>,

    /// Identifier to allocate to the next new Security Context.
    next_sid: NonZeroU32,

    /// Describes the currently active policy.
    policy: Option<Arc<LoadedPolicy>>,

    /// Holds active and pending states for each boolean defined by policy.
    booleans: SeLinuxBooleans,

    /// Encapsulates the security server "status" fields, which are exposed to
    /// userspace as a C-layout structure, and updated with the SeqLock pattern.
    status: SeLinuxStatus,

    /// True if hooks should enforce policy-based access decisions.
    enforcing: bool,

    /// Count of changes to the active policy.  Changes include both loads
    /// of complete new policies, and modifications to a previously loaded
    /// policy, e.g. by committing new values to conditional booleans in it.
    policy_change_count: u32,
}

impl SecurityServerState {
    fn security_context_to_sid(&mut self, security_context: SecurityContext) -> SecurityId {
        match self.sids.iter().find(|(_, sc)| **sc == security_context) {
            Some((sid, _)) => *sid,
            None => {
                // Create and insert a new SID for `security_context`.
                let sid = SecurityId(self.next_sid);
                self.next_sid = self.next_sid.checked_add(1).expect("exhausted SID namespace");
                assert!(
                    self.sids.insert(sid, security_context).is_none(),
                    "impossible error: SID already exists."
                );
                sid
            }
        }
    }
    fn sid_to_security_context(&self, sid: SecurityId) -> Option<&SecurityContext> {
        self.sids.get(&sid)
    }
    fn deny_unknown(&self) -> bool {
        self.policy.as_ref().map_or(true, |p| *p.parsed.handle_unknown() != HandleUnknown::Allow)
    }
    fn reject_unknown(&self) -> bool {
        self.policy.as_ref().map_or(false, |p| *p.parsed.handle_unknown() == HandleUnknown::Reject)
    }
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
    pub fn new(mode: Mode) -> Arc<Self> {
        let avc_manager = AvcManager::new();
        let status = SeLinuxStatus::new_default().expect("Failed to create SeLinuxStatus");
        let state = Mutex::new(SecurityServerState {
            sids: HashMap::new(),
            next_sid: NonZeroU32::new(FIRST_UNUSED_SID).unwrap(),
            policy: None,
            booleans: SeLinuxBooleans::default(),
            status,
            enforcing: false,
            policy_change_count: 0,
        });

        let security_server = Arc::new(Self { mode, avc_manager, state });

        // TODO(http://b/304776236): Consider constructing shared owner of `AvcManager` and
        // `SecurityServer` to eliminate weak reference.
        security_server.as_ref().avc_manager.set_security_server(Arc::downgrade(&security_server));

        security_server
    }

    /// Converts a shared pointer to [`SecurityServer`] to a [`PermissionCheck`] without consuming
    /// the pointer.
    pub fn as_permission_check<'a>(self: &'a Arc<Self>) -> impl PermissionCheck + 'a {
        PermissionCheckImpl::new(self, self.avc_manager.get_shared_cache())
    }

    /// Returns the security ID mapped to `security_context`, creating it if it does not exist.
    ///
    /// All objects with the same security context will have the same SID associated.
    pub fn security_context_to_sid(
        &self,
        security_context: &[u8],
    ) -> Result<SecurityId, anyhow::Error> {
        let mut state = self.state.lock();
        let policy = &state.policy.as_ref().ok_or(anyhow::anyhow!("no policy loaded"))?.parsed;
        let context =
            policy.parse_security_context(security_context).map_err(anyhow::Error::from)?;
        Ok(state.security_context_to_sid(context))
    }

    /// Returns the Security Context string for the requested `sid`.
    /// This is used only where Contexts need to be stringified to expose to userspace, as
    /// is the case for e.g. the `/proc/*/attr/` filesystem.
    pub fn sid_to_security_context(&self, sid: SecurityId) -> Option<Vec<u8>> {
        let state = self.state.lock();
        let context = state.sid_to_security_context(sid)?;
        Some(state.policy.as_ref()?.parsed.serialize_security_context(context))
    }

    /// Applies the supplied policy to the security server.
    pub fn load_policy(&self, binary_policy: Vec<u8>) -> Result<(), anyhow::Error> {
        // Parse the supplied policy, and reject the load operation if it is
        // malformed or invalid.
        let (parsed, binary) = parse_policy_by_value(binary_policy)?;
        let parsed = parsed.validate()?;

        // Bundle the binary policy together with a parsed copy for the
        // [`SecurityServer`] to use to answer queries. This will fail if the
        // supplied policy cannot be parsed due to being malformed, or if the
        // parsed policy is not valid.
        let policy = Arc::new(LoadedPolicy { parsed, binary });

        // Fetch the initial Security Contexts used by this implementation from
        // the new policy, ready to swap into the SID table.
        let mut initial_contexts = HashMap::<SecurityId, SecurityContext>::new();
        for id in InitialSid::all_variants() {
            let security_context = policy.parsed.initial_context(id);
            initial_contexts.insert(SecurityId::initial(id), security_context);
        }

        // Replace any existing policy and update the [`SeLinuxStatus`].
        self.with_state_and_update_status(|state| {
            // Replace the Contexts associated with initial SIDs used by this implementation.
            state.sids.extend(initial_contexts.drain());

            // TODO(b/324265752): Determine whether SELinux booleans need to be retained across
            // policy (re)loads.
            state.booleans.reset(
                policy
                    .parsed
                    .conditional_booleans()
                    .iter()
                    // TODO(b/324392507): Relax the UTF8 requirement on policy strings.
                    .map(|(name, value)| (String::from_utf8((*name).to_vec()).unwrap(), *value))
                    .collect(),
            );

            state.policy = Some(policy);
            state.policy_change_count += 1;
        });

        Ok(())
    }

    /// Returns the active policy in binary form.
    pub fn get_binary_policy(&self) -> Vec<u8> {
        self.state.lock().policy.as_ref().map_or(Vec::new(), |p| p.binary.clone())
    }

    /// Set to enforcing mode if `enforce` is true, permissive mode otherwise.
    pub fn set_enforcing(&self, enforcing: bool) {
        self.with_state_and_update_status(|state| state.enforcing = enforcing);
    }

    /// Returns true if the policy requires unknown class / permissions to be
    /// denied. Defaults to true until a policy is loaded.
    pub fn deny_unknown(&self) -> bool {
        if self.is_fake() {
            false
        } else {
            self.state.lock().deny_unknown()
        }
    }

    /// Returns true if the policy requires unknown class / permissions to be
    /// rejected. Defaults to false until a policy is loaded.
    pub fn reject_unknown(&self) -> bool {
        if self.is_fake() {
            false
        } else {
            self.state.lock().reject_unknown()
        }
    }

    /// Returns the list of names of boolean conditionals defined by the
    /// loaded policy.
    pub fn conditional_booleans(&self) -> Vec<String> {
        self.state.lock().booleans.names()
    }

    /// Returns the active and pending values of a policy boolean, if it exists.
    pub fn get_boolean(&self, name: &str) -> Result<(bool, bool), ()> {
        self.state.lock().booleans.get(name)
    }

    /// Sets the pending value of a boolean, if it is defined in the policy.
    pub fn set_pending_boolean(&self, name: &str, value: bool) -> Result<(), ()> {
        self.state.lock().booleans.set_pending(name, value)
    }

    /// Commits all pending changes to conditional booleans.
    pub fn commit_pending_booleans(&self) {
        // TODO(b/324264149): Commit values into the stored policy itself.
        self.with_state_and_update_status(|state| {
            state.booleans.commit_pending();
            state.policy_change_count += 1;
        });
    }

    /// Computes the precise access vector for `source_sid` targeting `target_sid` as class
    /// `target_class`.
    ///
    /// TODO(http://b/305722921): Implement complete access decision logic. For now, the security
    /// server abides by explicit `allow [source] [target]:[class] [permissions..];` statements.
    pub fn compute_access_vector(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: AbstractObjectClass,
    ) -> AccessVector {
        let state = self.state.lock();
        let policy = match state.policy.as_ref().map(Clone::clone) {
            Some(policy) => policy,
            // Policy is "allow all" when no policy is loaded, regardless of enforcing state.
            None => return AccessVector::ALL,
        };

        if let (Some(source_security_context), Some(target_security_context)) =
            (state.sid_to_security_context(source_sid), state.sid_to_security_context(target_sid))
        {
            // Take copies of the the type fields before dropping the state lock.
            let source_type = source_security_context.type_().clone();
            let target_type = target_security_context.type_().clone();
            drop(state);

            match target_class {
                AbstractObjectClass::System(target_class) => policy
                    .parsed
                    .compute_explicitly_allowed(&source_type, &target_type, target_class)
                    .unwrap_or(AccessVector::NONE),
                AbstractObjectClass::Custom(target_class) => policy
                    .parsed
                    .compute_explicitly_allowed_custom(&source_type, &target_type, &target_class)
                    .unwrap_or(AccessVector::NONE),
                // No meaningful policy can be determined without target class.
                _ => AccessVector::NONE,
            }
        } else {
            // No meaningful policy can be determined without source and target types.
            AccessVector::NONE
        }
    }

    /// Computes the appropriate security identifier (SID) for the security context of a file-like
    /// object of class `file_class` created by `source_sid` targeting `target_sid`.
    pub fn compute_new_file_sid(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        file_class: FileClass,
    ) -> Result<SecurityId, anyhow::Error> {
        let state = self.state.lock();
        let policy = match state.policy.as_ref().map(Clone::clone) {
            Some(policy) => policy,
            None => {
                return Err(anyhow::anyhow!("no policy loaded")).context("computing new file sid")
            }
        };

        if let (Some(source_security_context), Some(target_security_context)) = (
            state.sid_to_security_context(source_sid).cloned(),
            state.sid_to_security_context(target_sid).cloned(),
        ) {
            drop(state);

            policy
                .parsed
                .new_file_security_context(
                    &source_security_context,
                    &target_security_context,
                    &file_class,
                )
                .map(|sc| self.state.lock().security_context_to_sid(sc))
                .map_err(anyhow::Error::from)
                .context("computing new file security context from policy")
        } else {
            anyhow::bail!(
                "at least one security id, {:?} and/or {:?}, not recognized by security server",
                source_sid,
                target_sid
            )
        }
    }

    /// Returns a read-only VMO containing the SELinux "status" structure.
    pub fn get_status_vmo(&self) -> Arc<zx::Vmo> {
        self.state.lock().status.get_readonly_vmo()
    }

    /// Returns a reference to the shared access vector cache that delebates cache misses to `self`.
    pub fn get_shared_avc(&self) -> &impl Query {
        self.avc_manager.get_shared_cache()
    }

    /// Returns a newly constructed thread-local access vector cache that delegates cache misses to
    /// any shared caches owned by `self.avc_manager`, which ultimately delegate to `self`. The
    /// returned cache will be reset when this security server's policy is reset.
    pub fn new_thread_local_avc(&self) -> impl QueryMut {
        self.avc_manager.new_thread_local_cache()
    }

    // Runs the supplied function with locked `self`, and then updates the [`SeLinuxStatus`].
    fn with_state_and_update_status(&self, f: impl FnOnce(&mut SecurityServerState)) {
        let mut state = self.state.lock();
        f(state.deref_mut());
        let new_value = SeLinuxStatusValue {
            enforcing: state.enforcing as u32,
            policyload: state.policy_change_count,
            deny_unknown: if self.is_fake() { 0 } else { state.deny_unknown() as u32 },
        };
        state.status.set_value(new_value);
    }
}

impl SecurityServerStatus for SecurityServer {
    fn is_enforcing(&self) -> bool {
        self.state.lock().enforcing
    }

    fn is_fake(&self) -> bool {
        match self.mode {
            Mode::Fake => true,
            _ => false,
        }
    }
}

impl Query for SecurityServer {
    fn query(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: AbstractObjectClass,
    ) -> AccessVector {
        self.compute_access_vector(source_sid, target_sid, target_class)
    }
}

impl AccessVectorComputer for SecurityServer {
    fn access_vector_from_permission<P: ClassPermission + Into<Permission> + 'static>(
        &self,
        permission: P,
    ) -> AccessVector {
        match &self.state.lock().policy {
            Some(policy) => policy.parsed.access_vector_from_permission(permission),
            None => AccessVector::NONE,
        }
    }

    fn access_vector_from_permissions<
        P: ClassPermission + Into<Permission> + 'static,
        PI: IntoIterator<Item = P>,
    >(
        &self,
        permissions: PI,
    ) -> AccessVector {
        match &self.state.lock().policy {
            Some(policy) => policy.parsed.access_vector_from_permissions(permissions),
            None => AccessVector::NONE,
        }
    }
}

/// Header of the C-style struct exposed via the /sys/fs/selinux/status file,
/// to userspace. Defined here (instead of imported through bindgen) as selinux
/// headers are not exposed through  kernel uapi headers.
#[derive(AsBytes, Copy, Clone, NoCell)]
#[repr(C, align(4))]
struct SeLinuxStatusHeader {
    /// Version number of this structure (1).
    version: u32,
}

impl Default for SeLinuxStatusHeader {
    fn default() -> Self {
        Self { version: SELINUX_STATUS_VERSION }
    }
}

/// Value part of the C-style struct exposed via the /sys/fs/selinux/status file,
/// to userspace. Defined here (instead of imported through bindgen) as selinux
/// headers are not exposed through  kernel uapi headers.
#[derive(AsBytes, Copy, Clone, Default, NoCell)]
#[repr(C, align(4))]
struct SeLinuxStatusValue {
    /// `0` means permissive mode, `1` means enforcing mode.
    enforcing: u32,
    /// The number of times the selinux policy has been reloaded.
    policyload: u32,
    /// `0` means allow and `1` means deny unknown object classes/permissions.
    deny_unknown: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    use fuchsia_zircon::AsHandleRef as _;
    use selinux_common::ObjectClass;
    use std::mem::size_of;
    use zerocopy::{FromBytes, FromZeroes};

    const TESTSUITE_BINARY_POLICY: &[u8] = include_bytes!("../testdata/policies/selinux_testsuite");
    const EMULATOR_BINARY_POLICY: &[u8] = include_bytes!("../testdata/policies/emulator");

    fn security_server_with_emulator_policy() -> Arc<SecurityServer> {
        let policy_bytes = EMULATOR_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        assert_eq!(
            Ok(()),
            security_server.load_policy(policy_bytes).map_err(|e| format!("{:?}", e))
        );
        security_server
    }

    #[fuchsia::test]
    fn sid_to_security_context() {
        let security_context = b"u:unconfined_r:unconfined_t:s0";
        let security_server = security_server_with_emulator_policy();
        let sid = security_server
            .security_context_to_sid(security_context)
            .expect("creating SID from security context should succeed");
        assert_eq!(
            security_server.sid_to_security_context(sid).expect("sid not found"),
            security_context
        );
    }

    #[fuchsia::test]
    fn sids_for_different_security_contexts_differ() {
        let security_server = security_server_with_emulator_policy();
        let sid1 = security_server
            .security_context_to_sid(b"u:object_r:file_t:s0")
            .expect("creating SID from security context should succeed");
        let sid2 = security_server
            .security_context_to_sid(b"u:unconfined_r:unconfined_t:s0")
            .expect("creating SID from security context should succeed");
        assert_ne!(sid1, sid2);
    }

    #[fuchsia::test]
    fn sids_for_same_security_context_are_equal() {
        let security_context = b"u:unconfined_r:unconfined_t:s0";
        let security_server = security_server_with_emulator_policy();
        let sid_count_before = security_server.state.lock().sids.len();
        let sid1 = security_server
            .security_context_to_sid(security_context)
            .expect("creating SID from security context should succeed");
        let sid2 = security_server
            .security_context_to_sid(security_context)
            .expect("creating SID from security context should succeed");
        assert_eq!(sid1, sid2);
        assert_eq!(security_server.state.lock().sids.len(), sid_count_before + 1);
    }

    #[fuchsia::test]
    fn sids_allocated_outside_initial_range() {
        let security_context = b"u:unconfined_r:unconfined_t:s0";
        let security_server = security_server_with_emulator_policy();
        let sid_count_before = security_server.state.lock().sids.len();
        let sid = security_server
            .security_context_to_sid(security_context)
            .expect("creating SID from security context should succeed");
        assert_eq!(security_server.state.lock().sids.len(), sid_count_before + 1);
        assert!(sid.0.get() >= FIRST_UNUSED_SID);
    }

    #[fuchsia::test]
    fn compute_access_vector_allows_all() {
        let security_server = SecurityServer::new(Mode::Enable);
        let sid1 = SecurityId::initial(InitialSid::Kernel);
        let sid2 = SecurityId::initial(InitialSid::Unlabeled);
        assert_eq!(
            security_server.compute_access_vector(sid1, sid2, ObjectClass::Process.into()),
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
        let security_server = security_server_with_emulator_policy();
        assert_eq!(EMULATOR_BINARY_POLICY, security_server.get_binary_policy().as_slice());
    }

    #[fuchsia::test]
    fn loaded_policy_is_validated() {
        let not_really_a_policy = "not a real policy".as_bytes().to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        assert!(security_server.load_policy(not_really_a_policy.clone()).is_err());
    }

    #[fuchsia::test]
    fn enforcing_mode_is_reported() {
        let security_server = SecurityServer::new(Mode::Enable);
        assert!(!security_server.is_enforcing());

        security_server.set_enforcing(true);
        assert!(security_server.is_enforcing());
    }

    #[fuchsia::test]
    fn without_policy_conditional_booleans_are_empty() {
        let security_server = SecurityServer::new(Mode::Enable);
        assert!(security_server.conditional_booleans().is_empty());
    }

    #[fuchsia::test]
    fn conditional_booleans_can_be_queried() {
        let policy_bytes = TESTSUITE_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        assert_eq!(
            Ok(()),
            security_server.load_policy(policy_bytes).map_err(|e| format!("{:?}", e))
        );

        let booleans = security_server.conditional_booleans();
        assert!(!booleans.is_empty());
        let boolean = booleans[0].as_str();

        assert!(security_server.get_boolean("this_is_not_a_valid_boolean_name").is_err());
        assert!(security_server.get_boolean(boolean).is_ok());
    }

    #[fuchsia::test]
    fn conditional_booleans_can_be_changed() {
        let policy_bytes = TESTSUITE_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new(Mode::Enable);
        assert_eq!(
            Ok(()),
            security_server.load_policy(policy_bytes).map_err(|e| format!("{:?}", e))
        );

        let booleans = security_server.conditional_booleans();
        assert!(!booleans.is_empty());
        let boolean = booleans[0].as_str();

        let (active, pending) = security_server.get_boolean(boolean).unwrap();
        assert_eq!(active, pending, "Initially active and pending values should match");

        security_server.set_pending_boolean(boolean, !active).unwrap();
        let (active, pending) = security_server.get_boolean(boolean).unwrap();
        assert!(active != pending, "Before commit pending should differ from active");

        security_server.commit_pending_booleans();
        let (final_active, final_pending) = security_server.get_boolean(boolean).unwrap();
        assert_eq!(final_active, pending, "Pending value should be active after commit");
        assert_eq!(final_active, final_pending, "Active and pending are the same after commit");
    }

    #[fuchsia::test]
    fn status_vmo_has_correct_size_and_rights() {
        // The current version of the "status" file contains five packed
        // u32 values.
        const STATUS_T_SIZE: usize = size_of::<u32>() * 5;

        let security_server = SecurityServer::new(Mode::Enable);
        let status_vmo = security_server.get_status_vmo();

        // Verify the content and actual size of the structure are as expected.
        let content_size = status_vmo.get_content_size().unwrap() as usize;
        assert_eq!(content_size, STATUS_T_SIZE);
        let actual_size = status_vmo.get_size().unwrap() as usize;
        assert!(actual_size >= STATUS_T_SIZE);

        // Ensure the returned handle is read-only and non-resizable.
        let rights = status_vmo.basic_info().unwrap().rights;
        assert_eq!((rights & zx::Rights::MAP), zx::Rights::MAP);
        assert_eq!((rights & zx::Rights::READ), zx::Rights::READ);
        assert_eq!((rights & zx::Rights::GET_PROPERTY), zx::Rights::GET_PROPERTY);
        assert_eq!((rights & zx::Rights::WRITE), zx::Rights::NONE);
        assert_eq!((rights & zx::Rights::RESIZE), zx::Rights::NONE);
    }

    #[derive(FromBytes, FromZeroes)]
    #[repr(C, align(4))]
    struct TestSeLinuxStatusT {
        version: u32,
        sequence: u32,
        enforcing: u32,
        policyload: u32,
        deny_unknown: u32,
    }

    fn with_status_t<R>(
        security_server: &SecurityServer,
        do_test: impl FnOnce(&TestSeLinuxStatusT) -> R,
    ) -> R {
        let flags = zx::VmarFlags::PERM_READ
            | zx::VmarFlags::ALLOW_FAULTS
            | zx::VmarFlags::REQUIRE_NON_RESIZABLE;
        let map_addr = fuchsia_runtime::vmar_root_self()
            .map(0, &security_server.get_status_vmo(), 0, size_of::<TestSeLinuxStatusT>(), flags)
            .unwrap();
        let mapped_status = unsafe { &mut *(map_addr as *mut TestSeLinuxStatusT) };
        let result = do_test(mapped_status);
        unsafe {
            fuchsia_runtime::vmar_root_self()
                .unmap(map_addr, size_of::<TestSeLinuxStatusT>())
                .unwrap()
        };
        result
    }

    #[fuchsia::test]
    fn status_file_layout() {
        let security_server = SecurityServer::new(Mode::Enable);
        security_server.set_enforcing(false);
        let mut seq_no: u32 = 0;
        with_status_t(&security_server, |status| {
            assert_eq!(status.version, SELINUX_STATUS_VERSION);
            assert_eq!(status.enforcing, 0);
            seq_no = status.sequence;
            assert_eq!(seq_no % 2, 0);
        });
        security_server.set_enforcing(true);
        with_status_t(&security_server, |status| {
            assert_eq!(status.version, SELINUX_STATUS_VERSION);
            assert_eq!(status.enforcing, 1);
            assert_ne!(status.sequence, seq_no);
            seq_no = status.sequence;
            assert_eq!(seq_no % 2, 0);
        });
    }

    #[fuchsia::test]
    fn parse_security_context_no_policy() {
        let security_server = SecurityServer::new(Mode::Enable);
        let error = security_server
            .security_context_to_sid(b"u:unconfined_r:unconfined_t:s0")
            .expect_err("expected error");
        let error_string = format!("{:?}", error);
        assert!(error_string.contains("no policy"));
    }

    #[fuchsia::test]
    fn compute_new_file_sid_no_policy() {
        let security_server = SecurityServer::new(Mode::Enable);

        // Only initial SIDs can exist, until a policy has been loaded.
        let source_sid = SecurityId::initial(InitialSid::Kernel);
        let target_sid = SecurityId::initial(InitialSid::Unlabeled);
        let error = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect_err("expected error");
        let error_string = format!("{:?}", error);
        assert!(error_string.contains("no policy"));
    }

    #[fuchsia::test]
    fn compute_new_file_sid_unknown_sids() {
        let security_server = security_server_with_emulator_policy();

        // Synthesize two SIDs having been created, and subsequently invalidated.
        let source_sid = SecurityId(NonZeroU32::new(FIRST_UNUSED_SID + 1).unwrap());
        let target_sid = SecurityId(NonZeroU32::new(FIRST_UNUSED_SID + 2).unwrap());

        let error = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect_err("expected error");
        let error_string = format!("{:?}", error);
        assert!(error_string.contains("security id"));
        assert!(error_string.contains("not recognized"));
    }

    #[fuchsia::test]
    fn compute_new_file_sid_no_defaults() {
        let security_server = SecurityServer::new(Mode::Enable);
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_no_defaults_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s1")
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0")
            .expect("creating SID from security context should succeed");

        let computed_sid = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect("new sid computed");
        let computed_context = security_server
            .sid_to_security_context(computed_sid)
            .expect("computed sid associated with context");

        // User and low security level should be copied from the source,
        // and the role and type from the target.
        assert_eq!(computed_context, b"user_u:object_r:file_t:s0");
    }

    #[fuchsia::test]
    fn compute_new_file_sid_source_defaults() {
        let security_server = SecurityServer::new(Mode::Enable);
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_source_defaults_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s2:c0")
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s1-s3:c0")
            .expect("creating SID from security context should succeed");

        let computed_sid = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect("new sid computed");
        let computed_context = security_server
            .sid_to_security_context(computed_sid)
            .expect("computed sid associated with context");

        // All fields should be copied from the source, but only the "low" part of the security
        // range.
        assert_eq!(computed_context, b"user_u:unconfined_r:unconfined_t:s0");
    }

    #[fuchsia::test]
    fn compute_new_file_sid_target_defaults() {
        let security_server = SecurityServer::new(Mode::Enable);
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_target_defaults_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s2:c0")
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s1-s3:c0")
            .expect("creating SID from security context should succeed");

        let computed_sid = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect("new sid computed");
        let computed_context = security_server
            .sid_to_security_context(computed_sid)
            .expect("computed sid associated with context");

        // User, role and type copied from target, with source's low security level.
        assert_eq!(computed_context, b"file_u:object_r:file_t:s0");
    }

    #[fuchsia::test]
    fn compute_new_file_sid_range_source_low_default() {
        let security_server = SecurityServer::new(Mode::Enable);
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_source_low_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s1:c0")
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s1")
            .expect("creating SID from security context should succeed");

        let computed_sid = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect("new sid computed");
        let computed_context = security_server
            .sid_to_security_context(computed_sid)
            .expect("computed sid associated with context");

        // User and low security level copied from source, role and type as default.
        assert_eq!(computed_context, b"user_u:object_r:file_t:s0");
    }

    #[fuchsia::test]
    fn compute_new_file_sid_range_source_low_high_default() {
        let security_server = SecurityServer::new(Mode::Enable);
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_source_low_high_policy.pp")
                .to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s1:c0")
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s1")
            .expect("creating SID from security context should succeed");

        let computed_sid = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect("new sid computed");
        let computed_context = security_server
            .sid_to_security_context(computed_sid)
            .expect("computed sid associated with context");

        // User and full security range copied from source, role and type as default.
        assert_eq!(computed_context, b"user_u:object_r:file_t:s0-s1:c0");
    }

    #[fuchsia::test]
    fn compute_new_file_sid_range_source_high_default() {
        let security_server = SecurityServer::new(Mode::Enable);
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_source_high_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s1:c0")
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0")
            .expect("creating SID from security context should succeed");

        let computed_sid = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect("new sid computed");
        let computed_context = security_server
            .sid_to_security_context(computed_sid)
            .expect("computed sid associated with context");

        // User and high security level copied from source, role and type as default.
        assert_eq!(computed_context, b"user_u:object_r:file_t:s1:c0");
    }

    #[fuchsia::test]
    fn compute_new_file_sid_range_target_low_default() {
        let security_server = SecurityServer::new(Mode::Enable);
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_target_low_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s1")
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0-s1:c0")
            .expect("creating SID from security context should succeed");

        let computed_sid = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect("new sid computed");
        let computed_context = security_server
            .sid_to_security_context(computed_sid)
            .expect("computed sid associated with context");

        // User copied from source, low security level from target, role and type as default.
        assert_eq!(computed_context, b"user_u:object_r:file_t:s0");
    }

    #[fuchsia::test]
    fn compute_new_file_sid_range_target_low_high_default() {
        let security_server = SecurityServer::new(Mode::Enable);
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_target_low_high_policy.pp")
                .to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s1")
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0-s1:c0")
            .expect("creating SID from security context should succeed");

        let computed_sid = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect("new sid computed");
        let computed_context = security_server
            .sid_to_security_context(computed_sid)
            .expect("computed sid associated with context");

        // User copied from source, full security range from target, role and type as default.
        assert_eq!(computed_context, b"user_u:object_r:file_t:s0-s1:c0");
    }

    #[fuchsia::test]
    fn compute_new_file_sid_range_target_high_default() {
        let security_server = SecurityServer::new(Mode::Enable);
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_target_high_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0")
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0-s1:c0")
            .expect("creating SID from security context should succeed");

        let computed_sid = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect("new sid computed");
        let computed_context = security_server
            .sid_to_security_context(computed_sid)
            .expect("computed sid associated with context");

        // User copied from source, high security level from target, role and type as default.
        assert_eq!(computed_context, b"user_u:object_r:file_t:s1:c0");
    }
}
