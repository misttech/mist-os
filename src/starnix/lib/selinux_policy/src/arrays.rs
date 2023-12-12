// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Special cases of `Array<Bytes, Metadata, Data>` and instances of `Metadata` and `Data` that
//! appear in binary SELinux policies.

use super::{
    error::ParseError, extensible_bitmap::ExtensibleBitmap, symbols::MlsRange, Array, Counted,
    Parse, ParseSlice, Validate,
};

use anyhow::Context as _;
use std::{fmt::Debug, ops::Deref as _};
use zerocopy::{
    little_endian as le, AsBytes, ByteSlice, FromBytes, FromZeros, NoCell, Ref, Unaligned,
};

pub(crate) const EXTENDED_PERMISSIONS_IS_SPECIFIED_DRIVER_PERMISSIONS_MASK: u16 = 0x0700;
pub(crate) const MIN_POLICY_VERSION_FOR_INFINITIBAND_PARTITION_KEY: u32 = 31;

pub(crate) type SimpleArray<B, T> = Array<B, Ref<B, le::U32>, T>;

impl<B: ByteSlice + Debug + PartialEq, T: Debug + ParseSlice<B> + PartialEq + Validate> Validate
    for SimpleArray<B, T>
{
    type Error = <T as Validate>::Error;

    /// Defers to `self.data` for validation. `self.data` has access to all information, including
    /// size stored in `self.metadata`.
    fn validate(&self) -> Result<(), Self::Error> {
        self.data.validate()
    }
}

impl Counted for le::U32 {
    fn count(&self) -> u32 {
        self.get()
    }
}

impl Validate for le::U32 {
    type Error = ParseError;

    /// Using a raw `Ref<B, le::U32>` implies no additional constraints on its value. To operate
    /// with constraints, define a `struct T<B>(Ref<B, le::U32>);` and
    /// `impl<B: ...> Validate for T<B> { ... }`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ConditionalNodes<B> = Vec<ConditionalNode<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ConditionalNodes<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency between consecutive [`ConditionalNode`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type ConditionalNodeItems<B> =
    Array<B, Ref<B, ConditionalNodeMetadata>, Ref<B, [ConditionalNodeDatum]>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for ConditionalNodeItems<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency between [`ConditionalNodeMetadata`] consecutive
    /// [`ConditionalNodeDatum`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [ConditionalNodeDatum] {
    type Error = ParseError;

    /// TODO: Validate internal consistency between consecutive [`ConditionalNodeData`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct ConditionalNode<B: ByteSlice + Debug + PartialEq> {
    items: ConditionalNodeItems<B>,
    true_list: SimpleArray<B, AccessVectors<B>>,
    false_list: SimpleArray<B, AccessVectors<B>>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for ConditionalNode<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (items, tail) =
            ConditionalNodeItems::parse(tail).context("parsing conditional node items")?;

        let (true_list, tail) = SimpleArray::<B, AccessVectors<B>>::parse(tail)
            .context("parsing conditional node true list")?;

        let (false_list, tail) = SimpleArray::<B, AccessVectors<B>>::parse(tail)
            .context("parsing conditional node false list")?;

