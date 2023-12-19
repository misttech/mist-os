// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod error;
pub mod metadata;

mod arrays;
mod extensible_bitmap;
mod symbols;

use {
    arrays::{
        AccessVectors, ConditionalNodes, DeprecatedFilenameTransitions, FilenameTransitionList,
        FilenameTransitions, FsUses, GenericFsContexts, IPv6Nodes, InfinitiBandEndPorts,
        InfinitiBandPartitionKeys, InitialSids, NamedContextPairs, Nodes, Ports, RangeTranslations,
        RoleAllow, RoleTransition, SimpleArray, MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY,
    },
    error::ParseError,
    extensible_bitmap::ExtensibleBitmap,
    metadata::{Config, Counts, HandleUnknown, Magic, PolicyVersion, Signature},
    symbols::{
        Categories, Classes, CommonSymbols, ConditionalBooleans, Roles, Sensitivities, SymbolList,
        Types, Users,
    },
};

use anyhow::Context as _;
use once_cell::sync::Lazy;
use std::{collections::BTreeMap, fmt::Debug, marker::PhantomData, ops::Deref};
use zerocopy::{ByteSlice, FromBytes, NoCell, Ref, Unaligned};

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

/// A parsed binary policy.
///
/// TODO: Eliminate `dead_code` guard.
#[allow(dead_code)]
#[derive(Debug)]
pub struct Policy<B: ByteSlice + Debug + PartialEq> {
    /// A distinctive number that acts as a binary format-specific header for SELinux binary policy
    /// files.
    magic: Ref<B, Magic>,
    /// A length-encoded string, "SE Linux", which identifies this policy as an SE Linux policy.
    signature: Signature<B>,
    /// The policy format version number. Different version may support different policy features.
    policy_version: Ref<B, PolicyVersion>,
    /// Whole-policy configuration, such as how to handle queries against unknown classes.
    config: Config<B>,
    /// High-level counts of subsequent policy elements.
    counts: Ref<B, Counts>,
    policy_capabilities: ExtensibleBitmap<B>,
    permissive_map: ExtensibleBitmap<B>,
    /// Common permissions that can be mixed in to classes.
    common_symbols: SymbolList<B, CommonSymbols<B>>,
    /// The set of classes referenced by this policy.
    classes: SymbolList<B, Classes<B>>,
    /// The set of roles referenced by this policy.
    roles: SymbolList<B, Roles<B>>,
    /// The set of types referenced by this policy.
    types: SymbolList<B, Types<B>>,
    /// The set of users referenced by this policy.
    users: SymbolList<B, Users<B>>,
    /// The set of dynamically adjustable booleans referenced by this policy.
    conditional_booleans: SymbolList<B, ConditionalBooleans<B>>,
    /// The set of sensitivity levels referenced by this policy.
    sensitivities: SymbolList<B, Sensitivities<B>>,
    /// The set of categories referenced by this policy.
    categories: SymbolList<B, Categories<B>>,
    /// The set of access vectors referenced by this policy.
    access_vectors: SimpleArray<B, AccessVectors<B>>,
    conditional_lists: SimpleArray<B, ConditionalNodes<B>>,
    role_transitions: SimpleArray<B, Ref<B, [RoleTransition]>>,
    role_allowlist: SimpleArray<B, Ref<B, [RoleAllow]>>,
    filename_transition_list: FilenameTransitionList<B>,
    initial_sids: SimpleArray<B, InitialSids<B>>,
    filesystems: SimpleArray<B, NamedContextPairs<B>>,
    ports: SimpleArray<B, Ports<B>>,
    network_interfaces: SimpleArray<B, NamedContextPairs<B>>,
    nodes: SimpleArray<B, Nodes<B>>,
    fs_uses: SimpleArray<B, FsUses<B>>,
    ipv6_nodes: SimpleArray<B, IPv6Nodes<B>>,
    infinitiband_partition_keys: Option<SimpleArray<B, InfinitiBandPartitionKeys<B>>>,
    infinitiband_end_ports: Option<SimpleArray<B, InfinitiBandEndPorts<B>>>,
    generic_fs_contexts: SimpleArray<B, GenericFsContexts<B>>,
    range_translations: SimpleArray<B, RangeTranslations<B>>,
    /// Extensible bitmaps that encode associations between types and attributes.
    attribute_maps: Vec<ExtensibleBitmap<B>>,
}

