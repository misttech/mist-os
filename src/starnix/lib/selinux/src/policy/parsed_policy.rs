// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::NullessByteStr;

use super::arrays::{
    AccessVectorRules, ConditionalNodes, Context, DeprecatedFilenameTransitions,
    FilenameTransitionList, FilenameTransitions, FsUses, GenericFsContexts, IPv6Nodes,
    InfinitiBandEndPorts, InfinitiBandPartitionKeys, InitialSids, NamedContextPairs, Nodes, Ports,
    RangeTransitions, RoleAllow, RoleAllows, RoleTransition, RoleTransitions, SimpleArray,
    MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY, XPERMS_TYPE_IOCTL_PREFIXES,
    XPERMS_TYPE_IOCTL_PREFIX_AND_POSTFIXES,
};
use super::error::{ParseError, ValidateError};
use super::extensible_bitmap::ExtensibleBitmap;
use super::metadata::{Config, Counts, HandleUnknown, Magic, PolicyVersion, Signature};
use super::parser::ByValue;
use super::security_context::{Level, SecurityContext};
use super::symbols::{
    Category, Class, Classes, CommonSymbol, CommonSymbols, ConditionalBoolean, MlsLevel, Role,
    Sensitivity, SymbolList, Type, User,
};
use super::{
    AccessDecision, AccessVector, CategoryId, ClassId, IoctlAccessDecision, Parse, RoleId,
    SensitivityId, TypeId, UserId, Validate, XpermsBitmap, SELINUX_AVD_FLAGS_PERMISSIVE,
};

use anyhow::Context as _;
use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::Hash;
use zerocopy::little_endian as le;

/// A parsed binary policy.
#[derive(Debug)]
pub struct ParsedPolicy {
    /// A distinctive number that acts as a binary format-specific header for SELinux binary policy
    /// files.
    magic: Magic,
    /// A length-encoded string, "SE Linux", which identifies this policy as an SE Linux policy.
    signature: Signature,
    /// The policy format version number. Different version may support different policy features.
    policy_version: PolicyVersion,
    /// Whole-policy configuration, such as how to handle queries against unknown classes.
    config: Config,
    /// High-level counts of subsequent policy elements.
    counts: Counts,
    policy_capabilities: ExtensibleBitmap,
    permissive_map: ExtensibleBitmap,
    /// Common permissions that can be mixed in to classes.
    common_symbols: SymbolList<CommonSymbol>,
    /// The set of classes referenced by this policy.
    classes: SymbolList<Class>,
    /// The set of roles referenced by this policy.
    roles: SymbolList<Role>,
    /// The set of types referenced by this policy.
    types: SymbolList<Type>,
    /// The set of users referenced by this policy.
    users: SymbolList<User>,
    /// The set of dynamically adjustable booleans referenced by this policy.
    conditional_booleans: SymbolList<ConditionalBoolean>,
    /// The set of sensitivity levels referenced by this policy.
    sensitivities: SymbolList<Sensitivity>,
    /// The set of categories referenced by this policy.
    categories: SymbolList<Category>,
    /// The set of access vector rules referenced by this policy.
    access_vector_rules: SimpleArray<AccessVectorRules>,
    conditional_lists: SimpleArray<ConditionalNodes>,
    /// The set of role transitions to apply when instantiating new objects.
    role_transitions: RoleTransitions,
    /// The set of role transitions allowed by policy.
    role_allowlist: RoleAllows,
    filename_transition_list: FilenameTransitionList,
    initial_sids: SimpleArray<InitialSids>,
    filesystems: SimpleArray<NamedContextPairs>,
    ports: SimpleArray<Ports>,
    network_interfaces: SimpleArray<NamedContextPairs>,
    nodes: SimpleArray<Nodes>,
    fs_uses: SimpleArray<FsUses>,
    ipv6_nodes: SimpleArray<IPv6Nodes>,
    infinitiband_partition_keys: Option<SimpleArray<InfinitiBandPartitionKeys>>,
    infinitiband_end_ports: Option<SimpleArray<InfinitiBandEndPorts>>,
    /// A set of labeling statements to apply to given filesystems and/or their subdirectories.
    /// Corresponds to the `genfscon` labeling statement in the policy.
    generic_fs_contexts: SimpleArray<GenericFsContexts>,
    range_transitions: SimpleArray<RangeTransitions>,
    /// Extensible bitmaps that encode associations between types and attributes.
    attribute_maps: Vec<ExtensibleBitmap>,
}