        Ok((Self { items, true_list, false_list }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for ConditionalNode<B> {
    type Error = ParseError;

    /// TODO: Validate relationship between fields in [`ConditionalNode`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConditionalNodeMetadata {
    state: le::U32,
    count: le::U32,
}

impl Counted for ConditionalNodeMetadata {
    fn count(&self) -> u32 {
        self.count.get()
    }
}

impl Validate for ConditionalNodeMetadata {
    type Error = ParseError;

    /// TODO: Validate [`ConditionalNodeMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ConditionalNodeDatum {
    node_type: le::U32,
    boolean: le::U32,
}

impl Validate for ConditionalNodeDatum {
    type Error = ParseError;

    /// TODO: Validate [`ConditionalNodeDatum`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type AccessVectors<B> = Vec<AccessVector<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for AccessVectors<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency between consecutive [`AccessVector`] instances.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct AccessVector<B: ByteSlice + Debug + PartialEq> {
    metadata: Ref<B, AccessVectorMetadata>,
    extended_permissions: ExtendedPermissions<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for AccessVector<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (metadata, tail) = Ref::<B, AccessVectorMetadata>::new_unaligned_from_prefix(tail)
            .ok_or(ParseError::MissingData {
                type_name: std::any::type_name::<AccessVectorMetadata>(),
                type_size: std::mem::size_of::<AccessVectorMetadata>(),
                num_bytes,
            })?;

        let (extended_permissions, tail) = if metadata.deref().access_vector_type()
            & EXTENDED_PERMISSIONS_IS_SPECIFIED_DRIVER_PERMISSIONS_MASK
            != 0
        {
            let num_bytes = tail.len();
            let (specified_driver_permissions, tail) =
                Ref::<B, SpecifiedDriverPermissions>::new_unaligned_from_prefix(tail).ok_or(
                    ParseError::MissingData {
                        type_name: std::any::type_name::<SpecifiedDriverPermissions>(),
                        type_size: std::mem::size_of::<SpecifiedDriverPermissions>(),
                        num_bytes,
                    },
                )?;
            (ExtendedPermissions::SpecifiedDriverPermissions(specified_driver_permissions), tail)
        } else {
            let num_bytes = tail.len();
            let (permission_mask, tail) = Ref::<B, le::U32>::new_unaligned_from_prefix(tail)
                .ok_or(ParseError::MissingData {
                    type_name: "ExtendedPermissions::PermissionMask",
                    type_size: std::mem::size_of::<le::U32>(),
                    num_bytes,
                })?;
            (ExtendedPermissions::PermissionMask(permission_mask), tail)
        };

        Ok((Self { metadata, extended_permissions }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for AccessVector<B> {
    type Error = ParseError;

    /// TODO: Verify [`AccessVector`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct AccessVectorMetadata {
    source_type: le::U16,
    target_type: le::U16,
    class: le::U16,
    access_vector_type: le::U16,
}

impl AccessVectorMetadata {
    pub fn access_vector_type(&self) -> u16 {
        self.access_vector_type.get()
    }
}

impl Validate for AccessVectorMetadata {
    type Error = ParseError;

    /// TODO: Validate [`AccessVectorMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum ExtendedPermissions<B: ByteSlice + Debug + PartialEq> {
    SpecifiedDriverPermissions(Ref<B, SpecifiedDriverPermissions>),
    PermissionMask(Ref<B, le::U32>),
}

impl<B: ByteSlice + Debug + PartialEq> Validate for ExtendedPermissions<B> {
    type Error = ParseError;

    /// TODO: Validate [`ExtendedPermissions`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct SpecifiedDriverPermissions {
    specified: u8,
    driver: u8,
    permissions: [le::U32; 8],
}

impl Validate for SpecifiedDriverPermissions {
    type Error = ParseError;

    /// TODO: Validate [`SpecifiedDriverPermissions`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct RoleTransition {
    role: le::U32,
    role_type: le::U32,
    new_role: le::U32,
    tclass: le::U32,
}

impl Validate for RoleTransition {
    type Error = ParseError;

    /// TODO: Validate [`RoleTransition`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [RoleTransition] {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence [`RoleTransition`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct RoleAllow {
    role: le::U32,
    new_role: le::U32,
}

impl Validate for RoleAllow {
    type Error = ParseError;

    /// TODO: Validate [`RoleAllow`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Validate for [RoleAllow] {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence [`RoleAllow`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum FilenameTransitionList<B: ByteSlice + Debug + PartialEq> {
    PolicyVersionGeq33(SimpleArray<B, FilenameTransitions<B>>),
    PolicyVersionLeq32(SimpleArray<B, DeprecatedFilenameTransitions<B>>),
}

pub(crate) type FilenameTransitions<B> = Vec<FilenameTransition<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for FilenameTransitions<B> {
    type Error = ParseError;

    /// TODO: Validate sequence of [`FilenameTransition`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct FilenameTransition<B: ByteSlice + Debug + PartialEq> {
    filename: SimpleArray<B, Ref<B, [u8]>>,
    transition_type: Ref<B, le::U32>,
    transition_class: Ref<B, le::U32>,
    items: SimpleArray<B, FilenameTransitionItems<B>>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for FilenameTransition<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (filename, tail) = SimpleArray::<B, Ref<B, [u8]>>::parse(tail)
            .context("parsing filename for filename transition")?;

        let num_bytes = tail.len();
        let (transition_type, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "FilenameTransition::transition_type",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let num_bytes = tail.len();
        let (transition_class, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "FilenameTransition::transition_class",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (items, tail) = SimpleArray::<B, FilenameTransitionItems<B>>::parse(tail)
            .context("parsing items for filename transition")?;

        Ok((Self { filename, transition_type, transition_class, items }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for FilenameTransition<B> {
    type Error = anyhow::Error;

    /// TODO: Validate [`FilenameTransition`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type FilenameTransitionItems<B> = Vec<FilenameTransitionItem<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for FilenameTransitionItems<B> {
    type Error = ParseError;

    /// TODO: Validate sequence of [`FilenameTransitionItem`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct FilenameTransitionItem<B: ByteSlice + Debug + PartialEq> {
    stypes: ExtensibleBitmap<B>,
    out_type: Ref<B, le::U32>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for FilenameTransitionItem<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (stypes, tail) = ExtensibleBitmap::parse(tail)
            .context("parsing stypes extensible bitmap for file transition")?;

        let num_bytes = tail.len();
        let (out_type, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "FilenameTransitionItem::out_type",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        Ok((Self { stypes, out_type }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for FilenameTransitionItem<B> {
    type Error = ParseError;

    /// TODO: Validate [`FilenameTransitionItem`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type DeprecatedFilenameTransitions<B> = Vec<DeprecatedFilenameTransition<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for DeprecatedFilenameTransitions<B> {
    type Error = ParseError;

    /// TODO: Validate sequence of [`DeprecatedFilenameTransition`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct DeprecatedFilenameTransition<B: ByteSlice + Debug + PartialEq> {
    filename: SimpleArray<B, Ref<B, [u8]>>,
    metadata: Ref<B, DeprecatedFilenameTransitionMetadata>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for DeprecatedFilenameTransition<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (filename, tail) = SimpleArray::<B, Ref<B, [u8]>>::parse(tail)
            .context("parsing filename for deprecated filename transition")?;

        let (metadata, tail) = Ref::<B, DeprecatedFilenameTransitionMetadata>::parse(tail)
            .context("parsing metadata for deprecated filename transition")?;

        Ok((Self { filename, metadata }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for DeprecatedFilenameTransition<B> {
    type Error = ParseError;

    /// TODO: Validate [`DeprecatedFilenameTransition`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct DeprecatedFilenameTransitionMetadata {
    bit: le::U32,
    transition_type: le::U32,
    transition_class: le::U32,
    old_type: le::U32,
}

impl Validate for DeprecatedFilenameTransitionMetadata {
    type Error = ParseError;

    /// TODO: Validate [`DeprecatedFilenameTransitionMetadata`] internals.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type InitialSids<B> = Vec<InitialSid<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for InitialSids<B> {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence of [`InitialSid`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct InitialSid<B: ByteSlice + Debug + PartialEq> {
    sid: Ref<B, le::U32>,
    context: Context<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for InitialSid<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (sid, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "InitialSid::sid",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (context, tail) = Context::parse(tail).context("parsing context for initial sid")?;

        Ok((Self { sid, context }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for InitialSid<B> {
    type Error = ParseError;

    /// TODO: Validate `self.sid` and consistency between `self.sid` and `self.context`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Context<B: ByteSlice + Debug + PartialEq> {
    metadata: Ref<B, ContextMetadata>,
    mls_range: MlsRange<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for Context<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            Ref::<B, ContextMetadata>::parse(tail).context("parsing metadata for context")?;

        let (mls_range, tail) = MlsRange::parse(tail).context("parsing mls range for context")?;

        Ok((Self { metadata, mls_range }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for Context<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between `self.metadata` and `self.mls_range`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct ContextMetadata {
    user: le::U32,
    role: le::U32,
    context_type: le::U32,
}

impl Validate for ContextMetadata {
    type Error = ParseError;

    /// TODO: Validate fields and internal consistency of [`ContextMetadata`] object.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type NamedContextPairs<B> = Vec<NamedContextPair<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for NamedContextPairs<B> {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence of [`NamedContextPairs`] objects.
    ///
    /// TODO: Is different validation required for `filesystems` and `network_interfaces`? If so,
    /// create wrapper types with different [`Validate`] implementations.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct NamedContextPair<B: ByteSlice + Debug + PartialEq> {
    name: SimpleArray<B, Ref<B, [u8]>>,
    context1: Context<B>,
    context2: Context<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for NamedContextPair<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (name, tail) = SimpleArray::parse(tail).context("parsing filesystem context name")?;

        let (context1, tail) =
            Context::parse(tail).context("parsing first context for filesystem context")?;

        let (context2, tail) =
            Context::parse(tail).context("parsing second context for filesystem context")?;

        Ok((Self { name, context1, context2 }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for NamedContextPair<B> {
    type Error = ParseError;

    /// TODO: Validate `self.name` and consistency between all fields.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Ports<B> = Vec<Port<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Ports<B> {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence of [`Ports`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Port<B: ByteSlice + Debug + PartialEq> {
    metadata: Ref<B, PortMetadata>,
    context: Context<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for Port<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (metadata, tail) =
            Ref::<B, PortMetadata>::parse(tail).context("parsing metadata for context")?;

        let (context, tail) = Context::parse(tail).context("parsing context for port")?;

        Ok((Self { metadata, context }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for Port<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between `self.metadata` and `self.context`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct PortMetadata {
    protocol: le::U32,
    low_port: le::U32,
    high_port: le::U32,
}

impl Validate for PortMetadata {
    type Error = ParseError;

    /// TODO: Validate fields and internal consistency of [`PortMetadata`] object.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type Nodes<B> = Vec<Node<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for Nodes<B> {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence of [`Node`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct Node<B: ByteSlice + Debug + PartialEq> {
    address: Ref<B, le::U32>,
    mask: Ref<B, le::U32>,
    context: Context<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for Node<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (address, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "Node::address",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let num_bytes = tail.len();
        let (mask, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "Node::mask",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (context, tail) = Context::parse(tail).context("parsing context for node")?;

        Ok((Self { address, mask, context }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for Node<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between fields of [`Node`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type FsUses<B> = Vec<FsUse<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for FsUses<B> {
    type Error = ParseError;

    /// TODO: Validate sequence of [`FsUse`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct FsUse<B: ByteSlice + Debug + PartialEq> {
    behavior_and_name: Array<B, Ref<B, FsUseMetadata>, Ref<B, [u8]>>,
    context: Context<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for FsUse<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (behavior_and_name, tail) =
            Array::<B, Ref<B, FsUseMetadata>, Ref<B, [u8]>>::parse(tail)
                .context("parsing fs use metadata")?;

        let (context, tail) = Context::parse(tail).context("parsing context for fs use")?;

        Ok((Self { behavior_and_name, context }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for FsUse<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between fields of [`FsUse`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for Array<B, Ref<B, FsUseMetadata>, Ref<B, [u8]>> {
    type Error = ParseError;

    /// TODO: Validate consistency between `behavior` and `name`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct FsUseMetadata {
    behavior: le::U32,
    length: le::U32,
}

impl Counted for FsUseMetadata {
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for FsUseMetadata {
    type Error = ParseError;

    /// TODO: Validate consistency between fields of [`FsUseMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type IPv6Nodes<B> = Vec<IPv6Node<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for IPv6Nodes<B> {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence of [`IPv6Node`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct IPv6Node<B: ByteSlice + Debug + PartialEq> {
    address: Ref<B, [le::U32; 4]>,
    mask: Ref<B, [le::U32; 4]>,
    context: Context<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for IPv6Node<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (address, tail) = Ref::<B, [le::U32; 4]>::new_unaligned_from_prefix(tail).ok_or(
            ParseError::MissingData {
                type_name: "IPv6Node::address",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            },
        )?;

        let num_bytes = tail.len();
        let (mask, tail) = Ref::<B, [le::U32; 4]>::new_unaligned_from_prefix(tail).ok_or(
            ParseError::MissingData {
                type_name: "IPv6Node::mask",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            },
        )?;

        let (context, tail) = Context::parse(tail).context("parsing context for ipv6 node")?;

        Ok((Self { address, mask, context }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for IPv6Node<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between fields of [`IPv6Node`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type InfinitiBandPartitionKeys<B> = Vec<InfinitiBandPartitionKey<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for InfinitiBandPartitionKeys<B> {
    type Error = ParseError;

    /// TODO: Validate consistency of sequence of [`InfinitiBandPartitionKey`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct InfinitiBandPartitionKey<B: ByteSlice + Debug + PartialEq> {
    low: Ref<B, le::U32>,
    high: Ref<B, le::U32>,
    context: Context<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for InfinitiBandPartitionKey<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let num_bytes = tail.len();
        let (low, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "InfinitiBandPartitionKey::low",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let num_bytes = tail.len();
        let (high, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "InfinitiBandPartitionKey::high",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (context, tail) =
            Context::parse(tail).context("parsing context for infiniti band partition key")?;

        Ok((Self { low, high, context }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for InfinitiBandPartitionKey<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between fields of [`InfinitiBandPartitionKey`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type InfinitiBandEndPorts<B> = Vec<InfinitiBandEndPort<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for InfinitiBandEndPorts<B> {
    type Error = ParseError;

    /// TODO: Validate sequence of [`InfinitiBandEndPort`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct InfinitiBandEndPort<B: ByteSlice + Debug + PartialEq> {
    port_and_name: Array<B, Ref<B, InfinitiBandEndPortMetadata>, Ref<B, [u8]>>,
    context: Context<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for InfinitiBandEndPort<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (port_and_name, tail) =
            Array::<B, Ref<B, InfinitiBandEndPortMetadata>, Ref<B, [u8]>>::parse(tail)
                .context("parsing infiniti band end port metadata")?;

        let (context, tail) =
            Context::parse(tail).context("parsing context for infiniti band end port")?;

        Ok((Self { port_and_name, context }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for InfinitiBandEndPort<B> {
    type Error = ParseError;

    /// TODO: Validate consistency between fields of [`InfinitiBandEndPort`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate
    for Array<B, Ref<B, InfinitiBandEndPortMetadata>, Ref<B, [u8]>>
{
    type Error = ParseError;

    /// TODO: Validate consistency between `behavior` and `name`.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct InfinitiBandEndPortMetadata {
    length: le::U32,
    port: le::U32,
}

impl Counted for InfinitiBandEndPortMetadata {
    fn count(&self) -> u32 {
        self.length.get()
    }
}

impl Validate for InfinitiBandEndPortMetadata {
    type Error = ParseError;

    /// TODO: Validate consistency between fields of [`InfinitiBandEndPortMetadata`].
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type GenericFsContexts<B> = Vec<GenericFsContext<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for GenericFsContexts<B> {
    type Error = ParseError;

    /// TODO: Validate sequence of  [`GenericFsContext`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct GenericFsContext<B: ByteSlice + Debug + PartialEq> {
    type_name: SimpleArray<B, Ref<B, [u8]>>,
    contexts: SimpleArray<B, FsContexts<B>>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for GenericFsContext<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (type_name, tail) = SimpleArray::<B, Ref<B, [u8]>>::parse(tail)
            .context("parsing generic filesystem context name")?;

        let (contexts, tail) = SimpleArray::<B, FsContexts<B>>::parse(tail)
            .context("parsing generic filesystem contexts")?;

        Ok((Self { type_name, contexts }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for GenericFsContext<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency of [`GenericFsContext`] fields.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type FsContexts<B> = Vec<FsContext<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for FsContexts<B> {
    type Error = ParseError;

    /// TODO: Validate sequence of [`FsContext`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct FsContext<B: ByteSlice + Debug + PartialEq> {
    name: SimpleArray<B, Ref<B, [u8]>>,
    class: Ref<B, le::U32>,
    context: Context<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for FsContext<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (name, tail) = SimpleArray::<B, Ref<B, [u8]>>::parse(tail)
            .context("parsing filesystem context name")?;

        let num_bytes = tail.len();
        let (class, tail) =
            Ref::<B, le::U32>::new_unaligned_from_prefix(tail).ok_or(ParseError::MissingData {
                type_name: "FsContext::class",
                type_size: std::mem::size_of::<le::U32>(),
                num_bytes,
            })?;

        let (context, tail) =
            Context::parse(tail).context("parsing context for filesystem context")?;

        Ok((Self { name, class, context }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for FsContext<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency of [`FsContext`] fields.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub(crate) type RangeTranslations<B> = Vec<RangeTranslation<B>>;

impl<B: ByteSlice + Debug + PartialEq> Validate for RangeTranslations<B> {
    type Error = ParseError;

    /// TODO: Validate sequence of [`RangeTranslation`] objects.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}
#[derive(Debug, PartialEq)]
pub(crate) struct RangeTranslation<B: ByteSlice + Debug + PartialEq> {
    metadata: Ref<B, RangeTranslationMetadata>,
    mls_range: MlsRange<B>,
}

impl<B: ByteSlice + Debug + PartialEq> Parse<B> for RangeTranslation<B> {
    type Error = anyhow::Error;

    fn parse(bytes: B) -> Result<(Self, B), Self::Error> {
        let tail = bytes;

        let (metadata, tail) = Ref::<B, RangeTranslationMetadata>::parse(tail)
            .context("parsing range translation metadata")?;

        let (mls_range, tail) =
            MlsRange::parse(tail).context("parsing mls range for range translation")?;

        Ok((Self { metadata, mls_range }, tail))
    }
}

impl<B: ByteSlice + Debug + PartialEq> Validate for RangeTranslation<B> {
    type Error = ParseError;

    /// TODO: Validate internal consistency of [`RangeTranslation`] fields.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(AsBytes, Debug, FromZeros, FromBytes, NoCell, PartialEq, Unaligned)]
#[repr(C, packed)]
pub(crate) struct RangeTranslationMetadata {
    source_type: le::U32,
    target_type: le::U32,
    target_class: le::U32,
}

impl Validate for RangeTranslationMetadata {
    type Error = ParseError;

    /// TODO: Validate internal consistency of [`RangeTranslationMetadata`] fields.
    fn validate(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}
