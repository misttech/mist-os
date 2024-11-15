// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::access_vector_cache::{Manager as AvcManager, Query, QueryMut};
use crate::permission_check::PermissionCheck;
use crate::policy::metadata::HandleUnknown;
use crate::policy::parser::ByValue;
use crate::policy::{
    parse_policy_by_value, AccessDecision, AccessVector, AccessVectorComputer, ClassId,
    FsUseLabelAndType, FsUseType, Policy,
};
use crate::sid_table::SidTable;
use crate::sync::Mutex;
use crate::{
    AbstractObjectClass, ClassPermission, FileClass, FileSystemLabel, FileSystemLabelingScheme,
    FileSystemMountOptions, InitialSid, NullessByteStr, ObjectClass, Permission, SeLinuxStatus,
    SeLinuxStatusPublisher, SecurityId,
};

use anyhow::Context as _;
use std::collections::HashMap;
use std::ops::DerefMut;
use std::sync::Arc;

const ROOT_PATH: &'static str = "/";

struct ActivePolicy {
    /// Parsed policy structure.
    parsed: Arc<Policy<ByValue<Vec<u8>>>>,

    /// The binary policy that was previously passed to `load_policy()`.
    binary: Vec<u8>,

    /// Allocates and maintains the mapping between `SecurityId`s (SIDs) and Security Contexts.
    sid_table: SidTable,
}

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
    fn names(&self) -> Vec<String> {
        self.active.keys().cloned().collect()
    }
    fn set_pending(&mut self, name: &str, value: bool) -> Result<(), ()> {
        if !self.active.contains_key(name) {
            return Err(());
        }
        self.pending.insert(name.into(), value);
        Ok(())
    }
    fn get(&self, name: &str) -> Result<(bool, bool), ()> {
        let active = self.active.get(name).ok_or(())?;
        let pending = self.pending.get(name).unwrap_or(active);
        Ok((*active, *pending))
    }
    fn commit_pending(&mut self) {
        self.active.extend(self.pending.drain());
    }
}

struct SecurityServerState {
    /// Describes the currently active policy.
    active_policy: Option<ActivePolicy>,

    /// Holds active and pending states for each boolean defined by policy.
    booleans: SeLinuxBooleans,

    /// Write-only interface to the data stored in the selinuxfs status file.
    status_publisher: Option<Box<dyn SeLinuxStatusPublisher>>,

    /// True if hooks should enforce policy-based access decisions.
    enforcing: bool,

    /// Count of changes to the active policy.  Changes include both loads
    /// of complete new policies, and modifications to a previously loaded
    /// policy, e.g. by committing new values to conditional booleans in it.
    policy_change_count: u32,
}

impl SecurityServerState {
    fn deny_unknown(&self) -> bool {
        self.active_policy
            .as_ref()
            .map_or(true, |p| p.parsed.handle_unknown() != HandleUnknown::Allow)
    }
    fn reject_unknown(&self) -> bool {
        self.active_policy
            .as_ref()
            .map_or(false, |p| p.parsed.handle_unknown() == HandleUnknown::Reject)
    }

    fn expect_active_policy(&self) -> &ActivePolicy {
        &self.active_policy.as_ref().expect("policy should be loaded")
    }

    fn expect_active_policy_mut(&mut self) -> &mut ActivePolicy {
        self.active_policy.as_mut().expect("policy should be loaded")
    }
}

pub struct SecurityServer {
    /// Manager for any access vector cache layers that are shared between threads subject to access
    /// control by this security server. This [`AvcManager`] is also responsible for constructing
    /// thread-local caches for use by individual threads that subject to access control by this
    /// security server.
    avc_manager: AvcManager<SecurityServer>,

    /// The mutable state of the security server.
    state: Mutex<SecurityServerState>,
}