impl ParsedPolicy {
    /// The policy version stored in the underlying binary policy.
    pub fn policy_version(&self) -> u32 {
        self.policy_version.policy_version()
    }

    /// The way "unknown" policy decisions should be handed according to the underlying binary
    /// policy.
    pub fn handle_unknown(&self) -> HandleUnknown {
        self.config.handle_unknown()
    }

    /// Computes the access granted to `source_type` on `target_type`, for the specified
    /// `target_class`. The result is a set of access vectors with bits set for each
    /// `target_class` permission, describing which permissions are allowed, and
    /// which should have access checks audit-logged when denied, or allowed.
    ///
    /// An [`AccessDecision`] is accumulated, starting from no permissions to be granted,
    /// nor audit-logged if allowed, and all permissions to be audit-logged if denied.
    /// Permissions that are explicitly `allow`ed, but that are subject to unsatisfied
    /// constraints, are removed from the allowed set. Matching policy statements then
    /// add permissions to the granted & audit-allow sets, or remove them from the
    /// audit-deny set.
    pub(super) fn compute_access_decision(
        &self,
        source_context: &SecurityContext,
        target_context: &SecurityContext,
        target_class: &Class,
    ) -> AccessDecision {
        let mut access_decision = self.compute_explicitly_allowed(
            source_context.type_(),
            target_context.type_(),
            target_class,
        );
        access_decision.allow -=
            self.compute_denied_by_constraints(source_context, target_context, target_class);
        access_decision
    }

    /// Computes the access granted to `source_type` on `target_type`, for the specified
    /// `target_class`. The result is a set of access vectors with bits set for each
    /// `target_class` permission, describing which permissions are explicitly allowed,
    /// and which should have access checks audit-logged when denied, or allowed.
    pub(super) fn compute_explicitly_allowed(
        &self,
        source_type: TypeId,
        target_type: TypeId,
        target_class: &Class,
    ) -> AccessDecision {
        let target_class_id = target_class.id();

        let mut computed_access_vector = AccessVector::NONE;
        let mut computed_audit_allow = AccessVector::NONE;
        let mut computed_audit_deny = AccessVector::ALL;

        for access_vector_rule in self.access_vector_rules.data.iter() {
            // Ignore `access_vector_rule` entries not relayed to "allow" or
            // audit statements.
            //
            // TODO: https://fxbug.dev/379657220 - Can an `access_vector_rule`
            // entry express e.g. both "allow" and "auditallow" at the same
            // time?
            if !access_vector_rule.is_allow()
                && !access_vector_rule.is_auditallow()
                && !access_vector_rule.is_dontaudit()
            {
                continue;
            }

            // Concern ourselves only with `allow [source-type] [target-type]:[class] [...];`
            // policy statements where `[class]` matches `target_class_id`.
            if access_vector_rule.target_class() != target_class_id {
                continue;
            }

            // Note: Perform bitmap lookups last: they are the most expensive comparison operation.

            // Note: Type ids start at 1, but are 0-indexed in bitmaps: hence the `type - 1` bitmap
            // lookups below.

            // Concern ourselves only with `allow [source-type] [...];` policy statements where
            // `[source-type]` is associated with `source_type_id`.
            let source_attribute_bitmap: &ExtensibleBitmap =
                &self.attribute_maps[(source_type.0.get() - 1) as usize];
            if !source_attribute_bitmap.is_set(access_vector_rule.source_type().0.get() - 1) {
                continue;
            }

            // Concern ourselves only with `allow [source-type] [target-type][...];` policy
            // statements where `[target-type]` is associated with `target_type_id`.
            let target_attribute_bitmap: &ExtensibleBitmap =
                &self.attribute_maps[(target_type.0.get() - 1) as usize];
            if !target_attribute_bitmap.is_set(access_vector_rule.target_type().0.get() - 1) {
                continue;
            }

            // Multiple attributes may be associated with source/target types. Accumulate
            // explicitly allowed permissions into `computed_access_vector`.
            if let Some(access_vector) = access_vector_rule.access_vector() {
                if access_vector_rule.is_allow() {
                    // `access_vector` has bits set for each permission allowed by this rule.
                    computed_access_vector |= access_vector;
                } else if access_vector_rule.is_auditallow() {
                    // `access_vector` has bits set for each permission to audit when allowed.
                    computed_audit_allow |= access_vector;
                } else if access_vector_rule.is_dontaudit() {
                    // `access_vector` has bits cleared for each permission not to audit on denial.
                    computed_audit_deny &= access_vector;
                }
            }
        }

        // TODO: https://fxbug.dev/362706116 - Collate the auditallow & auditdeny sets.
        let mut flags = 0;
        if self.permissive_types().is_set(source_type.0.get()) {
            flags |= SELINUX_AVD_FLAGS_PERMISSIVE;
        }
        AccessDecision {
            allow: computed_access_vector,
            auditallow: computed_audit_allow,
            auditdeny: computed_audit_deny,
            flags,
            todo_bug: None,
        }
    }

