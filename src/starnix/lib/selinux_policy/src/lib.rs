// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod error;
pub mod metadata;
pub mod parser;

mod arrays;
mod extensible_bitmap;
mod symbols;

use {
    arrays::{
        AccessVectors, ConditionalNodes, DeprecatedFilenameTransitions, FilenameTransitionList,
        FilenameTransitions, FsUses, GenericFsContexts, IPv6Nodes, InfinitiBandEndPorts,
        InfinitiBandPartitionKeys, InitialSids, NamedContextPairs, Nodes, Ports, RangeTranslations,
        RoleAllows, RoleTransitions, SimpleArray,
        MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY,
    },
    error::ParseError,
    extensible_bitmap::ExtensibleBitmap,
    metadata::{Config, Counts, HandleUnknown, Magic, PolicyVersion, Signature},
    parser::{ByRef, ParseStrategy},
    symbols::{
        find_class_by_name, find_class_permission_by_name, find_type_alias_or_attribute_by_name,
        Category, Class, CommonSymbol, ConditionalBoolean, Role, Sensitivity, SymbolList, Type,
        User,
    },
};

use anyhow::Context as _;
use once_cell::sync::Lazy;
use parser::ByValue;
use std::{collections::BTreeMap, fmt::Debug, marker::PhantomData, ops::Deref};
use zerocopy::{little_endian as le, ByteSlice, FromBytes, NoCell, Ref, Unaligned};

/// Maximum SELinux policy version supported by this implementation.
pub const SUPPORTED_POLICY_VERSION: u32 = 33;

/// Binary policy SIDs that may be referenced in the policy without be explicitly introduced in the
/// policy because they are hard-coded in the Linux kernel.
///
/// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
pub(crate) static INITIAL_SIDS_IDENTIFIERS: Lazy<BTreeMap<u32, &'static [u8]>> = Lazy::new(|| {
    BTreeMap::<u32, &'static [u8]>::from([
        (1, b"kernel".as_slice()),
        (2, b"security".as_slice()),
        (3, b"unlabeled".as_slice()),
        (4, b"fs".as_slice()),
        (5, b"file".as_slice()),
        (6, b"file_labels".as_slice()),
        (7, b"init".as_slice()),
        (8, b"any_socket".as_slice()),
        (9, b"port".as_slice()),
        (10, b"netif".as_slice()),
        (11, b"netmsg".as_slice()),
        (12, b"node".as_slice()),
        (13, b"igmp_packet".as_slice()),
        (14, b"icmp_socket".as_slice()),
        (15, b"tcp_socket".as_slice()),
        (16, b"sysctl_modprobe".as_slice()),
        (17, b"sysctl".as_slice()),
        (18, b"sysctl_fs".as_slice()),
        (19, b"sysctl_kernel".as_slice()),
        (20, b"sysctl_net".as_slice()),
        (21, b"sysctl_net_unix".as_slice()),
        (22, b"sysctl_vm".as_slice()),
        (23, b"sysctl_dev".as_slice()),
        (24, b"kmod".as_slice()),
        (25, b"policy".as_slice()),
        (26, b"scmp_packet".as_slice()),
        (27, b"devnull".as_slice()),
    ])
});

/// Parses `binary_policy` by value; that is, copies underlying binary data out in addition to
/// building up parser output structures. This function returns
/// `(unvalidated_parser_output, binary_policy)` on success, or an error if parsing failed. Note
/// that the second component of the success case contains precisely the same bytes as the input.
/// This function depends on a uniformity of interface between the "by value" and "by reference"
/// strategies, but also requires an `unvalidated_parser_output` type that is independent of the
/// `binary_policy` lifetime. Taken together, these requirements demand the "move-in + move-out"
/// interface for `binary_policy`.
///
/// If the caller does not need access to the binary policy when parsing fails, but does need to
/// retain both the parsed output and the binary policy when parsing succeeds, the code will look
/// something like:
///
/// ```rust,ignore
/// let (unvalidated_policy, binary_policy) = parse_policy_by_value(binary_policy)?;
/// ```
///
/// If the caller does need access to the binary policy when parsing fails and needs to retain both
/// parsed output and the binary policy when parsing succeeds, the code will look something like:
///
/// ```rust,ignore
/// let (unvalidated_policy, _) = parse_policy_by_value(binary_policy.clone())?;
/// ```
///
/// If the caller does not need to retain both the parsed output and the binary policy, then
/// [`parse_policy_by_reference`] should be used instead.
pub fn parse_policy_by_value(
    binary_policy: Vec<u8>,
) -> Result<(Unvalidated<ByValue<Vec<u8>>>, Vec<u8>), anyhow::Error> {
    Policy::parse(ByValue::new(binary_policy))
}