impl SecurityServer {
    pub fn new() -> Arc<Self> {
        let avc_manager = AvcManager::new();
        let state = Mutex::new(SecurityServerState {
            active_policy: None,
            booleans: SeLinuxBooleans::default(),
            status_publisher: None,
            enforcing: false,
            policy_change_count: 0,
        });

        let security_server = Arc::new(Self { avc_manager, state });

        // TODO(http://b/304776236): Consider constructing shared owner of `AvcManager` and
        // `SecurityServer` to eliminate weak reference.
        security_server.as_ref().avc_manager.set_security_server(Arc::downgrade(&security_server));

        security_server
    }

    /// Converts a shared pointer to [`SecurityServer`] to a [`PermissionCheck`] without consuming
    /// the pointer.
    pub fn as_permission_check<'a>(self: &'a Self) -> PermissionCheck<'a> {
        PermissionCheck::new(self, self.avc_manager.get_shared_cache())
    }

    /// Returns the security ID mapped to `security_context`, creating it if it does not exist.
    ///
    /// All objects with the same security context will have the same SID associated.
    pub fn security_context_to_sid(
        &self,
        security_context: NullessByteStr<'_>,
    ) -> Result<SecurityId, anyhow::Error> {
        let mut locked_state = self.state.lock();
        let active_policy = locked_state
            .active_policy
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no policy loaded"))?;
        let context = active_policy
            .parsed
            .parse_security_context(security_context)
            .map_err(anyhow::Error::from)?;
        Ok(active_policy.sid_table.security_context_to_sid(&context))
    }

    /// Returns the Security Context string for the requested `sid`.
    /// This is used only where Contexts need to be stringified to expose to userspace, as
    /// is the case for e.g. the `/proc/*/attr/` filesystem and `security.selinux` extended
    /// attribute values.
    pub fn sid_to_security_context(&self, sid: SecurityId) -> Option<Vec<u8>> {
        let locked_state = self.state.lock();
        let active_policy = locked_state.active_policy.as_ref()?;
        let context = active_policy.sid_table.try_sid_to_security_context(sid)?;
        Some(active_policy.parsed.serialize_security_context(context))
    }

    /// Applies the supplied policy to the security server.
    pub fn load_policy(&self, binary_policy: Vec<u8>) -> Result<(), anyhow::Error> {
        // Parse the supplied policy, and reject the load operation if it is
        // malformed or invalid.
        let (parsed, binary) = parse_policy_by_value(binary_policy)?;
        let parsed = Arc::new(parsed.validate()?);

        // Replace any existing policy and push update to `state.status_publisher`.
        self.with_state_and_update_status(|state| {
            let sid_table = if let Some(previous_active_policy) = &state.active_policy {
                SidTable::new_from_previous(parsed.clone(), &previous_active_policy.sid_table)
            } else {
                SidTable::new(parsed.clone())
            };

            // TODO(b/324265752): Determine whether SELinux booleans need to be retained across
            // policy (re)loads.
            state.booleans.reset(
                parsed
                    .conditional_booleans()
                    .iter()
                    // TODO(b/324392507): Relax the UTF8 requirement on policy strings.
                    .map(|(name, value)| (String::from_utf8((*name).to_vec()).unwrap(), *value))
                    .collect(),
            );

            state.active_policy = Some(ActivePolicy { parsed, binary, sid_table });
            state.policy_change_count += 1;
        });

        Ok(())
    }

    /// Returns the active policy in binary form.
    pub fn get_binary_policy(&self) -> Vec<u8> {
        self.state.lock().active_policy.as_ref().map_or(Vec::new(), |p| p.binary.clone())
    }

    /// Returns true if a policy has been loaded.
    pub fn has_policy(&self) -> bool {
        self.state.lock().active_policy.is_some()
    }

    /// Set to enforcing mode if `enforce` is true, permissive mode otherwise.
    pub fn set_enforcing(&self, enforcing: bool) {
        self.with_state_and_update_status(|state| state.enforcing = enforcing);
    }

    pub fn is_enforcing(&self) -> bool {
        self.state.lock().enforcing
    }

    /// Returns true if the policy requires unknown class / permissions to be
    /// denied. Defaults to true until a policy is loaded.
    pub fn deny_unknown(&self) -> bool {
        self.state.lock().deny_unknown()
    }

    /// Returns true if the policy requires unknown class / permissions to be
    /// rejected. Defaults to false until a policy is loaded.
    pub fn reject_unknown(&self) -> bool {
        self.state.lock().reject_unknown()
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

    /// Returns the list of all class names.
    pub fn class_names(&self) -> Result<Vec<Vec<u8>>, ()> {
        let locked_state = self.state.lock();
        let names = locked_state
            .expect_active_policy()
            .parsed
            .classes()
            .iter()
            .map(|class| class.class_name.to_vec())
            .collect();
        Ok(names)
    }

    /// Returns the class identifier of a class, if it exists.
    pub fn class_id_by_name(&self, name: &str) -> Result<ClassId, ()> {
        let locked_state = self.state.lock();
        Ok(locked_state
            .expect_active_policy()
            .parsed
            .classes()
            .iter()
            .find(|class| class.class_name == name.as_bytes())
            .ok_or(())?
            .class_id)
    }

    /// Returns the class identifier of a class, if it exists.
    pub fn class_permissions_by_name(&self, name: &str) -> Result<Vec<(u32, Vec<u8>)>, ()> {
        let locked_state = self.state.lock();
        locked_state.expect_active_policy().parsed.find_class_permissions_by_name(name)
    }

    /// Determines the appropriate [`FileSystemLabel`] for a mounted filesystem given this security
    /// server's loaded policy, the name of the filesystem type ("ext4" or "tmpfs", for example),
    /// and the security-relevant mount options passed for the mount operation.
    pub fn resolve_fs_label(
        &self,
        fs_type: NullessByteStr<'_>,
        mount_options: &FileSystemMountOptions,
    ) -> FileSystemLabel {
        let mut locked_state = self.state.lock();
        let active_policy = locked_state.expect_active_policy_mut();

        let mountpoint_sid_from_mount_option =
            sid_from_mount_option(active_policy, &mount_options.context);
        let fs_sid_from_mount_option =
            sid_from_mount_option(active_policy, &mount_options.fs_context);
        let def_sid_from_mount_option =
            sid_from_mount_option(active_policy, &mount_options.def_context);
        let root_sid_from_mount_option =
            sid_from_mount_option(active_policy, &mount_options.root_context);

        if let Some(mountpoint_sid) = mountpoint_sid_from_mount_option {
            // `mount_options.context` is set, so the file-system and the nodes it contains
            // have the specified read-only security label set.
            FileSystemLabel { sid: mountpoint_sid, scheme: FileSystemLabelingScheme::Mountpoint }
        } else if let Some(FsUseLabelAndType { context, use_type }) =
            active_policy.parsed.fs_use_label_and_type(fs_type)
        {
            // There is an `fs_use` statement for this file-system type in the policy.
            let fs_sid_from_policy = active_policy.sid_table.security_context_to_sid(&context);
            let fs_sid = fs_sid_from_mount_option.unwrap_or(fs_sid_from_policy);
            FileSystemLabel {
                sid: fs_sid,
                scheme: FileSystemLabelingScheme::FsUse {
                    fs_use_type: use_type,
                    def_sid: def_sid_from_mount_option
                        .unwrap_or_else(|| SecurityId::initial(InitialSid::File)),
                    root_sid: root_sid_from_mount_option.unwrap_or(fs_sid),
                },
            }
        } else if let Some(context) =
            active_policy.parsed.genfscon_label_for_fs_and_path(fs_type, ROOT_PATH.into(), None)
        {
            // There is a `genfscon` statement for this file-system type in the policy.
            let genfscon_sid = active_policy.sid_table.security_context_to_sid(&context);
            let fs_sid = fs_sid_from_mount_option.unwrap_or(genfscon_sid);
            FileSystemLabel { sid: fs_sid, scheme: FileSystemLabelingScheme::GenFsCon }
        } else {
            // The name of the filesystem type was not recognized.
            //
            // TODO: https://fxbug.dev/363215797 - verify that these defaults are correct.
            let unrecognized_filesystem_type_sid = SecurityId::initial(InitialSid::File);
            let unrecognized_filesystem_type_fs_use_type = FsUseType::Xattr;

            FileSystemLabel {
                sid: fs_sid_from_mount_option.unwrap_or(unrecognized_filesystem_type_sid),
                scheme: FileSystemLabelingScheme::FsUse {
                    fs_use_type: unrecognized_filesystem_type_fs_use_type,
                    def_sid: def_sid_from_mount_option.unwrap_or(unrecognized_filesystem_type_sid),
                    root_sid: root_sid_from_mount_option
                        .unwrap_or(unrecognized_filesystem_type_sid),
                },
            }
        }
    }

    /// If there is a genfscon statement for the given filesystem type, returns the
    /// [`SecurityContext`] that should be used for a node in path `node_path`. When `node_path` is
    /// the root path ("/") the label additionally corresponds to the `FileSystem` label.
    pub fn genfscon_label_for_fs_and_path(
        &self,
        fs_type: NullessByteStr<'_>,
        node_path: NullessByteStr<'_>,
        class_id: Option<ClassId>,
    ) -> Option<SecurityId> {
        let mut locked_state = self.state.lock();
        let active_policy = locked_state.expect_active_policy_mut();
        let security_context = active_policy.parsed.genfscon_label_for_fs_and_path(
            fs_type,
            node_path.into(),
            class_id,
        )?;
        Some(active_policy.sid_table.security_context_to_sid(&security_context))
    }

    /// Computes the precise access vector for `source_sid` targeting `target_sid` as class
    /// `target_class`.
    pub fn compute_access_vector(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: AbstractObjectClass,
    ) -> AccessDecision {
        let locked_state = self.state.lock();

        let active_policy = match &locked_state.active_policy {
            Some(active_policy) => active_policy,
            // All permissions are allowed when no policy is loaded, regardless of enforcing state.
            None => return AccessDecision::allow(AccessVector::ALL),
        };

        // Policy is loaded, so `sid_to_security_context()` will not panic.
        let source_context = active_policy.sid_table.sid_to_security_context(source_sid);
        let target_context = active_policy.sid_table.sid_to_security_context(target_sid);

        // Access decisions are currently based solely on explicit "allow" rules.
        // TODO: https://fxbug.dev/372400976 - Include permissions from matched "constraints"
        // rules in the policy.
        // TODO: https://fxbug.dev/372400419 - Validate that "neverallow" rules are respected.
        match target_class {
            AbstractObjectClass::System(target_class) => {
                active_policy.parsed.compute_explicitly_allowed(
                    source_context.type_(),
                    target_context.type_(),
                    target_class,
                )
            }
            AbstractObjectClass::Custom(target_class) => {
                active_policy.parsed.compute_explicitly_allowed_custom(
                    source_context.type_(),
                    target_context.type_(),
                    &target_class,
                )
            }
            // No meaningful policy can be determined without target class.
            _ => AccessDecision::allow(AccessVector::NONE),
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
        let mut locked_state = self.state.lock();
        let active_policy = match &mut locked_state.active_policy {
            Some(active_policy) => active_policy,
            None => {
                return Err(anyhow::anyhow!("no policy loaded")).context("computing new file sid")
            }
        };

        // Policy is loaded, so `sid_to_security_context()` will not panic.
        let source_context = active_policy.sid_table.sid_to_security_context(source_sid);
        let target_context = active_policy.sid_table.sid_to_security_context(target_sid);

        active_policy
            .parsed
            .new_file_security_context(source_context, target_context, &file_class)
            // TODO(http://b/334968228): check that transitions are allowed.
            .map(|sc| active_policy.sid_table.security_context_to_sid(&sc))
            .map_err(anyhow::Error::from)
            .context("computing new file security context from policy")
    }

    pub fn compute_new_sid(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: ObjectClass,
    ) -> Result<SecurityId, anyhow::Error> {
        let mut locked_state = self.state.lock();
        let active_policy = locked_state.expect_active_policy_mut();

        // Policy is loaded, so `sid_to_security_context()` will not panic.
        let source_context = active_policy.sid_table.sid_to_security_context(source_sid);
        let target_context = active_policy.sid_table.sid_to_security_context(target_sid);

        active_policy
            .parsed
            .new_security_context(source_context, target_context, &target_class)
            .map(|sc| active_policy.sid_table.security_context_to_sid(&sc))
            .map_err(anyhow::Error::from)
            .context("computing new security context from policy")
    }

    /// Returns true if the `bounded_sid` is bounded by the `parent_sid`.
    /// Bounds relationships are mostly enforced by policy tooling, so this only requires validating
    /// that the policy entry for the `TypeId` of `bounded_sid` has the `TypeId` of `parent_sid`
    /// specified in its `bounds`.
    pub fn is_bounded_by(&self, bounded_sid: SecurityId, parent_sid: SecurityId) -> bool {
        let locked_state = self.state.lock();
        let active_policy = locked_state.expect_active_policy();
        let bounded_type = active_policy.sid_table.sid_to_security_context(bounded_sid).type_();
        let parent_type = active_policy.sid_table.sid_to_security_context(parent_sid).type_();
        active_policy.parsed.is_bounded_by(bounded_type, parent_type)
    }

    /// Assign a [`SeLinuxStatusPublisher`] to be used for pushing updates to the security server's
    /// policy status. This should be invoked exactly once when `selinuxfs` is initialized.
    ///
    /// # Panics
    ///
    /// This will panic on debug builds if it is invoked multiple times.
    pub fn set_status_publisher(&self, status_holder: Box<dyn SeLinuxStatusPublisher>) {
        self.with_state_and_update_status(|state| {
            assert!(state.status_publisher.is_none());
            state.status_publisher = Some(status_holder);
        });
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

    /// Runs the supplied function with locked `self`, and then updates the SELinux status file
    /// associated with `self.state.status_publisher`, if any.
    fn with_state_and_update_status(&self, f: impl FnOnce(&mut SecurityServerState)) {
        let mut locked_state = self.state.lock();
        f(locked_state.deref_mut());
        let new_value = SeLinuxStatus {
            is_enforcing: locked_state.enforcing,
            change_count: locked_state.policy_change_count,
            deny_unknown: locked_state.deny_unknown(),
        };
        if let Some(status_publisher) = &mut locked_state.status_publisher {
            status_publisher.set_status(new_value);
        }
    }
}

impl Query for SecurityServer {
    fn query(
        &self,
        source_sid: SecurityId,
        target_sid: SecurityId,
        target_class: AbstractObjectClass,
    ) -> AccessDecision {
        self.compute_access_vector(source_sid, target_sid, target_class)
    }
}

impl AccessVectorComputer for SecurityServer {
    fn access_vector_from_permissions<P: ClassPermission + Into<Permission> + Clone + 'static>(
        &self,
        permissions: &[P],
    ) -> Option<AccessVector> {
        match &self.state.lock().active_policy {
            Some(policy) => policy.parsed.access_vector_from_permissions(permissions),
            None => Some(AccessVector::NONE),
        }
    }
}

/// Computes a [`SecurityId`] given a non-[`None`] value for one of the four
/// "context" mount options (https://man7.org/linux/man-pages/man8/mount.8.html).
fn sid_from_mount_option(
    active_policy: &mut ActivePolicy,
    mount_option: &Option<Vec<u8>>,
) -> Option<SecurityId> {
    if let Some(label) = mount_option.as_ref() {
        Some(
            if let Some(context) = active_policy.parsed.parse_security_context(label.into()).ok() {
                active_policy.sid_table.security_context_to_sid(&context)
            } else {
                // The mount option is present-but-not-valid: we use `Unlabeled`.
                SecurityId::initial(InitialSid::Unlabeled)
            },
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::permission_check::PermissionCheckResult;
    use crate::sid_table::testing::allocate_sid;
    use crate::{CommonFilePermission, ProcessPermission};

    const TESTSUITE_BINARY_POLICY: &[u8] = include_bytes!("../testdata/policies/selinux_testsuite");
    const TESTS_BINARY_POLICY: &[u8] =
        include_bytes!("../testdata/micro_policies/security_server_tests_policy.pp");

    fn security_server_with_tests_policy() -> Arc<SecurityServer> {
        let policy_bytes = TESTS_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new();
        assert_eq!(
            Ok(()),
            security_server.load_policy(policy_bytes).map_err(|e| format!("{:?}", e))
        );
        security_server
    }

    #[test]
    fn compute_access_vector_allows_all() {
        let security_server = SecurityServer::new();
        let sid1 = SecurityId::initial(InitialSid::Kernel);
        let sid2 = SecurityId::initial(InitialSid::Unlabeled);
        assert_eq!(
            security_server.compute_access_vector(sid1, sid2, ObjectClass::Process.into()).allow,
            AccessVector::ALL
        );
    }

    #[test]
    fn loaded_policy_can_be_retrieved() {
        let security_server = security_server_with_tests_policy();
        assert_eq!(TESTS_BINARY_POLICY, security_server.get_binary_policy().as_slice());
    }

    #[test]
    fn loaded_policy_is_validated() {
        let not_really_a_policy = "not a real policy".as_bytes().to_vec();
        let security_server = SecurityServer::new();
        assert!(security_server.load_policy(not_really_a_policy.clone()).is_err());
    }

    #[test]
    fn enforcing_mode_is_reported() {
        let security_server = SecurityServer::new();
        assert!(!security_server.is_enforcing());

        security_server.set_enforcing(true);
        assert!(security_server.is_enforcing());
    }

    #[test]
    fn without_policy_conditional_booleans_are_empty() {
        let security_server = SecurityServer::new();
        assert!(security_server.conditional_booleans().is_empty());
    }

    #[test]
    fn conditional_booleans_can_be_queried() {
        let policy_bytes = TESTSUITE_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new();
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

    #[test]
    fn conditional_booleans_can_be_changed() {
        let policy_bytes = TESTSUITE_BINARY_POLICY.to_vec();
        let security_server = SecurityServer::new();
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

    #[test]
    fn parse_security_context_no_policy() {
        let security_server = SecurityServer::new();
        let error = security_server
            .security_context_to_sid(b"unconfined_u:unconfined_r:unconfined_t:s0".into())
            .expect_err("expected error");
        let error_string = format!("{:?}", error);
        assert!(error_string.contains("no policy"));
    }

    #[test]
    fn compute_new_file_sid_no_policy() {
        let security_server = SecurityServer::new();

        // Only initial SIDs can exist, until a policy has been loaded.
        let source_sid = SecurityId::initial(InitialSid::Kernel);
        let target_sid = SecurityId::initial(InitialSid::Unlabeled);
        let error = security_server
            .compute_new_file_sid(source_sid, target_sid, FileClass::File)
            .expect_err("expected error");
        let error_string = format!("{:?}", error);
        assert!(error_string.contains("no policy"));
    }

    #[test]
    fn compute_new_file_sid_no_defaults() {
        let security_server = SecurityServer::new();
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_no_defaults_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s1".into())
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0".into())
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

    #[test]
    fn compute_new_file_sid_source_defaults() {
        let security_server = SecurityServer::new();
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_source_defaults_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s2:c0".into())
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s1-s3:c0".into())
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

    #[test]
    fn compute_new_file_sid_target_defaults() {
        let security_server = SecurityServer::new();
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_target_defaults_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s2:c0".into())
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s1-s3:c0".into())
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

    #[test]
    fn compute_new_file_sid_range_source_low_default() {
        let security_server = SecurityServer::new();
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_source_low_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s1:c0".into())
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s1".into())
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

    #[test]
    fn compute_new_file_sid_range_source_low_high_default() {
        let security_server = SecurityServer::new();
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_source_low_high_policy.pp")
                .to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s1:c0".into())
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s1".into())
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

    #[test]
    fn compute_new_file_sid_range_source_high_default() {
        let security_server = SecurityServer::new();
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_source_high_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0-s1:c0".into())
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0".into())
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

    #[test]
    fn compute_new_file_sid_range_target_low_default() {
        let security_server = SecurityServer::new();
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_target_low_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s1".into())
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0-s1:c0".into())
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

    #[test]
    fn compute_new_file_sid_range_target_low_high_default() {
        let security_server = SecurityServer::new();
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_target_low_high_policy.pp")
                .to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s1".into())
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0-s1:c0".into())
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

    #[test]
    fn compute_new_file_sid_range_target_high_default() {
        let security_server = SecurityServer::new();
        let policy_bytes =
            include_bytes!("../testdata/micro_policies/file_range_target_high_policy.pp").to_vec();
        security_server.load_policy(policy_bytes).expect("binary policy loads");

        let source_sid = security_server
            .security_context_to_sid(b"user_u:unconfined_r:unconfined_t:s0".into())
            .expect("creating SID from security context should succeed");
        let target_sid = security_server
            .security_context_to_sid(b"file_u:object_r:file_t:s0-s1:c0".into())
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

    #[test]
    fn unknown_sids_are_effectively_unlabeled() {
        let security_server = security_server_with_tests_policy();
        security_server.set_enforcing(true);

        let valid_sid =
            security_server.security_context_to_sid(b"user1:object_r:type0:s0:c0".into()).unwrap();

        // Synthesize a SID with no Security Context in the SID table, which would be the case if the
        // SID had been allocated, but then removed because a policy load invalidated the Context.
        let unlabeled_sid =
            allocate_sid(&mut security_server.state.lock().expect_active_policy_mut().sid_table);

        let permission_check = security_server.as_permission_check();

        // Test policy allows "type0" the process getsched capability to "unlabeled_t".
        assert!(
            permission_check
                .has_permission(valid_sid, unlabeled_sid, ProcessPermission::GetSched)
                .permit
        );
        assert!(
            !permission_check
                .has_permission(valid_sid, unlabeled_sid, ProcessPermission::SetSched)
                .permit
        );

        // Test policy allows "unlabeled_t" the process setsched capability to "type0".
        assert!(
            !permission_check
                .has_permission(unlabeled_sid, valid_sid, ProcessPermission::GetSched)
                .permit
        );
        assert!(
            permission_check
                .has_permission(unlabeled_sid, valid_sid, ProcessPermission::SetSched)
                .permit
        );
    }

    #[test]
    fn permission_check_permissive() {
        let security_server = security_server_with_tests_policy();
        security_server.set_enforcing(false);
        assert!(!security_server.is_enforcing());

        let sid =
            security_server.security_context_to_sid("user0:object_r:type0:s0".into()).unwrap();
        let permission_check = security_server.as_permission_check();

        // Test policy grants "type0" the process-fork permission to itself.
        // Since the permission is granted by policy, the check will not be audit logged.
        assert_eq!(
            permission_check.has_permission(sid, sid, ProcessPermission::Fork),
            PermissionCheckResult { permit: true, audit: false }
        );

        // Test policy does not grant "type0" the process-getrlimit permission to itself, but
        // the security server is configured to be permissive. Because the permission was not
        // granted by the policy, the check will be audit logged.
        assert_eq!(
            permission_check.has_permission(sid, sid, ProcessPermission::GetRlimit),
            PermissionCheckResult { permit: true, audit: true }
        );

        // Test policy is built with "deny unknown" behaviour, and has no "blk_file" class defined.
        // This permission should be treated like a defined permission that is not allowed to the
        // source, and both allowed and audited here.
        assert_eq!(
            permission_check.has_permission(
                sid,
                sid,
                CommonFilePermission::GetAttr.for_class(FileClass::Block)
            ),
            PermissionCheckResult { permit: true, audit: true }
        );
    }

    #[test]
    fn permission_check_enforcing() {
        let security_server = security_server_with_tests_policy();
        security_server.set_enforcing(true);
        assert!(security_server.is_enforcing());

        let sid =
            security_server.security_context_to_sid("user0:object_r:type0:s0".into()).unwrap();
        let permission_check = security_server.as_permission_check();

        // Test policy grants "type0" the process-fork permission to itself.
        assert_eq!(
            permission_check.has_permission(sid, sid, ProcessPermission::Fork),
            PermissionCheckResult { permit: true, audit: false }
        );

        // Test policy does not grant "type0" the process-getrlimit permission to itself.
        // Permission denials are audit logged in enforcing mode.
        assert_eq!(
            permission_check.has_permission(sid, sid, ProcessPermission::GetRlimit),
            PermissionCheckResult { permit: false, audit: true }
        );

        // Test policy is built with "deny unknown" behaviour, and has no "blk_file" class defined.
        // This permission should therefore be denied, and the denial audited.
        assert_eq!(
            permission_check.has_permission(
                sid,
                sid,
                CommonFilePermission::GetAttr.for_class(FileClass::Block)
            ),
            PermissionCheckResult { permit: false, audit: true }
        );
    }

    #[test]
    fn permissive_domain() {
        let security_server = security_server_with_tests_policy();
        security_server.set_enforcing(true);
        assert!(security_server.is_enforcing());

        let permissive_sid = security_server
            .security_context_to_sid("user0:object_r:permissive_t:s0".into())
            .unwrap();
        let non_permissive_sid = security_server
            .security_context_to_sid("user0:object_r:non_permissive_t:s0".into())
            .unwrap();

        let permission_check = security_server.as_permission_check();

        // Test policy grants process-getsched permission to both of the test domains.
        assert_eq!(
            permission_check.has_permission(
                permissive_sid,
                permissive_sid,
                ProcessPermission::GetSched
            ),
            PermissionCheckResult { permit: true, audit: false }
        );
        assert_eq!(
            permission_check.has_permission(
                non_permissive_sid,
                non_permissive_sid,
                ProcessPermission::GetSched
            ),
            PermissionCheckResult { permit: true, audit: false }
        );

        // Test policy does not grant process-getsched permission to the test domains on one another.
        // The permissive domain will be granted the permission, since it is marked permissive.
        assert_eq!(
            permission_check.has_permission(
                permissive_sid,
                non_permissive_sid,
                ProcessPermission::GetSched
            ),
            // TODO: https://fxbug.dev/379153786 - Don't audit permissive types, to reduce log
            // overload in tests, until "dontaudit" is available.
            PermissionCheckResult { permit: true, audit: false }
        );
        assert_eq!(
            permission_check.has_permission(
                non_permissive_sid,
                permissive_sid,
                ProcessPermission::GetSched
            ),
            PermissionCheckResult { permit: false, audit: true }
        );

        // Test policy has "deny unknown" behaviour and does not define the "blk_file" class, so
        // access to a permission on it will depend on whether the source is permissive.
        // The target domain is irrelevant, since the class/permission do not exist, so the non-
        // permissive SID is used for both checks.
        assert_eq!(
            permission_check.has_permission(
                permissive_sid,
                non_permissive_sid,
                CommonFilePermission::GetAttr.for_class(FileClass::Block)
            ),
            // TODO: https://fxbug.dev/379153786 - Don't audit permissive types, to reduce log
            // overload in tests, until "dontaudit" is available.
            PermissionCheckResult { permit: true, audit: false }
        );
        assert_eq!(
            permission_check.has_permission(
                non_permissive_sid,
                non_permissive_sid,
                CommonFilePermission::GetAttr.for_class(FileClass::Block)
            ),
            PermissionCheckResult { permit: false, audit: true }
        );
    }
}
