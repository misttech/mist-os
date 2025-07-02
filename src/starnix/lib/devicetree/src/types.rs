// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use zerocopy::{BigEndian, FromBytes, Immutable, KnownLayout, U32, U64};

/// The devictree header.
///
/// See https://devicetree-specification.readthedocs.io/en/v0.3/flattened-format.html#header
#[derive(FromBytes, Debug, KnownLayout, Immutable)]
#[repr(C)]
pub struct Header {
    /// The value 0xd00dfeed.
    pub magic: U32<BigEndian>,

    /// The total size in bytes of the devicetree data structure.
    ///
    /// This includes the header, the memory reservation block, structure block, as well
    /// as any gaps between or after the blocks.
    pub totalsize: U32<BigEndian>,

    /// The offset, in bytes, of the structure block.
    pub off_dt_struct: U32<BigEndian>,

    /// The offset, in bytes, of the strings block.
    pub off_dt_strings: U32<BigEndian>,

    /// The offset, in bytes, of the memory reservation block.
    pub off_mem_rsvmap: U32<BigEndian>,

    /// The version of the devicetree structure.
    pub version: U32<BigEndian>,

    /// The lowest version of the devicetree data structure with which this version is
    /// backwards compatible.
    pub last_comp_version: U32<BigEndian>,

    /// The physical ID of the system's boot CPU.
    pub boot_cpuid_phys: U32<BigEndian>,

    /// The size, in bytes, of the strings block section.
    pub size_dt_strings: U32<BigEndian>,

    /// The size, in bytes, of the structure block section.
    pub size_dt_struct: U32<BigEndian>,
}

#[derive(FromBytes, Debug, KnownLayout, Immutable)]
#[repr(C)]
pub struct ReserveEntry {
    /// The physical address of the reserved memory region.
    pub address: U64<BigEndian>,

    /// The size, in bytes, of the reserved memory region.
    pub size: U64<BigEndian>,
}

#[derive(Debug)]
pub struct Property<'a> {
    /// The name of the property, which is stored as a null-terminated string in the strings
    /// section.
    pub name: &'a str,

    /// The value of the property.
    pub value: &'a [u8],
}

#[derive(Debug)]
pub struct Node<'a> {
    pub name: &'a str,
    pub properties: Vec<Property<'a>>,
    pub children: Vec<Node<'a>>,
}

impl<'a> Node<'a> {
    pub(crate) fn new(name: &'a str) -> Self {
        Node { name, properties: vec![], children: vec![] }
    }
}

#[derive(Debug)]
pub struct Devicetree<'a> {
    pub header: &'a Header,
    pub reserve_entries: &'a [ReserveEntry],
    pub root_node: Node<'a>,
}