/// Parses `binary_policy` by reference; that is, constructs parser output structures that contain
/// _references_ to data in `binary_policy`. This function returns `unvalidated_parser_output` on
/// success, or an error if parsing failed.
///
/// If the caller does needs to retain both the parsed output and the binary policy, then
/// [`parse_policy_by_value`] should be used instead.
pub fn parse_policy_by_reference<'a>(
    binary_policy: &'a [u8],
) -> Result<Unvalidated<ByRef<&'a [u8]>>, anyhow::Error> {
    let (unvalidated_policy, _) = Policy::parse(ByRef::new(binary_policy))?;
    Ok(unvalidated_policy)
}

/// A parsed binary policy.
///
/// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
#[derive(Debug)]
pub struct Policy<PS: ParseStrategy> {
    /// A distinctive number that acts as a binary format-specific header for SELinux binary policy
    /// files.
    magic: PS::Output<Magic>,
    /// A length-encoded string, "SE Linux", which identifies this policy as an SE Linux policy.
    signature: Signature<PS>,
    /// The policy format version number. Different version may support different policy features.
    policy_version: PS::Output<PolicyVersion>,
    /// Whole-policy configuration, such as how to handle queries against unknown classes.
    config: Config<PS>,
    /// High-level counts of subsequent policy elements.
    counts: PS::Output<Counts>,
    policy_capabilities: ExtensibleBitmap<PS>,
    permissive_map: ExtensibleBitmap<PS>,
    /// Common permissions that can be mixed in to classes.
    common_symbols: SymbolList<PS, CommonSymbol<PS>>,
    /// The set of classes referenced by this policy.
    classes: SymbolList<PS, Class<PS>>,
    /// The set of roles referenced by this policy.
    roles: SymbolList<PS, Role<PS>>,
    /// The set of types referenced by this policy.
    types: SymbolList<PS, Type<PS>>,
    /// The set of users referenced by this policy.
    users: SymbolList<PS, User<PS>>,
    /// The set of dynamically adjustable booleans referenced by this policy.
    conditional_booleans: SymbolList<PS, ConditionalBoolean<PS>>,
    /// The set of sensitivity levels referenced by this policy.
    sensitivities: SymbolList<PS, Sensitivity<PS>>,
    /// The set of categories referenced by this policy.
    categories: SymbolList<PS, Category<PS>>,
    /// The set of access vectors referenced by this policy.
    access_vectors: SimpleArray<PS, AccessVectors<PS>>,
    conditional_lists: SimpleArray<PS, ConditionalNodes<PS>>,
    role_transitions: RoleTransitions<PS>,
    role_allowlist: RoleAllows<PS>,
    filename_transition_list: FilenameTransitionList<PS>,
    initial_sids: SimpleArray<PS, InitialSids<PS>>,
    filesystems: SimpleArray<PS, NamedContextPairs<PS>>,
    ports: SimpleArray<PS, Ports<PS>>,
    network_interfaces: SimpleArray<PS, NamedContextPairs<PS>>,
    nodes: SimpleArray<PS, Nodes<PS>>,
    fs_uses: SimpleArray<PS, FsUses<PS>>,
    ipv6_nodes: SimpleArray<PS, IPv6Nodes<PS>>,
    infinitiband_partition_keys: Option<SimpleArray<PS, InfinitiBandPartitionKeys<PS>>>,
    infinitiband_end_ports: Option<SimpleArray<PS, InfinitiBandEndPorts<PS>>>,
    generic_fs_contexts: SimpleArray<PS, GenericFsContexts<PS>>,
    range_translations: SimpleArray<PS, RangeTranslations<PS>>,
    /// Extensible bitmaps that encode associations between types and attributes.
    attribute_maps: Vec<ExtensibleBitmap<PS>>,
}