    /// A permission is denied if it matches at least one unsatisfied constraint.
    fn compute_denied_by_constraints(
        &self,
        source_context: &SecurityContext,
        target_context: &SecurityContext,
        target_class: &Class,
    ) -> AccessVector {
        let mut denied = AccessVector::NONE;
        for constraint in target_class.constraints().iter() {
            match constraint.constraint_expr().evaluate(source_context, target_context) {
                Err(err) => {
                    unreachable!("validated constraint expression failed to evaluate: {:?}", err)
                }
                Ok(false) => denied |= constraint.access_vector(),
                Ok(true) => {}
            }
        }
        denied
    }

    /// Computes the ioctl extended permissions that should be allowed, audited when allowed, and
    /// audited when denied, for a given source context, target context, target class, and ioctl
    /// prefix byte.
    ///
    /// If there is an `allowxperm` rule for a particular source, target, and class, then only the
    /// named xperms should be allowed for that tuple. If there is no such `allowxperm` rule, then
    /// all xperms should be allowed for that tuple. (In both cases, the allow is conditional on the
    /// `ioctl` permission being allowed, but that should be checked separately before calling this
    /// function.)
    pub(super) fn compute_ioctl_access_decision(
        &self,
        source_context: &SecurityContext,
        target_context: &SecurityContext,
        target_class: &Class,
        ioctl_prefix: u8,
    ) -> IoctlAccessDecision {
        let target_class_id = target_class.id();

        let mut explicit_allow: Option<XpermsBitmap> = None;
        let mut auditallow = XpermsBitmap::NONE;
        let mut auditdeny = XpermsBitmap::ALL;

        for access_vector_rule in self.access_vector_rules.data.iter() {
            if !access_vector_rule.is_allowxperm()
                && !access_vector_rule.is_auditallowxperm()
                && !access_vector_rule.is_dontauditxperm()
            {
                continue;
            }
            if access_vector_rule.target_class() != target_class_id {
                continue;
            }
            let source_attribute_bitmap: &ExtensibleBitmap =
                &self.attribute_maps[(source_context.type_().0.get() - 1) as usize];
            if !source_attribute_bitmap.is_set(access_vector_rule.source_type().0.get() - 1) {
                continue;
            }
            let target_attribute_bitmap: &ExtensibleBitmap =
                &self.attribute_maps[(target_context.type_().0.get() - 1) as usize];
            if !target_attribute_bitmap.is_set(access_vector_rule.target_type().0.get() - 1) {
                continue;
            }

            if let Some(xperms) = access_vector_rule.extended_permissions() {
                // Only filter ioctls if there is at least one `allowxperm` rule for any ioctl
                // prefix.
                if access_vector_rule.is_allowxperm() {
                    explicit_allow.get_or_insert(XpermsBitmap::NONE);
                }
                // If the rule applies to ioctls with prefix `ioctl_prefix`, get a bitmap
                // of the ioctl postfixes named in the rule.
                let bitmap_if_prefix_matches = match xperms.xperms_type {
                    XPERMS_TYPE_IOCTL_PREFIX_AND_POSTFIXES => (xperms.xperms_optional_prefix
                        == ioctl_prefix)
                        .then_some(&xperms.xperms_bitmap),
                    XPERMS_TYPE_IOCTL_PREFIXES => {
                        xperms.xperms_bitmap.contains(ioctl_prefix).then_some(&XpermsBitmap::ALL)
                    }
                    _ => unreachable!("invalid xperms_type in validated ExtendedPermissions"),
                };
                let Some(xperms_bitmap) = bitmap_if_prefix_matches else {
                    continue;
                };
                if access_vector_rule.is_allowxperm() {
                    (*explicit_allow.get_or_insert(XpermsBitmap::NONE)) |= xperms_bitmap;
                }
                if access_vector_rule.is_auditallowxperm() {
                    auditallow |= xperms_bitmap;
                }
                if access_vector_rule.is_dontauditxperm() {
                    auditdeny -= xperms_bitmap;
                }
            }
        }
        let allow = explicit_allow.unwrap_or(XpermsBitmap::ALL);
        IoctlAccessDecision { allow, auditallow, auditdeny }
    }