impl<B: ByteSlice + Debug + PartialEq> Policy<B> {
    /// Parses the binary policy stored in `bytes`. It is an error for `bytes` to have trailing
    /// bytes after policy parsing completes.
    pub fn parse(bytes: B) -> Result<Self, anyhow::Error> {
        let (policy, tail) = <Policy<B> as Parse<B>>::parse(bytes)?;
        let num_bytes = tail.len();
        if num_bytes > 0 {
            return Err(ParseError::TrailingBytes { num_bytes }.into());
        }
        Ok(policy)
    }

    /// The policy version stored in the underlying binary policy.
    pub fn policy_version(&self) -> u32 {
        self.policy_version.policy_version()
    }

    /// The way "unknown" policy decisions should be handed according to the underlying binary
    /// policy.
    pub fn handle_unknown(&self) -> &HandleUnknown {
        self.config.handle_unknown()
    }
}

/// A data structure that can be parsed as a part of a binary policy.
pub(crate) trait Parse<B: ByteSlice + Debug + PartialEq> {
    /// The type of error that may be returned from `parse()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Parses a `Self` from `bytes`, returning the `Self` and trailing bytes, or an error if
    /// bytes corresponding to a `Self` are malformed.
    fn parse(bytes: B) -> Result<(Self, B), Self::Error>
    where
        Self: Sized;
}

/// Parse a data structure from a prefix of a [`ByteSlice`].
impl<B: ByteSlice + Debug + PartialEq> Parse<B> for Policy<B> {
    /// A [`Policy`] may add context to underlying [`ParseError`] values.
    type Error = anyhow::Error;

    /// Parses an entire binary policy.
    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (magic, tail) = Ref::<B, Magic>::parse(tail).context("parsing magic")?;

        let (signature, tail) = Signature::parse(tail).context("parsing signature")?;

        let (policy_version, tail) =
            Ref::<B, PolicyVersion>::parse(tail).context("parsing policy version")?;

        let (config, tail) = Config::parse(tail).context("parsing policy config")?;

        let (counts, tail) =
            Ref::<B, Counts>::parse(tail).context("parsing high-level policy object counts")?;

        let (policy_capabilities, tail) =
            ExtensibleBitmap::parse(tail).context("parsing policy capabilities")?;

        let (permissive_map, tail) =
            ExtensibleBitmap::parse(tail).context("parsing permissive map")?;

        let (common_symbols, tail) =
            SymbolList::<B, CommonSymbols<B>>::parse(tail).context("parsing common symbols")?;

        let (classes, tail) =
            SymbolList::<B, Classes<B>>::parse(tail).context("parsing classes")?;

        let (roles, tail) = SymbolList::<B, Roles<B>>::parse(tail).context("parsing roles")?;

        let (types, tail) = SymbolList::<B, Types<B>>::parse(tail).context("parsing types")?;

        let (users, tail) = SymbolList::<B, Users<B>>::parse(tail).context("parsing users")?;

        let (conditional_booleans, tail) = SymbolList::<B, ConditionalBooleans<B>>::parse(tail)
            .context("parsing conditional booleans")?;

        let (sensitivities, tail) =
            SymbolList::<B, Sensitivities<B>>::parse(tail).context("parsing sensitivites")?;

        let (categories, tail) =
            SymbolList::<B, Categories<B>>::parse(tail).context("parsing categories")?;

        let (access_vectors, tail) =
            SimpleArray::<B, AccessVectors<B>>::parse(tail).context("parsing access vectors")?;

        let (conditional_lists, tail) = SimpleArray::<B, ConditionalNodes<B>>::parse(tail)
            .context("parsing conditional lists")?;

        let (role_transitions, tail) = SimpleArray::<B, Ref<B, [RoleTransition]>>::parse(tail)
            .context("parsing role transitions")?;

        let (role_allowlist, tail) = SimpleArray::<B, Ref<B, [RoleAllow]>>::parse(tail)
            .context("parsing role allow rules")?;

        let (filename_transition_list, tail) = if policy_version.policy_version() >= 33 {
            let (filename_transition_list, tail) =
                SimpleArray::<B, FilenameTransitions<B>>::parse(tail)
                    .context("parsing standard filename transitions")?;
            (FilenameTransitionList::PolicyVersionGeq33(filename_transition_list), tail)
        } else {
            let (filename_transition_list, tail) =
                SimpleArray::<B, DeprecatedFilenameTransitions<B>>::parse(tail)
                    .context("parsing deprecated filename transitions")?;
            (FilenameTransitionList::PolicyVersionLeq32(filename_transition_list), tail)
        };