impl<PS: ParseStrategy> Policy<PS>
where
    Self: Parse<PS>,
{
    /// Parses the binary policy stored in `bytes`. It is an error for `bytes` to have trailing
    /// bytes after policy parsing completes.
    fn parse(bytes: PS) -> Result<(Unvalidated<PS>, PS::Input), anyhow::Error> {
        let (policy, tail) =
            <Policy<PS> as Parse<PS>>::parse(bytes).map_err(Into::<anyhow::Error>::into)?;
        let num_bytes = tail.len();
        if num_bytes > 0 {
            return Err(ParseError::TrailingBytes { num_bytes }.into());
        }
        Ok((Unvalidated(policy), tail.into_inner()))
    }

    /// The policy version stored in the underlying binary policy.
    pub fn policy_version(&self) -> u32 {
        PS::deref(&self.policy_version).policy_version()
    }

    /// The way "unknown" policy decisions should be handed according to the underlying binary
    /// policy.
    pub fn handle_unknown(&self) -> &HandleUnknown {
        self.config.handle_unknown()
    }

    /// Returns whether the input access vector is explicitly allowed by some `allow [...];` policy
    /// statement, or an error if lookups for input query strings fail.
    pub fn is_explicitly_allowed(
        &self,
        source_type_name: &str,
        target_type_name: &str,
        target_class_name: &str,
        permission_name: &str,
    ) -> Result<bool, ()> {
        let source_type =
            find_type_alias_or_attribute_by_name(&self.types.data, source_type_name).ok_or(())?;
        let target_type =
            find_type_alias_or_attribute_by_name(&self.types.data, target_type_name).ok_or(())?;
        let target_class = find_class_by_name(&self.classes.data, target_class_name).ok_or(())?;
        let permission =
            find_class_permission_by_name(&self.common_symbols.data, target_class, permission_name)
                .ok_or(())?;
        let permission_value = permission.value();
        let permission_bit = (1 as u32) << (permission_value - 1);

        let source_type_value = source_type.value();
        let target_type_value = target_type.value();
        let target_class_value = target_class.value();

        for access_vector in self.access_vectors.data.iter() {
            // Concern ourselves only with explicit `allow [...];` policy statements.
            if !access_vector.is_allow() {
                continue;
            }

            // Concern ourselves only with `allow [source-type] [target-type]:[class] [...];`
            // policy statements where `[class]` matches `target_class_value`.
            if access_vector.target_class() != target_class_value as u16 {
                continue;
            }

            // Concern ourselves only with
            // `allow [source-type] [target-type]:[class] { [permissions] };` policy statements
            // where `permission_bit` refers to one of `[permissions]`.
            match access_vector.permission_mask() {
                Some(mask) => {
                    if (mask & permission_bit) == 0 {
                        continue;
                    }
                }
                None => continue,
            }

            // Note: Perform bitmap lookups last: they are the most expensive comparison operation.

            // Note: Type values start at 1, but are 0-indexed in bitmaps: hence the `type - 1` bitmap
            // lookups below.

            // Concern ourselves only with `allow [source-type] [...];` policy statements where
            // `[source-type]` is associated with `source_type_value`.
            let source_attribute_bitmap: &ExtensibleBitmap<PS> =
                &self.attribute_maps[(source_type_value - 1) as usize];
            if !source_attribute_bitmap.is_set((access_vector.source_type() - 1) as u32) {
                continue;
            }

            // Concern ourselves only with `allow [source-type] [target-type][...];` policy
            // statements where `[target-type]` is associated with `target_type_value`.
            let target_attribute_bitmap: &ExtensibleBitmap<PS> =
                &self.attribute_maps[(target_type_value - 1) as usize];
            if !target_attribute_bitmap.is_set((access_vector.target_type() - 1) as u32) {
                continue;
            }

            // `access_vector` explicitly allows the source, target, permission in this query.
            return Ok(true);
        }

        // Failed to find any explicit-allow access vector for this source, target, permission
        // query.
        Ok(false)
    }
}