    /// Returns the policy entry for the specified initial Security Context.
    pub(super) fn initial_context(&self, id: crate::InitialSid) -> &Context {
        let id = le::U32::from(id as u32);
        // [`InitialSids`] validates that all `InitialSid` values are defined by the policy.
        &self.initial_sids.data.iter().find(|initial| initial.id() == id).unwrap().context()
    }

    /// Returns the `User` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(super) fn user(&self, id: UserId) -> &User {
        self.users.data.iter().find(|x| x.id() == id).unwrap()
    }

    /// Returns the named user, if present in the policy.
    pub(super) fn user_by_name(&self, name: &str) -> Option<&User> {
        self.users.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    /// Returns the `Role` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(super) fn role(&self, id: RoleId) -> &Role {
        self.roles.data.iter().find(|x| x.id() == id).unwrap()
    }

    /// Returns the named role, if present in the policy.
    pub(super) fn role_by_name(&self, name: &str) -> Option<&Role> {
        self.roles.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    /// Returns the `Type` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(super) fn type_(&self, id: TypeId) -> &Type {
        self.types.data.iter().find(|x| x.id() == id).unwrap()
    }

    /// Returns the named type, if present in the policy.
    pub(super) fn type_by_name(&self, name: &str) -> Option<&Type> {
        self.types.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    /// Returns the extensible bitmap describing the set of types/domains for which permission
    /// checks are permissive.
    pub(super) fn permissive_types(&self) -> &ExtensibleBitmap {
        &self.permissive_map
    }

    /// Returns the `Sensitivity` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(super) fn sensitivity(&self, id: SensitivityId) -> &Sensitivity {
        self.sensitivities.data.iter().find(|x| x.id() == id).unwrap()
    }

    /// Returns the named sensitivity level, if present in the policy.
    pub(super) fn sensitivity_by_name(&self, name: &str) -> Option<&Sensitivity> {
        self.sensitivities.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    /// Returns the `Category` structure for the requested Id. Valid policies include definitions
    /// for all the Ids they refer to internally; supply some other Id will trigger a panic.
    pub(super) fn category(&self, id: CategoryId) -> &Category {
        self.categories.data.iter().find(|y| y.id() == id).unwrap()
    }

    /// Returns the named category, if present in the policy.
    pub(super) fn category_by_name(&self, name: &str) -> Option<&Category> {
        self.categories.data.iter().find(|x| x.name_bytes() == name.as_bytes())
    }

    pub(super) fn classes(&self) -> &Classes {
        &self.classes.data
    }

    pub(super) fn common_symbols(&self) -> &CommonSymbols {
        &self.common_symbols.data
    }

    pub(super) fn conditional_booleans(&self) -> &Vec<ConditionalBoolean> {
        &self.conditional_booleans.data
    }

    pub(super) fn fs_uses(&self) -> &FsUses {
        &self.fs_uses.data
    }

    pub(super) fn generic_fs_contexts(&self) -> &GenericFsContexts {
        &self.generic_fs_contexts.data
    }

    pub(super) fn role_allowlist(&self) -> &[RoleAllow] {
        &self.role_allowlist.data
    }

    pub(super) fn role_transitions(&self) -> &[RoleTransition] {
        &self.role_transitions.data
    }

    pub(super) fn range_transitions(&self) -> &RangeTransitions {
        &self.range_transitions.data
    }

    pub(super) fn access_vector_rules(&self) -> &AccessVectorRules {
        &self.access_vector_rules.data
    }

    pub(super) fn compute_filename_transition(
        &self,
        source_type: TypeId,
        target_type: TypeId,
        class: ClassId,
        name: NullessByteStr<'_>,
    ) -> Option<TypeId> {
        match &self.filename_transition_list {
            FilenameTransitionList::PolicyVersionGeq33(list) => {
                let entry = list.data.iter().find(|transition| {
                    transition.target_type() == target_type
                        && transition.target_class() == class
                        && transition.name_bytes() == name.as_bytes()
                })?;
                entry
                    .outputs()
                    .iter()
                    .find(|entry| entry.has_source_type(source_type))
                    .map(|x| x.out_type())
            }
            FilenameTransitionList::PolicyVersionLeq32(list) => list
                .data
                .iter()
                .find(|transition| {
                    transition.target_class() == class
                        && transition.target_type() == target_type
                        && transition.source_type() == source_type
                        && transition.name_bytes() == name.as_bytes()
                })
                .map(|x| x.out_type()),
        }
    }

    // Validate an MLS range statement against sets of defined sensitivity and category
    // IDs:
    // - Verify that all sensitivity and category IDs referenced in the MLS levels are
    //   defined.
    // - Verify that the range is internally consistent; i.e., the high level (if any)
    //   dominates the low level.
    fn validate_mls_range(
        &self,
        low_level: &MlsLevel,
        high_level: &Option<MlsLevel>,
        sensitivity_ids: &HashSet<SensitivityId>,
        category_ids: &HashSet<CategoryId>,
    ) -> Result<(), anyhow::Error> {
        validate_id(sensitivity_ids, low_level.sensitivity(), "sensitivity")?;
        for id in low_level.category_ids() {
            validate_id(category_ids, id, "category")?;
        }
        if let Some(high) = high_level {
            validate_id(sensitivity_ids, high.sensitivity(), "sensitivity")?;
            for id in high.category_ids() {
                validate_id(category_ids, id, "category")?;
            }
            if !high.dominates(low_level) {
                return Err(ValidateError::InvalidMlsRange {
                    low: low_level.serialize(self).into(),
                    high: high.serialize(self).into(),
                }
                .into());
            }
        }
        Ok(())
    }
}