        let (initial_sids, tail) =
            SimpleArray::<B, InitialSids<B>>::parse(tail).context("parsing initial sids")?;

        let (filesystems, tail) = SimpleArray::<B, NamedContextPairs<B>>::parse(tail)
            .context("parsing filesystem contexts")?;

        let (ports, tail) = SimpleArray::<B, Ports<B>>::parse(tail).context("parsing ports")?;

        let (network_interfaces, tail) = SimpleArray::<B, NamedContextPairs<B>>::parse(tail)
            .context("parsing network interfaces")?;

        let (nodes, tail) = SimpleArray::<B, Nodes<B>>::parse(tail).context("parsing nodes")?;

        let (fs_uses, tail) =
            SimpleArray::<B, FsUses<B>>::parse(tail).context("parsing fs uses")?;

        let (ipv6_nodes, tail) =
            SimpleArray::<B, IPv6Nodes<B>>::parse(tail).context("parsing ipv6 nodes")?;

        let (infinitiband_partition_keys, infinitiband_end_ports, tail) =
            if policy_version.policy_version() >= MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY
            {
                let (infinity_band_partition_keys, tail) =
                    SimpleArray::<B, InfinitiBandPartitionKeys<B>>::parse(tail)
                        .context("parsing infiniti band partition keys")?;
                let (infinitiband_end_ports, tail) =
                    SimpleArray::<B, InfinitiBandEndPorts<B>>::parse(tail)
                        .context("parsing infiniti band end ports")?;
                (Some(infinity_band_partition_keys), Some(infinitiband_end_ports), tail)
            } else {
                (None, None, tail)
            };

        let (generic_fs_contexts, tail) = SimpleArray::<B, GenericFsContexts<B>>::parse(tail)
            .context("parsing generic filesystem contexts")?;

        let (range_translations, tail) = SimpleArray::<B, RangeTranslations<B>>::parse(tail)
            .context("parsing range translations")?;

        let primary_names_count = types.metadata.deref().primary_names_count();
        let mut attribute_maps = Vec::with_capacity(primary_names_count as usize);
        let mut tail = tail;