/// Parse a data structure from a prefix of a [`ParseStrategy`].
impl<PS: ParseStrategy> Parse<PS> for Policy<PS>
where
    Signature<PS>: Parse<PS>,
    ExtensibleBitmap<PS>: Parse<PS>,
    SymbolList<PS, CommonSymbol<PS>>: Parse<PS>,
    SymbolList<PS, Class<PS>>: Parse<PS>,
    SymbolList<PS, Role<PS>>: Parse<PS>,
    SymbolList<PS, Type<PS>>: Parse<PS>,
    SymbolList<PS, User<PS>>: Parse<PS>,
    SymbolList<PS, ConditionalBoolean<PS>>: Parse<PS>,
    SymbolList<PS, Sensitivity<PS>>: Parse<PS>,
    SymbolList<PS, Category<PS>>: Parse<PS>,
    SimpleArray<PS, AccessVectors<PS>>: Parse<PS>,
    SimpleArray<PS, ConditionalNodes<PS>>: Parse<PS>,
    RoleTransitions<PS>: Parse<PS>,
    RoleAllows<PS>: Parse<PS>,
    SimpleArray<PS, FilenameTransitions<PS>>: Parse<PS>,
    SimpleArray<PS, DeprecatedFilenameTransitions<PS>>: Parse<PS>,
    SimpleArray<PS, InitialSids<PS>>: Parse<PS>,
    SimpleArray<PS, NamedContextPairs<PS>>: Parse<PS>,
    SimpleArray<PS, Ports<PS>>: Parse<PS>,
    SimpleArray<PS, NamedContextPairs<PS>>: Parse<PS>,
    SimpleArray<PS, Nodes<PS>>: Parse<PS>,
    SimpleArray<PS, FsUses<PS>>: Parse<PS>,
    SimpleArray<PS, IPv6Nodes<PS>>: Parse<PS>,
    SimpleArray<PS, InfinitiBandPartitionKeys<PS>>: Parse<PS>,
    SimpleArray<PS, InfinitiBandEndPorts<PS>>: Parse<PS>,
    SimpleArray<PS, GenericFsContexts<PS>>: Parse<PS>,
    SimpleArray<PS, RangeTranslations<PS>>: Parse<PS>,
{
    /// A [`Policy`] may add context to underlying [`ParseError`] values.
    type Error = anyhow::Error;

    /// Parses an entire binary policy.
    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (magic, tail) = PS::parse::<Magic>(tail).context("parsing magic")?;

        let (signature, tail) = Signature::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing signature")?;

        let (policy_version, tail) =
            PS::parse::<PolicyVersion>(tail).context("parsing policy version")?;
        let policy_version_value = PS::deref(&policy_version).policy_version();

        let (config, tail) = Config::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing policy config")?;

        let (counts, tail) =
            PS::parse::<Counts>(tail).context("parsing high-level policy object counts")?;

        let (policy_capabilities, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing policy capabilities")?;

        let (permissive_map, tail) = ExtensibleBitmap::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing permissive map")?;

        let (common_symbols, tail) = SymbolList::<PS, CommonSymbol<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing common symbols")?;

        let (classes, tail) = SymbolList::<PS, Class<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing classes")?;

        let (roles, tail) = SymbolList::<PS, Role<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing roles")?;

        let (types, tail) = SymbolList::<PS, Type<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing types")?;

        let (users, tail) = SymbolList::<PS, User<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing users")?;

        let (conditional_booleans, tail) = SymbolList::<PS, ConditionalBoolean<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional booleans")?;

        let (sensitivities, tail) = SymbolList::<PS, Sensitivity<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing sensitivites")?;

        let (categories, tail) = SymbolList::<PS, Category<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing categories")?;

        let (access_vectors, tail) = SimpleArray::<PS, AccessVectors<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing access vectors")?;

        let (conditional_lists, tail) = SimpleArray::<PS, ConditionalNodes<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing conditional lists")?;

        let (role_transitions, tail) = RoleTransitions::<PS>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role transitions")?;

        let (role_allowlist, tail) = RoleAllows::<PS>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing role allow rules")?;

        let (filename_transition_list, tail) = if policy_version_value >= 33 {
            let (filename_transition_list, tail) =
                SimpleArray::<PS, FilenameTransitions<PS>>::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing standard filename transitions")?;
            (FilenameTransitionList::PolicyVersionGeq33(filename_transition_list), tail)
        } else {
            let (filename_transition_list, tail) =
                SimpleArray::<PS, DeprecatedFilenameTransitions<PS>>::parse(tail)
                    .map_err(Into::<anyhow::Error>::into)
                    .context("parsing deprecated filename transitions")?;
            (FilenameTransitionList::PolicyVersionLeq32(filename_transition_list), tail)
        };

        let (initial_sids, tail) = SimpleArray::<PS, InitialSids<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing initial sids")?;

        let (filesystems, tail) = SimpleArray::<PS, NamedContextPairs<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing filesystem contexts")?;

        let (ports, tail) = SimpleArray::<PS, Ports<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing ports")?;

        let (network_interfaces, tail) = SimpleArray::<PS, NamedContextPairs<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing network interfaces")?;

        let (nodes, tail) = SimpleArray::<PS, Nodes<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing nodes")?;

        let (fs_uses, tail) = SimpleArray::<PS, FsUses<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing fs uses")?;

        let (ipv6_nodes, tail) = SimpleArray::<PS, IPv6Nodes<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing ipv6 nodes")?;

        let (infinitiband_partition_keys, infinitiband_end_ports, tail) =
            if policy_version_value >= MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY {
                let (infinity_band_partition_keys, tail) =
                    SimpleArray::<PS, InfinitiBandPartitionKeys<PS>>::parse(tail)
                        .map_err(Into::<anyhow::Error>::into)
                        .context("parsing infiniti band partition keys")?;
                let (infinitiband_end_ports, tail) =
                    SimpleArray::<PS, InfinitiBandEndPorts<PS>>::parse(tail)
                        .map_err(Into::<anyhow::Error>::into)
                        .context("parsing infiniti band end ports")?;
                (Some(infinity_band_partition_keys), Some(infinitiband_end_ports), tail)
            } else {
                (None, None, tail)
            };

        let (generic_fs_contexts, tail) = SimpleArray::<PS, GenericFsContexts<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing generic filesystem contexts")?;

        let (range_translations, tail) = SimpleArray::<PS, RangeTranslations<PS>>::parse(tail)
            .map_err(Into::<anyhow::Error>::into)
            .context("parsing range translations")?;

        let primary_names_count = PS::deref(&types.metadata).primary_names_count();
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
                access_vectors,
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
                range_translations,
                attribute_maps,
            },
            tail,
        ))
    }
}