impl ParsedPolicy
where
    Self: Parse,
{
    /// Parses the binary policy stored in `bytes`. It is an error for `bytes` to have trailing
    /// bytes after policy parsing completes.
    pub(super) fn parse(bytes: ByValue<Vec<u8>>) -> Result<(Self, Vec<u8>), anyhow::Error> {
        let (policy, tail) =
            <ParsedPolicy as Parse>::parse(bytes).map_err(Into::<anyhow::Error>::into)?;
        let num_bytes = tail.len();
        if num_bytes > 0 {
            return Err(ParseError::TrailingBytes { num_bytes }.into());
        }
        Ok((policy, tail.into_inner()))
    }
}

/// Parse a data structure from a prefix of a [`ParseStrategy`].
impl Parse for ParsedPolicy
where
    Signature: Parse,
    ExtensibleBitmap: Parse,
    SymbolList<CommonSymbol>: Parse,
    SymbolList<Class>: Parse,
    SymbolList<Role>: Parse,
    SymbolList<Type>: Parse,
    SymbolList<User>: Parse,
    SymbolList<ConditionalBoolean>: Parse,
    SymbolList<Sensitivity>: Parse,
    SymbolList<Category>: Parse,
    SimpleArray<AccessVectorRules>: Parse,
    SimpleArray<ConditionalNodes>: Parse,
    RoleTransitions: Parse,
    RoleAllows: Parse,
    SimpleArray<FilenameTransitions>: Parse,
    SimpleArray<DeprecatedFilenameTransitions>: Parse,
    SimpleArray<InitialSids>: Parse,
    SimpleArray<NamedContextPairs>: Parse,
    SimpleArray<Ports>: Parse,
    SimpleArray<Nodes>: Parse,
    SimpleArray<FsUses>: Parse,
    SimpleArray<IPv6Nodes>: Parse,
    SimpleArray<InfinitiBandPartitionKeys>: Parse,
    SimpleArray<InfinitiBandEndPorts>: Parse,
    SimpleArray<GenericFsContexts>: Parse,
    SimpleArray<RangeTransitions>: Parse,
{
    /// A [`Policy`] may add context to underlying [`ParseError`] values.
    type Error = anyhow::Error;

    /// Parses an entire binary policy.
    fn parse(bytes: ByValue<Vec<u8>>) -> Result<(Self, ByValue<Vec<u8>>), Self::Error> {
        let tail = bytes;

        let (magic, tail) = ByValue::parse::<Magic>(tail).context("parsing magic")?;

        let (signature, tail) = Signature::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing signature")?;

        let (policy_version, tail) =
            ByValue::parse::<PolicyVersion>(tail).context("parsing policy version")?;
        let policy_version_value = policy_version.policy_version();

        let (config, tail) = Config::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing policy config")?;

        let (counts, tail) =
            ByValue::parse::<Counts>(tail).context("parsing high-level policy object counts")?;

        let (policy_capabilities, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing policy capabilities")?;

        let (permissive_map, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing permissive map")?;

        let (common_symbols, tail) = SymbolList::<CommonSymbol>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing common symbols")?;

        let (classes, tail) = SymbolList::<Class>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing classes")?;

        let (roles, tail) = SymbolList::<Role>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing roles")?;

        let (types, tail) = SymbolList::<Type>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing types")?;

        let (users, tail) = SymbolList::<User>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing users")?;

        let (conditional_booleans, tail) = SymbolList::<ConditionalBoolean>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional booleans")?;

        let (sensitivities, tail) = SymbolList::<Sensitivity>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing sensitivites")?;

        let (categories, tail) = SymbolList::<Category>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing categories")?;

        let (access_vector_rules, tail) = SimpleArray::<AccessVectorRules>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing access vector rules")?;

        let (conditional_lists, tail) = SimpleArray::<ConditionalNodes>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional lists")?;

        let (role_transitions, tail) = RoleTransitions::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role transitions")?;

        let (role_allowlist, tail) = RoleAllows::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role allow rules")?;

        let (filename_transition_list, tail) = if policy_version_value >= 33 {
            let (filename_transition_list, tail) = SimpleArray::<FilenameTransitions>::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .context("parsing standard filename transitions")?;
            (FilenameTransitionList::PolicyVersionGeq33(filename_transition_list), tail)
        } else {
            let (filename_transition_list, tail) =
                SimpleArray::<DeprecatedFilenameTransitions>::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing deprecated filename transitions")?;
            (FilenameTransitionList::PolicyVersionLeq32(filename_transition_list), tail)
        };

        let (initial_sids, tail) = SimpleArray::<InitialSids>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing initial sids")?;

        let (filesystems, tail) = SimpleArray::<NamedContextPairs>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filesystem contexts")?;

        let (ports, tail) = SimpleArray::<Ports>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing ports")?;

        let (network_interfaces, tail) = SimpleArray::<NamedContextPairs>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing network interfaces")?;

        let (nodes, tail) = SimpleArray::<Nodes>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing nodes")?;

        let (fs_uses, tail) = SimpleArray::<FsUses>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing fs uses")?;

        let (ipv6_nodes, tail) = SimpleArray::<IPv6Nodes>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing ipv6 nodes")?;

        let (infinitiband_partition_keys, infinitiband_end_ports, tail) = if policy_version_value
            >= MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY
        {
            let (infinity_band_partition_keys, tail) =
                SimpleArray::<InfinitiBandPartitionKeys>::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing infiniti band partition keys")?;
            let (infinitiband_end_ports, tail) = SimpleArray::<InfinitiBandEndPorts>::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .context("parsing infiniti band end ports")?;
            (Some(infinity_band_partition_keys), Some(infinitiband_end_ports), tail)
        } else {
            (None, None, tail)
        };

        let (generic_fs_contexts, tail) = SimpleArray::<GenericFsContexts>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing generic filesystem contexts")?;

        let (range_transitions, tail) = SimpleArray::<RangeTransitions>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing range transitions")?;

        let primary_names_count = types.metadata.primary_names_count();
        let mut attribute_maps = Vec::with_capacity(primary_names_count as usize);
        let mut tail = tail;

        for i in 0..primary_names_count {
            let (item, next_tail) = ExtensibleBitmap::parse(tail)
                .map_err(Into::<anyhow::Error>::into)
                .with_context(|| format!("parsing {}th attribtue map", i))?;
            attribute_maps.push(item);
            tail = next_tail;
        }
        let tail = tail;
        let attribute_maps = attribute_maps;

        Ok((
            Self {
                magic,
                signature,
                policy_version,
                config,
                counts,
                policy_capabilities,
                permissive_map,
                common_symbols,
                classes,
                roles,
                types,
                users,
                conditional_booleans,
                sensitivities,
                categories,
                access_vector_rules,
                conditional_lists,
                role_transitions,
                role_allowlist,
                filename_transition_list,
                initial_sids,
                filesystems,
                ports,
                network_interfaces,
                nodes,
                fs_uses,
                ipv6_nodes,
                infinitiband_partition_keys,
                infinitiband_end_ports,
                generic_fs_contexts,
                range_transitions,
                attribute_maps,
            },
            tail,
        ))
    }
}

impl Validate for ParsedPolicy {
    /// A [`Policy`] may add context to underlying [`ValidateError`] values.
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        self.magic.validate().map_err(Into::<anyhow::Error>::into).context("validating magic")?;
        self.signature
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating signature")?;
        self.policy_version
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating policy_version")?;
        self.config.validate().map_err(Into::<anyhow::Error>::into).context("validating config")?;
        self.counts.validate().map_err(Into::<anyhow::Error>::into).context("validating counts")?;
        self.policy_capabilities
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating policy_capabilities")?;
        self.permissive_map
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating permissive_map")?;
        self.common_symbols
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating common_symbols")?;
        self.classes
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating classes")?;
        self.roles.validate().map_err(Into::<anyhow::Error>::into).context("validating roles")?;
        self.types.validate().map_err(Into::<anyhow::Error>::into).context("validating types")?;
        self.users.validate().map_err(Into::<anyhow::Error>::into).context("validating users")?;
        self.conditional_booleans
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating conditional_booleans")?;
        self.sensitivities
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating sensitivities")?;
        self.categories
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating categories")?;
        self.access_vector_rules
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating access_vector_rules")?;
        self.conditional_lists
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating conditional_lists")?;
        self.role_transitions
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating role_transitions")?;
        self.role_allowlist
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating role_allowlist")?;
        self.filename_transition_list
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating filename_transition_list")?;
        self.initial_sids
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating initial_sids")?;
        self.filesystems
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating filesystems")?;
        self.ports.validate().map_err(Into::<anyhow::Error>::into).context("validating ports")?;
        self.network_interfaces
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating network_interfaces")?;
        self.nodes.validate().map_err(Into::<anyhow::Error>::into).context("validating nodes")?;
        self.fs_uses
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating fs_uses")?;
        self.ipv6_nodes
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating ipv6 nodes")?;
        self.infinitiband_partition_keys
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating infinitiband_partition_keys")?;
        self.infinitiband_end_ports
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating infinitiband_end_ports")?;
        self.generic_fs_contexts
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating generic_fs_contexts")?;
        self.range_transitions
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating range_transitions")?;
        self.attribute_maps
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating attribute_maps")?;

        // Collate the sets of user, role, type, sensitivity and category Ids.
        let user_ids: HashSet<UserId> = self.users.data.iter().map(|x| x.id()).collect();
        let role_ids: HashSet<RoleId> = self.roles.data.iter().map(|x| x.id()).collect();
        let class_ids: HashSet<ClassId> = self.classes.data.iter().map(|x| x.id()).collect();
        let type_ids: HashSet<TypeId> = self.types.data.iter().map(|x| x.id()).collect();
        let sensitivity_ids: HashSet<SensitivityId> =
            self.sensitivities.data.iter().map(|x| x.id()).collect();
        let category_ids: HashSet<CategoryId> =
            self.categories.data.iter().map(|x| x.id()).collect();

        // Validate that users use only defined sensitivities and categories, and that
        // each user's MLS levels are internally consistent (i.e., the high level
        // dominates the low level).
        for user in &self.users.data {
            self.validate_mls_range(
                user.mls_range().low(),
                user.mls_range().high(),
                &sensitivity_ids,
                &category_ids,
            )?;
        }

        // Validate that initial contexts use only defined user, role, type, etc Ids.
        // Check that all sensitivity and category IDs are defined and that MLS levels
        // are internally consistent.
        for initial_sid in &self.initial_sids.data {
            let context = initial_sid.context();
            validate_id(&user_ids, context.user_id(), "user")?;
            validate_id(&role_ids, context.role_id(), "role")?;
            validate_id(&type_ids, context.type_id(), "type")?;
            self.validate_mls_range(
                context.low_level(),
                context.high_level(),
                &sensitivity_ids,
                &category_ids,
            )?;
        }

        // Validate that contexts specified in filesystem labeling rules only use
        // policy-defined Ids for their fields. Check that MLS levels are internally
        // consistent.
        for fs_use in &self.fs_uses.data {
            let context = fs_use.context();
            validate_id(&user_ids, context.user_id(), "user")?;
            validate_id(&role_ids, context.role_id(), "role")?;
            validate_id(&type_ids, context.type_id(), "type")?;
            self.validate_mls_range(
                context.low_level(),
                context.high_level(),
                &sensitivity_ids,
                &category_ids,
            )?;
        }

        // Validate that roles output by role- transitions & allows are defined.
        for transition in &self.role_transitions.data {
            validate_id(&role_ids, transition.current_role(), "current_role")?;
            validate_id(&type_ids, transition.type_(), "type")?;
            validate_id(&class_ids, transition.class(), "class")?;
            validate_id(&role_ids, transition.new_role(), "new_role")?;
        }
        for allow in &self.role_allowlist.data {
            validate_id(&role_ids, allow.source_role(), "source_role")?;
            validate_id(&role_ids, allow.new_role(), "new_role")?;
        }

        // Validate that types output by access vector rules are defined.
        for access_vector_rule in &self.access_vector_rules.data {
            if let Some(type_id) = access_vector_rule.new_type() {
                validate_id(&type_ids, type_id, "new_type")?;
            }
        }

        // Validate that constraints are well-formed by evaluating against
        // a source and target security context.
        let initial_context = SecurityContext::new_from_policy_context(
            self.initial_context(crate::InitialSid::Kernel),
        );
        for class in self.classes() {
            for constraint in class.constraints() {
                constraint
                    .constraint_expr()
                    .evaluate(&initial_context, &initial_context)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("validating constraints")?;
            }
        }

        // To-do comments for cross-policy validations yet to be implemented go here.
        // TODO(b/356569876): Determine which "bounds" should be verified for correctness here.

        Ok(())
    }
}

fn validate_id<IdType: Debug + Eq + Hash>(
    id_set: &HashSet<IdType>,
    id: IdType,
    debug_kind: &'static str,
) -> Result<(), anyhow::Error> {
    if !id_set.contains(&id) {
        return Err(ValidateError::UnknownId { kind: debug_kind, id: format!("{:?}", id) }.into());
    }
    Ok(())
}