        for i in 0..primary_names_count {
            let (item, next_tail) = ExtensibleBitmap::parse(tail)
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

impl<B: ByteSlice + Debug + PartialEq> Validate for Policy<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between related top-level policy components, such as
    /// `self.infinitiband_partition_keys` and `self.infiniband_end_port`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Parse a data as a slice of inner data structures from a prefix of a [`ByteSlice`].
pub(crate) trait ParseSlice<B: ByteSlice + Debug + PartialEq> {
    /// The type of error that may be returned from `parse()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Parses a `Self` as `count` of internal itemsfrom `bytes`, returning the `Self` and trailing
    /// bytes, or an error if bytes corresponding to a `Self` are malformed.
    fn parse_slice(bytes: B, count: usize) -> Result<(Self, B), Self::Error>
    where
        Self: Sized;
}

/// Validate a parsed data structure.
pub(crate) trait Validate {
    /// The type of error that may be returned from `validate()`, usually [`ParseError`] or
    /// [`anyhow::Error`].
    type Error: Into<anyhow::Error>;

    /// Validates a `Self`, returning a `Self::Error` if `self` is internally inconsistent.
    fn validate(&self) -> Result<(), Self::Error>;
}

impl Validate for u8 {
    type Error = ParseError;

    /// Bare byte has no validation constraints.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [u8] {
    type Error = ParseError;

    /// Bare byte slices have no validation constraints.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice + Debug + PartialEq, T: Validate + FromBytes + NoCell> Validate for Ref<B, T> {
    type Error = <T as Validate>::Error;

    /// A [`Ref`] of `T` that implements [`FromBytes`] delegates to `T::validate()` via
    /// `Ref::deref()`.
    fn validate(&self) -> Result<(), Self::Error> {
        self.deref().validate()
    }
}

impl<B: ByteSlice + Debug + PartialEq, T: Validate + FromBytes + NoCell> Validate for Ref<B, [T]> {
    type Error = <T as Validate>::Error;

    /// A [`Ref`] of `[T]` that implements [`FromBytes`] delegates to `T::validate()` via
    /// `Ref::deref()` for each `T`.
    fn validate(&self) -> Result<(), Self::Error> {
        for item in self.deref().iter() {
            item.validate()?;
        }
        Ok(())
    }
}

/// Treat a type as metadata that contains a count of subsequent data.
pub(crate) trait Counted {
    /// Returns the count of subsequent data items.
    fn count(&self) -> u32;
}

impl<B: ByteSlice + Debug + PartialEq, T: Counted + FromBytes + NoCell> Counted for Ref<B, T> {
    fn count(&self) -> u32 {
        self.deref().count()
    }
}

/// A length-encoded array that contains metadata in `M` and a slice of data items internally
/// managed by `D`.
#[derive(Debug, PartialEq)]
struct Array<
    B: ByteSlice + Debug + PartialEq,
    M: Counted + Debug + Parse<B> + PartialEq + Validate,
    D: Debug + ParseSlice<B> + PartialEq + Validate,
> {
    metadata: M,
    data: D,
    _marker: PhantomData<B>,
}

impl<
        B: ByteSlice + Debug + PartialEq,
        M: Debug + Counted + Parse<B> + PartialEq + Validate,
        D: Debug + ParseSlice<B> + PartialEq + Validate,
    > Parse<B> for Array<B, M, D>
where
    Array<B, M, D>: Validate,
{
    /// [`Array`] abstracts over two types (`M` and `D`) that may have different [`Parse::Error`]
    /// types. Unify error return type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses [`Array`] by parsing *and validating* `metadata`, `data`, and `self`.
    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = M::parse(tail).map_err(Into::<anyhow::Error>::into)?;

        metadata.validate().map_err(Into::<anyhow::Error>::into)?;

        let (data, tail) =
            D::parse_slice(tail, metadata.count() as usize).map_err(Into::<anyhow::Error>::into)?;

        data.validate().map_err(Into::<anyhow::Error>::into)?;

        let array = Self { metadata, data, _marker: PhantomData };
        array.validate().map_err(Into::<anyhow::Error>::into)?;

        Ok((array, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq, T: FromBytes + NoCell + Unaligned + Validate> Parse<B>
    for Ref<B, T>
{
    /// [`Ref`] may return a [`ParseError`] internally, or `<T as Parse>::Error`. Unify error return
    /// type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses [`Ref`] by consuming it as an unaligned prefix, then validating it via `Ref::deref`.
    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) =
            Ref::<B, T>::new_unaligned_from_prefix(bytes).ok_or(ParseError::MissingData {
                type_name: std::any::type_name::<T>(),
                type_size: std::mem::size_of::<T>(),
                num_bytes,
            })?;
        data.deref().validate().map_err(Into::<anyhow::Error>::into)?;

        Ok((data, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq, T: FromBytes + NoCell + Unaligned> ParseSlice<B>
    for Ref<B, [T]>
where
    [T]: Validate,
{
    /// [`Ref`] may return a [`ParseError`] internally, or `<T as Parse>::Error`. Unify error return
    /// type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses [`Ref`] by consuming it as an unaligned prefix as a slice, then validating the slice
    /// via `Ref::deref`.
    fn parse_slice(bytes: B, count: usize) -> Result<(Self, B), Self::Error> {
        let num_bytes = bytes.len();
        let (data, tail) = Ref::<B, [T]>::new_slice_from_prefix(bytes, count).ok_or(
            ParseError::MissingSliceData {
                type_name: std::any::type_name::<T>(),
                type_size: std::mem::size_of::<T>(),
                num_items: count,
                num_bytes,
            },
        )?;
        data.deref().validate().map_err(Into::<anyhow::Error>::into)?;

        Ok((data, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq, T: Parse<B> + Validate> ParseSlice<B> for Vec<T>
where
    Self: Validate,
{
    /// `Vec<T>` may return a [`ParseError`] internally, or `<T as Parse>::Error`. Unify error
    /// return type via [`anyhow::Error`].
    type Error = anyhow::Error;

    /// Parses `Vec<T>` by parsing individual `T` instances, then validating them.
    fn parse_slice(bytes: B, count: usize) -> Result<(Self, B), Self::Error> {
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
    use super::error::ParseError;

    /// Downcasts an [`anyhow::Error`] to a [`ParseError`] for structured error comparison in tests.
    pub fn as_parse_error(error: anyhow::Error) -> ParseError {
        error.downcast::<ParseError>().expect("parse error")
    }
}