impl<PS: ParseStrategy> Validate for Policy<PS> {
    /// A [`Policy`] may add context to underlying [`ValidateError`] values.
    type Error = anyhow::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        PS::deref(&self.magic)
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating magic")?;
        self.signature
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating signature")?;
        PS::deref(&self.policy_version)
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating policy_version")?;
        self.config.validate().map_err(Into::<anyhow::Error>::into).context("validating config")?;
        PS::deref(&self.counts)
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating counts")?;
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
        self.access_vectors
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating access_vectors")?;
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
        self.range_translations
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating range_translations")?;
        self.attribute_maps
            .validate()
            .map_err(Into::<anyhow::Error>::into)
            .context("validating attribute_maps")?;

        Ok(())
    }
}

/// A [`Policy`] that has been successfully parsed, but not validated.
pub struct Unvalidated<PS: ParseStrategy>(Policy<PS>);

impl<PS: ParseStrategy> Unvalidated<PS> {
    pub fn validate(self) -> Result<Policy<PS>, anyhow::Error> {
        Validate::validate(&self.0)?;
        Ok(self.0)
    }
}

/// A data structure that can be parsed as a part of a binary policy.
pub trait Parse<PS: ParseStrategy>: Sized {
    /// The type of error that may be returned from `parse()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Parses a `Self` from `bytes`, returning the `Self` and trailing bytes, or an error if
    /// bytes corresponding to a `Self` are malformed.
    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error>;
}

/// Parse a data as a slice of inner data structures from a prefix of a [`ByteSlice`].
pub(crate) trait ParseSlice<PS: ParseStrategy>: Sized {
    /// The type of error that may be returned from `parse()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Parses a `Self` as `count` of internal itemsfrom `bytes`, returning the `Self` and trailing
    /// bytes, or an error if bytes corresponding to a `Self` are malformed.
    fn parse_slice(bytes: PS, count: usize) -> Result<(Self, PS), Self::Error>;
}

/// Validate a parsed data structure.
pub(crate) trait Validate {
    /// The type of error that may be returned from `validate()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Validates a `Self`, returning a `Self::Error` if `self` is internally inconsistent.
    fn validate(&self) -> Result<(), Self::Error>;
}

pub(crate) trait ValidateArray<M, D> {
    /// The type of error that may be returned from `validate()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Validates a `Self`, returning a `Self::Error` if `self` is internally inconsistent.
    fn validate_array<'a>(metadata: &'a M, data: &'a [D]) -> Result<(), Self::Error>;
}

/// Treat a type as metadata that contains a count of subsequent data.
pub(crate) trait Counted {
    /// Returns the count of subsequent data items.
    fn count(&self) -> u32;
}

impl<T: Validate> Validate for Option<T> {
    type Error = <T as Validate>::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        match self {
            Some(value) => value.validate(),
            None => Ok(()),
        }
    }
}

impl Validate for le::U32 {
    type Error = anyhow::Error;

    /// Using a raw `le::U32` implies no additional constraints on its value. To operate with
    /// constraints, define a `struct T(le::U32);` and `impl Validate for T { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for u8 {
    type Error = anyhow::Error;

    /// Using a raw `u8` implies no additional constraints on its value. To operate with
    /// constraints, define a `struct T(u8);` and `impl Validate for T { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [u8] {
    type Error = anyhow::Error;

    /// Using a raw `[u8]` implies no additional constraints on its value. To operate with
    /// constraints, define a `struct T([u8]);` and `impl Validate for T { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice, T: Validate + FromBytes + NoCell> Validate for Ref<B, T> {
    type Error = <T as Validate>::Error;

    fn validate(&self) -> Result<(), Self::Error> {
        self.deref().validate()
    }
}

impl<B: ByteSlice, T: Counted + FromBytes + NoCell> Counted for Ref<B, T> {
    fn count(&self) -> u32 {
        self.deref().count()
    }
}

/// A length-encoded array that contains metadata in `M` and a slice of data items internally
/// managed by `D`.
#[derive(Clone, Debug, PartialEq)]
struct Array<PS, M, D> {
    metadata: M,
    data: D,
    _marker: PhantomData<PS>,
}

impl<PS: ParseStrategy, M: Counted + Parse<PS>, D: ParseSlice<PS>> Parse<PS> for Array<PS, M, D> {
    /// [`Array`] abstracts over two types (`M` and `D`) that may have different [`Parse::Error`]
    /// types. Unify error return type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses [`Array`] by parsing *and validating* `metadata`, `data`, and `self`.
    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = M::parse(tail).map_err(Into::<anyhow::Error>::into)?;

        let (data, tail) =
            D::parse_slice(tail, metadata.count() as usize).map_err(Into::<anyhow::Error>::into)?;

        let array = Self { metadata, data, _marker: PhantomData };

        Ok((array, tail))
    }
}

impl<
        T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned,
        PS: ParseStrategy<Output<T> = T>,
    > Parse<PS> for T
{
    type Error = anyhow::Error;

    fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) = PS::parse::<T>(bytes).ok_or(ParseError::MissingData {
            type_name: std::any::type_name::<T>(),
            type_size: std::mem::size_of::<T>(),
            num_bytes,
        })?;

        Ok((data, tail))
    }
}

/// Defines a at type that wraps an [`Array`], implementing `Deref`-as-`Array` and [`Parse`]. This
/// macro should be used in contexts where using a general [`Array`] implementation may introduce
/// conflicting implementations on account of general [`Array`] type parameters.
macro_rules! array_type {
    ($type_name:ident, $parse_strategy:ident, $metadata_type:ty, $data_type:ty, $metadata_type_name:expr, $data_type_name:expr) => {
        #[doc = "An [`Array`] with [`"]
        #[doc = $metadata_type_name]
        #[doc = "`] metadata and [`"]
        #[doc = $data_type_name]
        #[doc = "`] data items."]
        #[derive(Debug, PartialEq)]
        pub(crate) struct $type_name<$parse_strategy: crate::parser::ParseStrategy>(
            crate::Array<PS, $metadata_type, $data_type>,
        );

        impl<PS: crate::parser::ParseStrategy> std::ops::Deref for $type_name<PS> {
            type Target = crate::Array<PS, $metadata_type, $data_type>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl<PS: crate::parser::ParseStrategy> crate::Parse<PS> for $type_name<PS>
        where
            Array<PS, $metadata_type, $data_type>: crate::Parse<PS>,
        {
            type Error = <Array<PS, $metadata_type, $data_type> as crate::Parse<PS>>::Error;

            fn parse(bytes: PS) -> Result<(Self, PS), Self::Error> {
                let (array, tail) = Array::<PS, $metadata_type, $data_type>::parse(bytes)?;
                Ok((Self(array), tail))
            }
        }
    };

    ($type_name:ident, $parse_strategy:ident, $metadata_type:ty, $data_type:ty) => {
        array_type!(
            $type_name,
            $parse_strategy,
            $metadata_type,
            $data_type,
            stringify!($metadata_type),
            stringify!($data_type)
        );
    };
}

pub(crate) use array_type;

macro_rules! array_type_validate_deref_both {
    ($type_name:ident) => {
        impl<PS: crate::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = PS::deref(&self.metadata);
                metadata.validate()?;

                let data = PS::deref_slice(&self.data);
                data.validate()?;

                Self::validate_array(metadata, data).map_err(Into::<anyhow::Error>::into)
            }
        }
    };
}

pub(crate) use array_type_validate_deref_both;

macro_rules! array_type_validate_deref_data {
    ($type_name:ident) => {
        impl<PS: crate::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = &self.metadata;
                metadata.validate()?;

                let data = PS::deref_slice(&self.data);
                data.validate()?;

                Self::validate_array(metadata, data)
            }
        }
    };
}

pub(crate) use array_type_validate_deref_data;

macro_rules! array_type_validate_deref_metadata_data_vec {
    ($type_name:ident) => {
        impl<PS: crate::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = PS::deref(&self.metadata);
                metadata.validate()?;

                let data = &self.data;
                data.validate()?;

                Self::validate_array(metadata, data.as_slice())
            }
        }
    };
}

pub(crate) use array_type_validate_deref_metadata_data_vec;

macro_rules! array_type_validate_deref_none_data_vec {
    ($type_name:ident) => {
        impl<PS: crate::parser::ParseStrategy> Validate for $type_name<PS> {
            type Error = anyhow::Error;

            fn validate(&self) -> Result<(), Self::Error> {
                let metadata = &self.metadata;
                metadata.validate()?;

                let data = &self.data;
                data.validate()?;

                Self::validate_array(metadata, data.as_slice())
            }
        }
    };
}

pub(crate) use array_type_validate_deref_none_data_vec;

impl<
        B: Debug + ByteSlice + PartialEq,
        T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned,
    > Parse<ByRef<B>> for Ref<B, T>
{
    type Error = anyhow::Error;

    fn parse(bytes: ByRef<B>) -> Result<(Self, ByRef<B>), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) = ByRef::<B>::parse::<T>(bytes).ok_or(ParseError::MissingData {
            type_name: std::any::type_name::<T>(),
            type_size: std::mem::size_of::<T>(),
            num_bytes,
        })?;

        Ok((data, tail))
    }
}

impl<
        B: Debug + ByteSlice + PartialEq,
        T: Clone + Debug + FromBytes + NoCell + PartialEq + Unaligned,
    > ParseSlice<ByRef<B>> for Ref<B, [T]>
{
    /// [`Ref`] may return a [`ParseError`] internally, or `<T as Parse>::Error`. Unify error return
    /// type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses [`Ref`] by consuming it as an unaligned prefix as a slice, then validating the slice
    /// via `Ref::deref`.
    fn parse_slice(bytes: ByRef<B>, count: usize) -> Result<(Self, ByRef<B>), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) =
            ByRef::<B>::parse_slice::<T>(bytes, count).ok_or(ParseError::MissingSliceData {
                type_name: std::any::type_name::<T>(),
                type_size: std::mem::size_of::<T>(),
                num_items: count,
                num_bytes,
            })?;

        Ok((data, tail))
    }
}

impl<PS: ParseStrategy, T: Parse<PS>> ParseSlice<PS> for Vec<T> {
    /// `Vec<T>` may return a [`ParseError`] internally, or `<T as Parse>::Error`. Unify error
    /// return type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses `Vec<T>` by parsing individual `T` instances, then validating them.
    fn parse_slice(bytes: PS, count: usize) -> Result<(Self, PS), Self::Error> {
        let mut slice = Vec::with_capacity(count);
        let mut tail = bytes;

        for _ in 0..count {
            let (item, next_tail) = T::parse(tail).map_err(Into::<anyhow::Error>::into)?;
            slice.push(item);
            tail = next_tail;
        }

        Ok((slice, tail))
    }
}

#[cfg(test)]
pub(crate) mod test {
    use super::*;

    use super::error::ValidateError;

    /// Downcasts an [`anyhow::Error`] to a [`ParseError`] for structured error comparison in tests.
    pub fn as_parse_error(error: anyhow::Error) -> ParseError {
        error.downcast::<ParseError>().expect("parse error")
    }

    /// Downcasts an [`anyhow::Error`] to a [`ParseError`] for structured error comparison in tests.
    pub fn as_validate_error(error: anyhow::Error) -> ValidateError {
        error.downcast::<ValidateError>().expect("validate error")
    }

    macro_rules! parse_test {
        ($parse_output:ident, $data:expr, $result:tt, $check_impl:block) => {{
            let data = $data;
            fn check_by_ref<'a>(
                $result: Result<
                    ($parse_output<ByRef<&'a [u8]>>, ByRef<&'a [u8]>),
                    <$parse_output<ByRef<&'a [u8]>> as crate::Parse<ByRef<&'a [u8]>>>::Error,
                >,
            ) {
                $check_impl;
            }

            fn check_by_value(
                $result: Result<
                    ($parse_output<ByValue<Vec<u8>>>, ByValue<Vec<u8>>),
                    <$parse_output<ByValue<Vec<u8>>> as crate::Parse<ByValue<Vec<u8>>>>::Error,
                >,
            ) -> Option<($parse_output<ByValue<Vec<u8>>>, ByValue<Vec<u8>>)> {
                $check_impl
            }

            let by_ref = ByRef::new(data.as_slice());
            let by_ref_result = $parse_output::parse(by_ref);
            check_by_ref(by_ref_result);
            let by_value_result = $parse_output::<ByValue<Vec<u8>>>::parse(ByValue::new(data));
            let _ = check_by_value(by_value_result);
        }};
    }

    pub(crate) use parse_test;

    macro_rules! validate_test {
        ($parse_output:ident, $data:expr, $result:tt, $check_impl:block) => {{
            let data = $data;
            fn check_by_ref<'a>(
                $result: Result<(), <$parse_output<ByRef<&'a [u8]>> as crate::Validate>::Error>,
            ) {
                $check_impl;
            }

            fn check_by_value(
                $result: Result<(), <$parse_output<ByValue<Vec<u8>>> as crate::Validate>::Error>,
            ) {
                $check_impl
            }

            let by_ref = ByRef::new(data.as_slice());
            let (by_ref_parsed, _) =
                $parse_output::parse(by_ref).expect("successful parse for validate test");
            let by_ref_result = by_ref_parsed.validate();
            check_by_ref(by_ref_result);
            let (by_value_parsed, _) = $parse_output::<ByValue<Vec<u8>>>::parse(ByValue::new(data))
                .expect("successful parse for validate test");
            let by_value_result = by_value_parsed.validate();
            check_by_value(by_value_result);
        }};
    }

    pub(crate) use validate_test;
}
