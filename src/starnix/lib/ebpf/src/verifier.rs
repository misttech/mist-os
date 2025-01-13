// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(unused_variables)]

use crate::visitor::{BpfVisitor, ProgramCounter, Register, Source};
use crate::{
    DataWidth, EbpfError, EbpfInstruction, MapSchema, BPF_MAX_INSTS, BPF_STACK_SIZE,
    GENERAL_REGISTER_COUNT, REGISTER_COUNT,
};
use byteorder::{BigEndian, ByteOrder, LittleEndian, NativeEndian};
use fuchsia_sync::Mutex;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use zerocopy::IntoBytes;

/// A trait to receive the log from the verifier.
pub trait VerifierLogger {
    /// Log a line. The line is always a correct encoded ASCII string.
    fn log(&mut self, line: &[u8]);
}

/// A `VerifierLogger` that drops all its content.
pub struct NullVerifierLogger;

impl VerifierLogger for NullVerifierLogger {
    fn log(&mut self, line: &[u8]) {
        debug_assert!(line.is_ascii());
    }
}

/// An identifier for a memory buffer accessible by an ebpf program. The identifiers are built as a
/// chain of unique identifier so that a buffer can contain multiple pointers to the same type and
/// the verifier can distinguish between the different instances.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MemoryId {
    id: u64,
    parent: Option<Box<MemoryId>>,
}

impl From<u64> for MemoryId {
    fn from(id: u64) -> Self {
        Self { id, parent: None }
    }
}

/// A counter that allows to generate new ids for parameters. The namespace is the same as for id
/// generated for types while verifying an ebpf program, but it is started a u64::MAX / 2 and so is
/// guaranteed to never collide because the number of instruction of an ebpf program are bounded.
static BPF_TYPE_IDENTIFIER_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX / 2);

impl MemoryId {
    pub fn new() -> MemoryId {
        Self {
            id: BPF_TYPE_IDENTIFIER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            parent: None,
        }
    }

    /// Build a new id such that `other` is prepended to the chain of parent of `self`.
    fn prepended(&self, other: MemoryId) -> Self {
        match &self.parent {
            None => MemoryId { id: self.id, parent: Some(Box::new(other)) },
            Some(parent) => {
                MemoryId { id: self.id, parent: Some(Box::new(parent.prepended(other))) }
            }
        }
    }

    /// Returns whether the parent of `self` is `parent`
    fn has_parent(&self, parent: &MemoryId) -> bool {
        match &self.parent {
            None => false,
            Some(p) => p.as_ref() == parent,
        }
    }
}

/// The type of a filed in a strcut pointed by `Type::PtrToStruct`.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldType {
    /// Read-only scalar value.
    Scalar { size: usize },

    /// Mutable scalar value.
    MutableScalar { size: usize },

    /// A pointer to the kernel memory. The full buffer is `buffer_size` bytes long.
    PtrToMemory { is_32_bit: bool, id: MemoryId, buffer_size: usize },

    /// A pointer to the kernel memory. The full buffer size is determined by an instance of
    /// `PtrToEndArray` with the same `id`.
    PtrToArray { is_32_bit: bool, id: MemoryId },

    /// A pointer to the kernel memory that represents the first non accessible byte of a
    /// `PtrToArray` with the same `id`.
    PtrToEndArray { is_32_bit: bool, id: MemoryId },
}

/// Definition of a field in a `Type::PtrToStruct`.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldDescriptor {
    /// The offset at which the field is located.
    pub offset: usize,

    /// The type of the pointed memory. Currently the verifier supports only `PtrToArray`,
    /// `PtrToStruct`, `PtrToEndArray` and `ScalarValue`.
    pub field_type: FieldType,
}

impl FieldDescriptor {
    fn size(&self) -> usize {
        match self.field_type {
            FieldType::Scalar { size } | FieldType::MutableScalar { size } => size,
            FieldType::PtrToMemory { is_32_bit, .. }
            | FieldType::PtrToArray { is_32_bit, .. }
            | FieldType::PtrToEndArray { is_32_bit, .. } => {
                if is_32_bit {
                    4
                } else {
                    8
                }
            }
        }
    }
}

/// The offset and width of a field in a struct.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Field {
    offset: i16,
    width: DataWidth,
}

impl Field {
    fn new(offset: i16, width: DataWidth) -> Self {
        Self { offset, width }
    }
}

/// Defines field layout in a struct pointed by `Type::PtrToStruct`.
#[derive(Debug, PartialEq, Default)]
pub struct StructDescriptor {
    /// The list of fields.
    pub fields: Vec<FieldDescriptor>,
}

impl StructDescriptor {
    /// Finds the field type for load/store at the specified location. None is returned if the
    /// access is invalid and the program must be rejected.
    fn find_field(&self, base_offset: i64, field: Field) -> Option<&FieldDescriptor> {
        let offset: usize = (base_offset).checked_add(field.offset as i64)?.try_into().ok()?;
        let field_desc =
            self.fields.iter().find(|f| f.offset <= offset && offset < f.offset + f.size())?;
        let is_valid_load = match field_desc.field_type {
            FieldType::Scalar { size } | FieldType::MutableScalar { size } => {
                // For scalars check that the access is within the bounds of the field.
                offset + field.width.bytes() <= field_desc.offset + size
            }
            FieldType::PtrToMemory { is_32_bit, .. }
            | FieldType::PtrToArray { is_32_bit, .. }
            | FieldType::PtrToEndArray { is_32_bit, .. } => {
                let expected_width = if is_32_bit { DataWidth::U32 } else { DataWidth::U64 };
                // Pointer loads are expected to load the whole field.
                offset == field_desc.offset as usize && field.width == expected_width
            }
        };

        is_valid_load.then(|| field_desc)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MemoryParameterSize {
    /// The memory buffer have the given size.
    Value(u64),
    /// The memory buffer size is given by the parameter in the given index.
    Reference { index: u8 },
}

impl MemoryParameterSize {
    fn size(&self, context: &ComputationContext) -> Result<u64, String> {
        match self {
            Self::Value(size) => Ok(*size),
            Self::Reference { index } => {
                let size_type = context.reg(index + 1)?;
                match size_type {
                    Type::ScalarValue { value, unknown_mask: 0, .. } => Ok(value),
                    _ => Err(format!("cannot know buffer size at pc {}", context.pc)),
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Type {
    /// A number.
    ScalarValue {
        /// The value. Its interpresentation depends on `unknown_mask` and `unwritten_mask`.
        value: u64,
        /// A bit mask of unknown bits. A bit in `value` is valid (and can be used by the verifier)
        /// if the equivalent mask in unknown_mask is 0.
        unknown_mask: u64,
        /// A bit mask of unwritten bits. A bit in `value` is written (and can be sent back to
        /// userspace) if the equivalent mask in unknown_mask is 0. `unknown_mask` must always be a
        /// subset of `unwritten_mask`.
        unwritten_mask: u64,
    },
    /// A pointer to a map object.
    ConstPtrToMap { id: u64, schema: MapSchema },
    /// A pointer into the stack.
    PtrToStack { offset: StackOffset },
    /// A pointer to the kernel memory. The full buffer is `buffer_size` bytes long. The pointer is
    /// situated at `offset` from the start of the buffer.
    PtrToMemory { id: MemoryId, offset: i64, buffer_size: u64 },
    /// A pointer to a struct with the specified `StructDescriptor`.
    PtrToStruct { id: MemoryId, offset: i64, descriptor: Arc<StructDescriptor> },
    /// A pointer to the kernel memory. The full buffer size is determined by an instance of
    /// `PtrToEndArray` with the same `id`. The pointer is situadted at `offset` from the start of
    /// the buffer.
    PtrToArray { id: MemoryId, offset: i64 },
    /// A pointer to the kernel memory that represents the first non accessible byte of a
    /// `PtrToArray` with the same `id`.
    PtrToEndArray { id: MemoryId },
    /// A pointer that might be null and must be validated before being referenced.
    NullOr { id: MemoryId, inner: Box<Type> },
    /// An object that must be passed to a method with an associated `ReleaseParameter` before the
    /// end of the program.
    Releasable { id: MemoryId, inner: Box<Type> },
    /// A function parameter that must be a `ScalarValue` when called.
    ScalarValueParameter,
    /// A function parameter that must be a `ConstPtrToMap` when called.
    ConstPtrToMapParameter,
    /// A function parameter that must be a key of a map.
    MapKeyParameter {
        /// The index in the arguments list that contains a `ConstPtrToMap` for the map this key is
        /// associated with.
        map_ptr_index: u8,
    },
    /// A function parameter that must be a value of a map.
    MapValueParameter {
        /// The index in the arguments list that contains a `ConstPtrToMap` for the map this key is
        /// associated with.
        map_ptr_index: u8,
    },
    /// A function parameter that must be a pointer to memory.
    MemoryParameter {
        /// The index in the arguments list that contains a scalar value containing the size of the
        /// memory.
        size: MemoryParameterSize,
        /// Whether this memory is read by the function.
        input: bool,
        /// Whether this memory is written by the function.
        output: bool,
    },
    /// A function return value that is the same type as a parameter.
    AliasParameter {
        /// The index in the argument list of the parameter that has the type of this return value.
        parameter_index: u8,
    },
    /// A function return value that is either null, or the given type.
    NullOrParameter(Box<Type>),
    /// A function parameter that must be a pointer to memory with the given id.
    StructParameter { id: MemoryId },
    /// A function return value that must be passed to a method with an associated
    /// `ReleaseParameter` before the end of the program.
    ReleasableParameter { id: MemoryId, inner: Box<Type> },
    /// A function parameter that will release the value.
    ReleaseParameter { id: MemoryId },
}

/// Defines a partial ordering on `Type` instances, capturing the notion of how "broad"
/// a type is in terms of the set of potential values it represents.
///
/// The ordering is defined such that `t1 > t2` if a proof that an eBPF program terminates
/// in a state where a register or memory location has type `t1` is also a proof that
/// the program terminates in a state where that location has type `t2`.
///
/// In other words, a "broader" type represents a larger set of possible values, and
/// proving termination with a broader type implies termination with any narrower type.
///
/// Examples:
/// * `Type::ScalarValue { unknown_mask: 0, .. }` (a known scalar value) is less than
///   `Type::ScalarValue { unknown_mask: u64::MAX, .. }` (an unknown scalar value).
impl PartialOrd for Type {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        fn mask_is_larger(m1: u64, m2: u64) -> bool {
            m1 & m2 == m2 && m1 | m2 == m1
        }

        // If the values are equals, return the known result.
        if self == other {
            return Some(Ordering::Equal);
        }

        // If one value is not initialized, the types are ordered.
        if self == &Type::UNINITIALIZED {
            return Some(Ordering::Greater);
        }
        if other == &Type::UNINITIALIZED {
            return Some(Ordering::Less);
        }

        // Otherwise, only scalars are comparables.
        match (self, other) {
            (
                Self::ScalarValue {
                    value: value1,
                    unknown_mask: unknown_mask1,
                    unwritten_mask: unwritten_mask1,
                },
                Self::ScalarValue {
                    value: value2,
                    unknown_mask: unknown_mask2,
                    unwritten_mask: unwritten_mask2,
                },
            ) => {
                if mask_is_larger(*unwritten_mask1, *unwritten_mask2)
                    && mask_is_larger(*unknown_mask1, *unknown_mask2)
                    && value1 & !unknown_mask1 == value2 & !unknown_mask1
                {
                    return Some(Ordering::Greater);
                }
                if mask_is_larger(*unwritten_mask2, *unwritten_mask1)
                    && mask_is_larger(*unknown_mask2, *unknown_mask1)
                    && value1 & !unknown_mask2 == value2 & !unknown_mask2
                {
                    return Some(Ordering::Less);
                }
                None
            }
            _ => None,
        }
    }
}

impl From<u64> for Type {
    fn from(value: u64) -> Self {
        Self::ScalarValue { value, unknown_mask: 0, unwritten_mask: 0 }
    }
}

impl Default for Type {
    /// A new instance of `Type` where no bit has been written yet.
    fn default() -> Self {
        Self::UNINITIALIZED.clone()
    }
}

impl Type {
    /// An uninitialized value.
    pub const UNINITIALIZED: Self =
        Self::ScalarValue { value: 0, unknown_mask: u64::MAX, unwritten_mask: u64::MAX };

    /// A `Type` where the data is usable by userspace, but the value is not known by the verifier.
    pub const UNKNOWN_SCALAR: Self =
        Self::ScalarValue { value: 0, unknown_mask: u64::MAX, unwritten_mask: 0 };
    pub fn unknown_written_scalar_value() -> Self {
        Self::UNKNOWN_SCALAR.clone()
    }

    /// The mask associated with a data of size `width`.
    fn mask(width: DataWidth) -> u64 {
        if width == DataWidth::U64 {
            u64::MAX
        } else {
            (1 << width.bits()) - 1
        }
    }

    /// Return true if `self` is a subtype of `super`.
    pub fn is_subtype(&self, super_type: &Type) -> bool {
        match (self, super_type) {
            // Anything can be used in place of an uninitialized value.
            (_, Self::ScalarValue { unwritten_mask: u64::MAX, .. }) => true,

            // Every type is a subtype of itself.
            (self_type, super_type) if self_type == super_type => true,

            _ => false,
        }
    }

    fn inner(&self, context: &ComputationContext) -> Result<&Type, String> {
        match self {
            Self::Releasable { id, inner } => {
                if context.resources.contains(id) {
                    Ok(&inner)
                } else {
                    Err(format!("Access to released resource at pc {}", context.pc))
                }
            }
            _ => Ok(self),
        }
    }

    /// Constraints `type1` and `type2` for a conditional jump with the specified `jump_type` and
    /// `jump_width`.
    fn constraint(
        context: &mut ComputationContext,
        jump_type: JumpType,
        jump_width: JumpWidth,
        type1: Self,
        type2: Self,
    ) -> Result<(Self, Self), String> {
        let result = match (jump_width, jump_type, type1.inner(context)?, type2.inner(context)?) {
            (
                JumpWidth::W64,
                JumpType::Eq,
                Self::ScalarValue { value: value1, unknown_mask: known1, unwritten_mask: 0 },
                Self::ScalarValue { value: value2, unknown_mask: known2, unwritten_mask: 0 },
            ) => {
                let v = Self::ScalarValue {
                    value: value1 | value2,
                    unknown_mask: known1 & known2,
                    unwritten_mask: 0,
                };
                (v.clone(), v)
            }
            (
                JumpWidth::W32,
                JumpType::Eq,
                Self::ScalarValue { value: value1, unknown_mask: known1, unwritten_mask: 0 },
                Self::ScalarValue { value: value2, unknown_mask: known2, unwritten_mask: 0 },
            ) => {
                let v1 = Self::ScalarValue {
                    value: value1 | (value2 & (u32::MAX as u64)),
                    unknown_mask: known1 & (known2 | ((u32::MAX as u64) << 32)),
                    unwritten_mask: 0,
                };
                let v2 = Self::ScalarValue {
                    value: value2 | (value1 & (u32::MAX as u64)),
                    unknown_mask: known2 & (known1 | ((u32::MAX as u64) << 32)),
                    unwritten_mask: 0,
                };
                (v1, v2)
            }
            (
                JumpWidth::W64,
                JumpType::Eq,
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
                Self::NullOr { id, .. },
            )
            | (
                JumpWidth::W64,
                JumpType::Eq,
                Self::NullOr { id, .. },
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
            ) => {
                context.set_null(id, true);
                let zero = Type::from(0);
                (zero.clone(), zero)
            }
            (
                JumpWidth::W64,
                jump_type,
                Self::NullOr { id, inner },
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
            ) if jump_type.is_strict() => {
                context.set_null(id, false);
                let inner = *inner.clone();
                inner.register_resource(context);
                (inner, type2)
            }
            (
                JumpWidth::W64,
                jump_type,
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
                Self::NullOr { id, inner },
            ) if jump_type.is_strict() => {
                context.set_null(id, false);
                let inner = *inner.clone();
                inner.register_resource(context);
                (type1, inner)
            }

            (
                JumpWidth::W64,
                JumpType::Eq,
                Type::PtrToArray { id: id1, offset },
                Type::PtrToEndArray { id: id2 },
            )
            | (
                JumpWidth::W64,
                JumpType::Le,
                Type::PtrToArray { id: id1, offset },
                Type::PtrToEndArray { id: id2 },
            )
            | (
                JumpWidth::W64,
                JumpType::Ge,
                Type::PtrToEndArray { id: id1 },
                Type::PtrToArray { id: id2, offset },
            ) if id1 == id2 && *offset >= 0 => {
                context.update_array_bounds(id1.clone(), *offset as u64);
                (type1, type2)
            }
            (
                JumpWidth::W64,
                JumpType::Lt,
                Type::PtrToArray { id: id1, offset },
                Type::PtrToEndArray { id: id2 },
            )
            | (
                JumpWidth::W64,
                JumpType::Gt,
                Type::PtrToEndArray { id: id1 },
                Type::PtrToArray { id: id2, offset },
            ) if id1 == id2 && *offset >= -1 => {
                context.update_array_bounds(id1.clone(), (*offset + 1) as u64);
                (type1, type2)
            }
            (JumpWidth::W64, JumpType::Eq, _, _) => (type1.clone(), type1),
            _ => (type1, type2),
        };
        Ok(result)
    }

    fn match_parameter_type(
        &self,
        context: &ComputationContext,
        parameter_type: &Type,
        index: usize,
        next: &mut ComputationContext,
    ) -> Result<(), String> {
        match (parameter_type, self) {
            (Type::ScalarValueParameter, Type::ScalarValue { unwritten_mask: 0, .. })
            | (Type::ConstPtrToMapParameter, Type::ConstPtrToMap { .. }) => Ok(()),
            (
                Type::MapKeyParameter { map_ptr_index },
                Type::PtrToMemory { offset, buffer_size, .. },
            ) => {
                let schema = context.get_map_schema(*map_ptr_index)?;
                context.check_memory_access(*offset, *buffer_size, 0, schema.key_size as usize)
            }
            (Type::MapKeyParameter { map_ptr_index }, Type::PtrToStack { offset }) => {
                let schema = context.get_map_schema(*map_ptr_index)?;
                context.stack.read_data_ptr(context.pc, *offset, schema.key_size as u64)
            }
            (
                Type::MapValueParameter { map_ptr_index },
                Type::PtrToMemory { offset, buffer_size, .. },
            ) => {
                let schema = context.get_map_schema(*map_ptr_index)?;
                context.check_memory_access(*offset, *buffer_size, 0, schema.value_size as usize)
            }
            (Type::MapValueParameter { map_ptr_index }, Type::PtrToStack { offset }) => {
                let schema = context.get_map_schema(*map_ptr_index)?;
                context.stack.read_data_ptr(context.pc, *offset, schema.value_size as u64)
            }
            (Type::MemoryParameter { size, .. }, Type::PtrToMemory { offset, buffer_size, .. }) => {
                let expected_size = size.size(context)?;
                i64::try_from(*buffer_size)
                    .ok()
                    .and_then(|v| v.checked_sub(*offset))
                    .and_then(|v| u64::try_from(v).ok())
                    .and_then(|size_left| (expected_size <= size_left).then_some(()))
                    .ok_or_else(|| format!("out of bound read at pc {}", context.pc))
            }

            (Type::MemoryParameter { size, input, output }, Type::PtrToStack { offset }) => {
                let size = size.size(context)?;
                let buffer_end = offset.checked_add(size).unwrap_or(StackOffset::INVALID);
                if !buffer_end.is_valid() {
                    Err(format!("out of bound access at pc {}", context.pc))
                } else {
                    if *output {
                        next.stack.write_data_ptr(context.pc, *offset, size)?;
                    }
                    if *input {
                        context.stack.read_data_ptr(context.pc, *offset, size)?;
                    }
                    Ok(())
                }
            }
            (
                Type::StructParameter { id: id1 },
                Type::PtrToMemory { id: id2, offset: 0, .. }
                | Type::PtrToStruct { id: id2, offset: 0, .. },
            ) if *id1 == *id2 => Ok(()),
            (
                Type::ReleasableParameter { id: id1, inner: inner1 },
                Type::Releasable { id: id2, inner: inner2 },
            ) if id2.has_parent(id1) => {
                if next.resources.contains(id2) {
                    inner2.match_parameter_type(context, inner1, index, next)
                } else {
                    Err(format!("Resource already released for index {index} at pc {}", context.pc))
                }
            }
            (Type::ReleaseParameter { id: id1 }, Type::Releasable { id: id2, .. })
                if id2.has_parent(id1) =>
            {
                if next.resources.remove(id2) {
                    Ok(())
                } else {
                    Err(format!(
                        "{id2:?} Resource already released for index {index} at pc {}",
                        context.pc
                    ))
                }
            }

            _ => Err(format!("incorrect parameter for index {index} at pc {}", context.pc)),
        }
    }

    /// If this `Type` is an instance of NullOr with the given `null_id`, replace it wither either
    /// 0 or the subtype depending on `is_null`
    fn set_null(&mut self, null_id: &MemoryId, is_null: bool) {
        match self {
            Type::NullOr { id, inner } if id == null_id => {
                if is_null {
                    *self = Type::from(0);
                } else {
                    *self = *inner.clone();
                }
            }
            _ => {}
        }
    }

    fn register_resource(&self, context: &mut ComputationContext) {
        match self {
            Type::Releasable { id, .. } => {
                context.resources.insert(id.clone());
            }
            _ => {}
        }
    }

    /// Partially Compares two iterators of comparable items.
    ///
    /// This function iterates through both input iterators simultaneously and compares the corresponding elements.
    /// The comparison continues until:
    /// 1. Both iterators are exhausted and all elements were considered equal, in which case it returns `Some(Ordering::Equal)`.
    /// 2. All pairs of corresponding elements that are not equal have the same ordering (`Ordering::Less` or `Ordering::Greater`), in which case it returns `Some(Ordering)` reflecting that consistent ordering.
    /// 3. One iterator is exhausted before the other, or any comparison between elements yields `None`, or not all non-equal pairs have the same ordering, in which case it returns `None`.
    fn compare_list<'a>(
        mut l1: impl Iterator<Item = &'a Self>,
        mut l2: impl Iterator<Item = &'a Self>,
    ) -> Option<Ordering> {
        let mut result = Ordering::Equal;
        loop {
            match (l1.next(), l2.next()) {
                (None, None) => return Some(result),
                (_, None) | (None, _) => return None,
                (Some(v1), Some(v2)) => {
                    result = associate_orderings(result, v1.partial_cmp(v2)?)?;
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct FunctionSignature {
    pub args: Vec<Type>,
    pub return_value: Type,
    pub invalidate_array_bounds: bool,
}

#[derive(Debug, Default)]
pub struct CallingContext {
    /// List of map schemas of the associated map. The maps can be accessed using LDDW instruction
    /// with `src_reg=BPF_PSEUDO_MAP_IDX`.
    pub maps: Vec<MapSchema>,
    /// The registered external functions.
    pub helpers: HashMap<u32, FunctionSignature>,
    /// The args of the program.
    pub args: Vec<Type>,
    /// Packet type. Normally it should be either `None` or `args[0]`.
    pub packet_type: Option<Type>,
}

impl CallingContext {
    pub fn register_map(&mut self, schema: MapSchema) -> usize {
        let index = self.maps.len();
        self.maps.push(schema);
        index
    }
    pub fn set_helpers(&mut self, helpers: HashMap<u32, FunctionSignature>) {
        self.helpers = helpers;
    }
    pub fn set_args(&mut self, args: &[Type]) {
        assert!(args.len() <= 5);
        self.args = args.to_vec();
    }
    pub fn set_packet_type(&mut self, packet_type: Type) {
        self.packet_type = Some(packet_type);
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct StructAccess {
    pub(crate) pc: ProgramCounter,

    // Memory Id of the struct being accessed.
    pub(crate) memory_id: MemoryId,

    // Offset of the field being loaded.
    pub(crate) field_offset: usize,

    // Indicates that this is a 32-bit pointer load. These instructions must be remapped.
    pub(crate) is_32_bit_ptr_load: bool,
}

#[derive(Debug)]
pub struct VerifiedEbpfProgram {
    pub(crate) code: Vec<EbpfInstruction>,
    pub(crate) args: Vec<Type>,
    pub(crate) struct_access_instructions: Vec<StructAccess>,
    pub(crate) maps: Vec<MapSchema>,
}

impl VerifiedEbpfProgram {
    // Convert the program to raw code. Can be used only when the program doesn't access any
    // structs and maps.
    pub fn to_code(self) -> Vec<EbpfInstruction> {
        assert!(self.struct_access_instructions.is_empty());
        assert!(self.maps.is_empty());
        self.code
    }

    pub fn from_verified_code(code: Vec<EbpfInstruction>, args: Vec<Type>) -> Self {
        Self { code, struct_access_instructions: vec![], maps: vec![], args }
    }
}

/// Verify the given code depending on the type of the parameters and the registered external
/// functions. Returned `VerifiedEbpfProgram` should be linked in order to execute it.
pub fn verify_program(
    code: Vec<EbpfInstruction>,
    calling_context: CallingContext,
    logger: &mut dyn VerifierLogger,
) -> Result<VerifiedEbpfProgram, EbpfError> {
    if code.len() > BPF_MAX_INSTS {
        return error_and_log(logger, "ebpf program too long");
    }

    let mut context = ComputationContext::default();
    for (i, t) in calling_context.args.iter().enumerate() {
        // The parameter registers are r1 to r5.
        context.set_reg((i + 1) as u8, t.clone()).map_err(EbpfError::ProgramVerifyError)?;
    }
    let states = vec![context];
    let mut verification_context = VerificationContext {
        calling_context,
        logger,
        states,
        code: &code,
        counter: 0,
        iteration: 0,
        terminating_contexts: Default::default(),
        struct_access_instructions: Default::default(),
    };
    while let Some(mut context) = verification_context.states.pop() {
        if let Some(terminating_contexts) =
            verification_context.terminating_contexts.get(&context.pc)
        {
            // Check whether there exist a context that terminate and prove that this context does
            // also terminate.
            if let Some(ending_context) =
                terminating_contexts.iter().find(|c| c.computation_context >= context)
            {
                // One such context has been found, this proves the current context terminates.
                // If the context has a parent, register the data dependencies and try to terminate
                // it.
                if let Some(parent) = context.parent.take() {
                    parent.dependencies.lock().push(ending_context.dependencies.clone());
                    if let Some(parent) = Arc::into_inner(parent) {
                        parent
                            .terminate(&mut verification_context)
                            .map_err(EbpfError::ProgramVerifyError)?;
                    }
                }
                continue;
            }
        }
        if verification_context.iteration > 10 * BPF_MAX_INSTS {
            return error_and_log(verification_context.logger, "bpf byte code does not terminate");
        }
        if context.pc >= verification_context.code.len() {
            return error_and_log(verification_context.logger, "pc out of bounds");
        }
        let visit_result = context.visit(&mut verification_context, &code[context.pc..]);
        match visit_result {
            Err(message) => {
                return error_and_log(verification_context.logger, message);
            }
            _ => {}
        }
        verification_context.iteration += 1;
    }

    let struct_access_instructions =
        verification_context.struct_access_instructions.into_values().collect::<Vec<_>>();
    let CallingContext { maps, args, .. } = verification_context.calling_context;
    Ok(VerifiedEbpfProgram { code, struct_access_instructions, maps, args })
}

struct VerificationContext<'a> {
    /// The type information for the program arguments and the registered functions.
    calling_context: CallingContext,
    /// The logger to use.
    logger: &'a mut dyn VerifierLogger,
    /// The `ComputationContext` yet to be validated.
    states: Vec<ComputationContext>,
    /// The program being analyzed.
    code: &'a [EbpfInstruction],
    /// A counter used to generated unique ids for memory buffers and maps.
    counter: u64,
    /// The current iteration of the verifier. Used to ensure termination by limiting the number of
    /// iteration before bailing out.
    iteration: usize,
    /// Keep track of the context that terminates at a given pc. The list of context will all be
    /// incomparables as each time a bigger context is computed, the smaller ones are removed from
    /// the list.
    terminating_contexts: BTreeMap<ProgramCounter, Vec<TerminatingContext>>,
    /// The current set of struct access instructions that will need to be updated when the
    /// program is linked. This is also used to ensure that a given instruction always loads the
    /// same field. If this is not the case, the verifier will reject the program.
    struct_access_instructions: HashMap<ProgramCounter, StructAccess>,
}

impl<'a> VerificationContext<'a> {
    fn next_id(&mut self) -> u64 {
        let id = self.counter;
        self.counter += 1;
        id
    }

    /// Register an instruction that loads or stores a struct field. These instructions will need
    /// to updated later when the program is linked.
    fn register_struct_access(&mut self, struct_access: StructAccess) -> Result<(), String> {
        match self.struct_access_instructions.entry(struct_access.pc) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(struct_access);
            }
            std::collections::hash_map::Entry::Occupied(entry) => {
                if *entry.get() != struct_access {
                    return Err(format!(
                        "Inconsistent struct field access at pc: {}",
                        struct_access.pc
                    ));
                }
            }
        }
        Ok(())
    }
}

const STACK_ELEMENT_SIZE: usize = std::mem::size_of::<u64>();
const STACK_MAX_INDEX: usize = BPF_STACK_SIZE / STACK_ELEMENT_SIZE;

/// An offset inside the stack. The offset is from the end of the stack.
/// downward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StackOffset(i64);

impl Default for StackOffset {
    fn default() -> Self {
        Self(BPF_STACK_SIZE as i64)
    }
}

impl StackOffset {
    const INVALID: Self = Self(i64::MIN);

    /// Whether the current offset is valid.
    fn is_valid(&self) -> bool {
        self.0 >= 0 && self.0 <= (BPF_STACK_SIZE as i64)
    }

    /// The value of the register.
    fn reg(&self) -> u64 {
        self.0 as u64
    }

    /// The index into the stack array this offset points to. Can be called only if `is_valid()`
    /// is true.
    fn array_index(&self) -> usize {
        usize::try_from(self.0).unwrap() / STACK_ELEMENT_SIZE
    }

    /// The offset inside the aligned u64 in the stack.
    fn sub_index(&self) -> usize {
        usize::try_from(self.0).unwrap() % STACK_ELEMENT_SIZE
    }

    fn checked_add<T: TryInto<i64>>(self, rhs: T) -> Option<Self> {
        self.0.checked_add(rhs.try_into().ok()?).map(Self)
    }
}

/// The state of the stack
#[derive(Clone, Debug, Default, PartialEq)]
struct Stack {
    data: HashMap<usize, Type>,
}

impl Stack {
    /// Replace all instances of the NullOr type with the given `null_id` to either 0 or the
    /// subtype depending on `is_null`
    fn set_null(&mut self, null_id: &MemoryId, is_null: bool) {
        for (_, t) in self.data.iter_mut() {
            t.set_null(null_id, is_null);
        }
    }

    fn get(&self, index: usize) -> &Type {
        self.data.get(&index).unwrap_or(&Type::UNINITIALIZED)
    }

    fn set(&mut self, index: usize, t: Type) {
        if t == Type::UNINITIALIZED {
            self.data.remove(&index);
        } else {
            self.data.insert(index, t);
        }
    }

    fn extract_sub_value(value: u64, offset: usize, byte_count: usize) -> u64 {
        NativeEndian::read_uint(&value.as_bytes()[offset..], byte_count)
    }

    fn insert_sub_value(mut original: u64, value: u64, width: DataWidth, offset: usize) -> u64 {
        let byte_count = width.bytes();
        let original_buf = original.as_mut_bytes();
        let value_buf = value.as_bytes();
        original_buf[offset..(byte_count + offset)].copy_from_slice(&value_buf[..byte_count]);
        original
    }

    fn write_data_ptr(
        &mut self,
        pc: ProgramCounter,
        mut offset: StackOffset,
        bytes: u64,
    ) -> Result<(), String> {
        for i in 0..bytes {
            self.store(pc, offset, Type::unknown_written_scalar_value(), DataWidth::U8)?;
            offset = offset.checked_add(1).unwrap();
        }
        Ok(())
    }

    fn read_data_ptr(
        &self,
        pc: ProgramCounter,
        offset: StackOffset,
        bytes: u64,
    ) -> Result<(), String> {
        let read_element =
            |index: usize, start_offset: usize, end_offset: usize| -> Result<(), String> {
                match self.get(index) {
                    Type::ScalarValue { unwritten_mask, .. } => {
                        debug_assert!(end_offset > start_offset);
                        let unwritten_bits = Self::extract_sub_value(
                            *unwritten_mask,
                            start_offset,
                            end_offset - start_offset,
                        );
                        if unwritten_bits == 0 {
                            Ok(())
                        } else {
                            Err(format!("reading unwritten value from the stack at pc {}", pc))
                        }
                    }
                    _ => Err(format!("invalid read from the stack at pc {}", pc)),
                }
            };
        if bytes == 0 {
            return Ok(());
        }

        if !offset.is_valid() {
            return Err(format!("invalid stack offset at pc {}", pc));
        }

        let end_offset = offset
            .checked_add(bytes)
            .filter(|v| v.is_valid())
            .ok_or_else(|| format!("stack overflow at pc {}", pc))?;

        // Handle the case where all the data is contained in a single element excluding the last
        // byte (the case when the read ends at an element edge, i.e. `end_offset.sub_index()==0`,
        // is covered by the default path below).
        if offset.array_index() == end_offset.array_index() {
            return read_element(offset.array_index(), offset.sub_index(), end_offset.sub_index());
        }

        // Handle the first element, that might be partial.
        read_element(offset.array_index(), offset.sub_index(), STACK_ELEMENT_SIZE)?;

        // Handle the last element, that might be partial.
        if end_offset.sub_index() != 0 {
            read_element(end_offset.array_index(), 0, end_offset.sub_index())?;
        }

        // Handle the any full type between beginning and end.
        for i in (offset.array_index() + 1)..end_offset.array_index() {
            read_element(i, 0, STACK_ELEMENT_SIZE)?;
        }

        Ok(())
    }

    fn store(
        &mut self,
        pc: ProgramCounter,
        offset: StackOffset,
        value: Type,
        width: DataWidth,
    ) -> Result<(), String> {
        if !offset.is_valid() {
            return Err(format!("out of bounds store at pc {}", pc));
        }
        if offset.sub_index() % width.bytes() != 0 {
            return Err(format!("misaligned access at pc {}", pc));
        }

        let index = offset.array_index();
        if width == DataWidth::U64 {
            self.set(index, value);
        } else {
            match value {
                Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                    let (old_value, old_unknown_mask, old_unwritten_mask) = match self.get(index) {
                        Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                            (*value, *unknown_mask, *unwritten_mask)
                        }
                        _ => {
                            // The value in the stack is not a scalar. Let consider it an scalar
                            // value with no written bits.
                            let Type::ScalarValue { value, unknown_mask, unwritten_mask } =
                                Type::UNINITIALIZED
                            else {
                                unreachable!();
                            };
                            (value, unknown_mask, unwritten_mask)
                        }
                    };
                    let sub_index = offset.sub_index();
                    let value = Self::insert_sub_value(old_value, value, width, sub_index);
                    let unknown_mask =
                        Self::insert_sub_value(old_unknown_mask, unknown_mask, width, sub_index);
                    let unwritten_mask = Self::insert_sub_value(
                        old_unwritten_mask,
                        unwritten_mask,
                        width,
                        sub_index,
                    );
                    self.set(index, Type::ScalarValue { value, unknown_mask, unwritten_mask });
                }
                _ => {
                    return Err(format!(
                        "cannot store part of a non scalar value on the stack at pc {}",
                        pc
                    ));
                }
            }
        }
        Ok(())
    }

    fn load(
        &self,
        pc: ProgramCounter,
        offset: StackOffset,
        width: DataWidth,
    ) -> Result<Type, String> {
        if offset.array_index() >= STACK_MAX_INDEX {
            return Err(format!("out of bounds load at pc {}", pc));
        }
        if offset.sub_index() % width.bytes() != 0 {
            return Err(format!("misaligned access at pc {}", pc));
        }

        let index = offset.array_index();
        let loaded_type = self.get(index).clone();
        if width == DataWidth::U64 {
            Ok(loaded_type)
        } else {
            match loaded_type {
                Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                    let sub_index = offset.sub_index();
                    let value = Self::extract_sub_value(value, sub_index, width.bytes());
                    let unknown_mask =
                        Self::extract_sub_value(unknown_mask, sub_index, width.bytes());
                    let unwritten_mask =
                        Self::extract_sub_value(unwritten_mask, sub_index, width.bytes());
                    Ok(Type::ScalarValue { value, unknown_mask, unwritten_mask })
                }
                _ => Err(format!("incorrect load of {} bytes at pc {}", width.bytes(), pc)),
            }
        }
    }
}

/// Two types are ordered with `t1` > `t2` if a proof that a program in a state `t1` finish is also
/// a proof that a program in a state `t2` finish.
impl PartialOrd for Stack {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let mut result = Ordering::Equal;
        let mut data_iter1 = self.data.iter().peekable();
        let mut data_iter2 = other.data.iter().peekable();
        loop {
            let k1 = data_iter1.peek().map(|(k, _)| *k);
            let k2 = data_iter2.peek().map(|(k, _)| *k);
            let k = match (k1, k2) {
                (None, None) => return Some(result),
                (Some(k), None) => {
                    data_iter1.next();
                    *k
                }
                (None, Some(k)) => {
                    data_iter2.next();
                    *k
                }
                (Some(k1), Some(k2)) => {
                    if k1 <= k2 {
                        data_iter1.next();
                    }
                    if k2 <= k1 {
                        data_iter2.next();
                    }
                    *std::cmp::min(k1, k2)
                }
            };
            result = associate_orderings(result, self.get(k).partial_cmp(other.get(k))?)?;
        }
    }
}

macro_rules! bpf_log {
    ($context:ident, $verification_context:ident, $($msg:tt)*) => {
        let prefix = format!("{}: ({:02x})", $context.pc, $verification_context.code[$context.pc].code);
        let suffix = format!($($msg)*);
        $verification_context.logger.log(format!("{prefix} {suffix}").as_bytes());
    }
}

/// The state of the computation as known by the verifier at a given point in time.
#[derive(Debug, Default)]
struct ComputationContext {
    /// The program counter.
    pc: ProgramCounter,
    /// Register 0 to 9.
    registers: [Type; GENERAL_REGISTER_COUNT as usize],
    /// The state of the stack.
    stack: Stack,
    /// The dynamically known bounds of buffers indexed by their ids.
    array_bounds: BTreeMap<MemoryId, u64>,
    /// The currently allocated resources.
    resources: HashSet<MemoryId>,
    /// The previous context in the computation.
    parent: Option<Arc<ComputationContext>>,
    /// The data dependencies of this context. This is used to broaden a known ending context to
    /// help cutting computation branches.
    dependencies: Mutex<Vec<DataDependencies>>,
}

impl Clone for ComputationContext {
    fn clone(&self) -> Self {
        Self {
            pc: self.pc,
            registers: self.registers.clone(),
            stack: self.stack.clone(),
            array_bounds: self.array_bounds.clone(),
            resources: self.resources.clone(),
            parent: self.parent.clone(),
            // dependencies are erased as they must always be used on the same instance of the
            // context.
            dependencies: Default::default(),
        }
    }
}

/// parent and dependencies are ignored for the comparison, as they do not matter for termination.
impl PartialEq for ComputationContext {
    fn eq(&self, other: &Self) -> bool {
        self.pc == other.pc
            && self.registers == other.registers
            && self.stack == other.stack
            && self.array_bounds == other.array_bounds
    }
}

impl ComputationContext {
    /// Replace all instances of the NullOr type with the given `null_id` to either 0 or the
    /// subtype depending on `is_null`
    fn set_null(&mut self, null_id: &MemoryId, is_null: bool) {
        for i in 0..self.registers.len() {
            self.registers[i].set_null(null_id, is_null);
        }
        self.stack.set_null(null_id, is_null);
    }

    fn reg(&self, index: Register) -> Result<Type, String> {
        if index >= REGISTER_COUNT {
            return Err(format!("R{index} is invalid at pc {}", self.pc));
        }
        if index < GENERAL_REGISTER_COUNT {
            Ok(self.registers[index as usize].clone())
        } else {
            Ok(Type::PtrToStack { offset: StackOffset::default() })
        }
    }

    fn set_reg(&mut self, index: Register, reg_type: Type) -> Result<(), String> {
        if index >= GENERAL_REGISTER_COUNT {
            return Err(format!("R{index} is invalid at pc {}", self.pc));
        }
        self.registers[index as usize] = reg_type;
        Ok(())
    }

    fn update_array_bounds(&mut self, id: MemoryId, new_bound: u64) {
        self.array_bounds
            .entry(id)
            .and_modify(|v| *v = std::cmp::max(*v, new_bound))
            .or_insert(new_bound);
    }

    fn get_map_schema(&self, argument: u8) -> Result<MapSchema, String> {
        match self.reg(argument + 1)? {
            Type::ConstPtrToMap { schema, .. } => Ok(schema),
            _ => Err(format!("No map found at argument {argument} at pc {}", self.pc)),
        }
    }

    fn next(&self) -> Result<Self, String> {
        let parent = Some(Arc::new(self.clone()));
        self.jump_with_offset(0, parent)
    }

    /// Returns a new `ComputationContext` where the pc has jump by `offset + 1`. In particular,
    /// the next instruction is reached with `jump_with_offset(0)`.
    fn jump_with_offset(&self, offset: i16, parent: Option<Arc<Self>>) -> Result<Self, String> {
        let pc = self
            .pc
            .checked_add_signed(offset.into())
            .and_then(|v| v.checked_add_signed(1))
            .ok_or_else(|| format!("jump outside of program at pc {}", self.pc))?;
        let result = Self {
            pc,
            registers: self.registers.clone(),
            stack: self.stack.clone(),
            array_bounds: self.array_bounds.clone(),
            resources: self.resources.clone(),
            parent,
            dependencies: Default::default(),
        };
        Ok(result)
    }

    fn check_memory_access(
        &self,
        dst_offset: i64,
        dst_buffer_size: u64,
        instruction_offset: i16,
        width: usize,
    ) -> Result<(), String> {
        let final_offset = dst_offset
            .checked_add(instruction_offset as i64)
            .ok_or_else(|| format!("out of bound access at pc {}", self.pc))?;
        let end_offset = final_offset
            .checked_add(width as i64)
            .ok_or_else(|| format!("out of bound access at pc {}", self.pc))?;
        if final_offset < 0 || end_offset as u64 > dst_buffer_size {
            return Err(format!("out of bound access at pc {}", self.pc));
        }
        Ok(())
    }

    fn store_memory(
        &mut self,
        context: &mut VerificationContext<'_>,
        addr: &Type,
        field: Field,
        value: Type,
    ) -> Result<(), String> {
        let addr = addr.inner(self)?;
        match *addr {
            Type::PtrToStack { offset } => {
                let offset_sum = offset.checked_add(field.offset).unwrap_or(StackOffset::INVALID);
                return self.stack.store(self.pc, offset_sum, value, field.width);
            }
            Type::PtrToMemory { offset, buffer_size, .. } => {
                self.check_memory_access(offset, buffer_size, field.offset, field.width.bytes())?;
            }
            Type::PtrToStruct { ref id, offset, ref descriptor, .. } => {
                let field_desc = descriptor
                    .find_field(offset, field)
                    .ok_or_else(|| format!("incorrect store at pc {}", self.pc))?;

                if !matches!(field_desc.field_type, FieldType::MutableScalar { .. }) {
                    return Err(format!("store to a read-only field at pc {}", self.pc));
                }

                context.register_struct_access(StructAccess {
                    pc: self.pc,
                    memory_id: id.clone(),
                    field_offset: field_desc.offset,
                    is_32_bit_ptr_load: false,
                })?;
            }
            Type::PtrToArray { ref id, offset } => {
                self.check_memory_access(
                    offset,
                    *self.array_bounds.get(&id).unwrap_or(&0),
                    field.offset,
                    field.width.bytes(),
                )?;
            }
            _ => return Err(format!("incorrect store at pc {}", self.pc)),
        }

        match value {
            Type::ScalarValue { unwritten_mask: 0, .. } => {}
            // Private data should not be leaked.
            _ => return Err(format!("incorrect store at pc {}", self.pc)),
        }
        Ok(())
    }

    fn load_memory(
        &self,
        context: &mut VerificationContext<'_>,
        addr: &Type,
        field: Field,
    ) -> Result<Type, String> {
        let addr = addr.inner(self)?;
        match *addr {
            Type::PtrToStack { offset } => {
                let offset_sum = offset.checked_add(field.offset).unwrap_or(StackOffset::INVALID);
                self.stack.load(self.pc, offset_sum, field.width)
            }
            Type::PtrToMemory { ref id, offset, buffer_size, .. } => {
                self.check_memory_access(offset, buffer_size, field.offset, field.width.bytes())?;
                Ok(Type::unknown_written_scalar_value())
            }
            Type::PtrToStruct { ref id, offset, ref descriptor, .. } => {
                let field_desc = descriptor
                    .find_field(offset, field)
                    .ok_or_else(|| format!("incorrect load at pc {}", self.pc))?;

                let (return_type, is_32_bit_ptr_load) = match &field_desc.field_type {
                    FieldType::Scalar { .. } | FieldType::MutableScalar { .. } => {
                        (Type::unknown_written_scalar_value(), false)
                    }
                    FieldType::PtrToArray { id: array_id, is_32_bit } => (
                        Type::PtrToArray { id: array_id.prepended(id.clone()), offset: 0 },
                        *is_32_bit,
                    ),
                    FieldType::PtrToEndArray { id: array_id, is_32_bit } => {
                        (Type::PtrToEndArray { id: array_id.prepended(id.clone()) }, *is_32_bit)
                    }
                    FieldType::PtrToMemory { id: memory_id, buffer_size, is_32_bit } => (
                        Type::PtrToMemory {
                            id: memory_id.prepended(id.clone()),
                            offset: 0,
                            buffer_size: *buffer_size as u64,
                        },
                        *is_32_bit,
                    ),
                };

                context.register_struct_access(StructAccess {
                    pc: self.pc,
                    memory_id: id.clone(),
                    field_offset: field_desc.offset,
                    is_32_bit_ptr_load,
                })?;

                Ok(return_type)
            }
            Type::PtrToArray { ref id, offset } => {
                self.check_memory_access(
                    offset,
                    *self.array_bounds.get(&id).unwrap_or(&0),
                    field.offset,
                    field.width.bytes(),
                )?;
                Ok(Type::unknown_written_scalar_value())
            }
            _ => Err(format!("incorrect load at pc {}", self.pc)),
        }
    }

    /**
     * Given the given `return_value` in a method signature, return the concrete type to use,
     * updating the `next` context is needed.
     *
     * `maybe_null` is true is the computed value will be a descendant of a `NullOr` type.
     */
    fn resolve_return_value(
        &self,
        verification_context: &mut VerificationContext<'_>,
        return_value: &Type,
        next: &mut ComputationContext,
        maybe_null: bool,
    ) -> Result<Type, String> {
        match return_value {
            Type::AliasParameter { parameter_index } => self.reg(parameter_index + 1),
            Type::ReleasableParameter { id, inner } => {
                let id = MemoryId::from(verification_context.next_id()).prepended(id.clone());
                if !maybe_null {
                    next.resources.insert(id.clone());
                }
                Ok(Type::Releasable {
                    id,
                    inner: Box::new(self.resolve_return_value(
                        verification_context,
                        inner,
                        next,
                        maybe_null,
                    )?),
                })
            }
            Type::NullOrParameter(t) => {
                let id = verification_context.next_id();
                Ok(Type::NullOr {
                    id: id.into(),
                    inner: Box::new(self.resolve_return_value(
                        verification_context,
                        t,
                        next,
                        true,
                    )?),
                })
            }
            Type::MapValueParameter { map_ptr_index } => {
                let schema = self.get_map_schema(*map_ptr_index)?;
                let id = verification_context.next_id();
                Ok(Type::PtrToMemory {
                    id: id.into(),
                    offset: 0,
                    buffer_size: schema.value_size as u64,
                })
            }
            Type::MemoryParameter { size, .. } => {
                let buffer_size = size.size(self)?;
                let id = verification_context.next_id();
                Ok(Type::PtrToMemory { id: id.into(), offset: 0, buffer_size })
            }
            t => Ok(t.clone()),
        }
    }

    fn compute_source(&self, src: Source) -> Result<Type, String> {
        match src {
            Source::Reg(reg) => self.reg(reg),
            Source::Value(v) => Ok(v.into()),
        }
    }

    fn apply_computation(
        context: &ComputationContext,
        op1: Type,
        op2: Type,
        alu_type: AluType,
        op: impl Fn(u64, u64) -> u64,
    ) -> Result<Type, String> {
        let result: Type = match (alu_type, op1.inner(context)?, op2.inner(context)?) {
            (
                _,
                Type::ScalarValue { value: value1, unknown_mask: 0, .. },
                Type::ScalarValue { value: value2, unknown_mask: 0, .. },
            ) => op(*value1, *value2).into(),
            (
                AluType::Bitwise,
                Type::ScalarValue {
                    value: value1,
                    unknown_mask: unknown_mask1,
                    unwritten_mask: unwritten_mask1,
                },
                Type::ScalarValue {
                    value: value2,
                    unknown_mask: unknown_mask2,
                    unwritten_mask: unwritten_mask2,
                },
            ) => {
                let unknown_mask = unknown_mask1 | unknown_mask2;
                let unwritten_mask = unwritten_mask1 | unwritten_mask2;
                let value = op(*value1, *value2) & !unknown_mask;
                Type::ScalarValue { value, unknown_mask, unwritten_mask }
            }
            (
                AluType::Shift,
                Type::ScalarValue {
                    value: value1,
                    unknown_mask: unknown_mask1,
                    unwritten_mask: unwritten_mask1,
                },
                Type::ScalarValue { value: value2, unknown_mask: 0, .. },
            ) => {
                let value = op(*value1, *value2);
                let unknown_mask = op(*unknown_mask1, *value2);
                let unwritten_mask = op(*unwritten_mask1, *value2);
                Type::ScalarValue { value, unknown_mask, unwritten_mask }
            }
            (
                AluType::Arsh,
                Type::ScalarValue {
                    value: value1,
                    unknown_mask: unknown_mask1,
                    unwritten_mask: unwritten_mask1,
                },
                Type::ScalarValue { value: value2, unknown_mask: 0, .. },
            ) => {
                let unknown_mask = unknown_mask1.overflowing_shr(*value2 as u32).0;
                let unwritten_mask = unwritten_mask1.overflowing_shr(*value2 as u32).0;
                let value = op(*value1, *value2) & !unknown_mask;
                Type::ScalarValue { value, unknown_mask, unwritten_mask }
            }
            (
                alu_type,
                Type::PtrToStack { offset: x },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) if alu_type.is_ptr_compatible() => {
                Type::PtrToStack { offset: run_on_stack_offset(*x, |x| op(x, *y)) }
            }
            (
                alu_type,
                Type::PtrToMemory { id, offset: x, buffer_size },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) if alu_type.is_ptr_compatible() => {
                let offset = op(*x as u64, *y);
                Type::PtrToMemory {
                    id: id.clone(),
                    offset: offset as i64,
                    buffer_size: *buffer_size,
                }
            }
            (
                alu_type,
                Type::PtrToStruct { id, offset: x, descriptor },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) if alu_type.is_ptr_compatible() => {
                let offset = op(*x as u64, *y);
                Type::PtrToStruct {
                    id: id.clone(),
                    offset: offset as i64,
                    descriptor: descriptor.clone(),
                }
            }
            (
                alu_type,
                Type::PtrToArray { id, offset: x },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) if alu_type.is_ptr_compatible() => {
                let offset = op(*x as u64, *y);
                Type::PtrToArray { id: id.clone(), offset: offset as i64 }
            }
            (
                AluType::Sub,
                Type::PtrToMemory { id: id1, offset: x1, .. },
                Type::PtrToMemory { id: id2, offset: x2, .. },
            )
            | (
                AluType::Sub,
                Type::PtrToStruct { id: id1, offset: x1, .. },
                Type::PtrToStruct { id: id2, offset: x2, .. },
            )
            | (
                AluType::Sub,
                Type::PtrToArray { id: id1, offset: x1 },
                Type::PtrToArray { id: id2, offset: x2 },
            ) if id1 == id2 => Type::from(op(*x1 as u64, *x2 as u64)),
            (AluType::Sub, Type::PtrToStack { offset: x1 }, Type::PtrToStack { offset: x2 }) => {
                Type::from(op(x1.reg(), x2.reg()))
            }
            (
                AluType::Sub,
                Type::PtrToArray { id: id1, .. },
                Type::PtrToEndArray { id: id2, .. },
            )
            | (
                AluType::Sub,
                Type::PtrToEndArray { id: id1, .. },
                Type::PtrToArray { id: id2, .. },
            ) if id1 == id2 => Type::unknown_written_scalar_value(),
            (
                _,
                Type::ScalarValue { unwritten_mask: 0, .. },
                Type::ScalarValue { unwritten_mask: 0, .. },
            ) => Type::unknown_written_scalar_value(),
            _ => Type::default(),
        };
        Ok(result)
    }

    fn alu(
        &mut self,
        op_name: Option<&str>,
        verification_context: &mut VerificationContext<'_>,
        dst: Register,
        src: Source,
        alu_type: AluType,
        op: impl Fn(u64, u64) -> u64,
    ) -> Result<(), String> {
        if let Some(op_name) = op_name {
            bpf_log!(
                self,
                verification_context,
                "{op_name} {}, {}",
                display_register(dst),
                display_source(src)
            );
        }
        let op1 = self.reg(dst)?;
        let op2 = self.compute_source(src)?;
        let result = Self::apply_computation(self, op1, op2, alu_type, op)?;
        let mut next = self.next()?;
        next.set_reg(dst, result)?;
        verification_context.states.push(next);
        Ok(())
    }

    fn log_atomic_operation(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) {
        bpf_log!(
            self,
            verification_context,
            "lock {}{} [{}{}], {}",
            if fetch { "fetch " } else { "" },
            op_name,
            display_register(dst),
            print_offset(offset),
            display_register(src),
        );
    }

    fn raw_atomic_operation(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        width: DataWidth,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
        op: impl FnOnce(&ComputationContext, Type, Type) -> Result<Type, String>,
    ) -> Result<(), String> {
        self.log_atomic_operation(op_name, verification_context, fetch, dst, offset, src);
        let addr = self.reg(dst)?;
        let value = self.reg(src)?;
        let field = Field::new(offset, width);
        let loaded_type = self.load_memory(verification_context, &addr, field)?;
        let result = op(self, loaded_type.clone(), value)?;
        let mut next = self.next()?;
        next.store_memory(verification_context, &addr, field, result)?;
        if fetch {
            next.set_reg(src, loaded_type)?;
        }
        verification_context.states.push(next);
        Ok(())
    }

    fn atomic_operation(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        width: DataWidth,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
        alu_type: AluType,
        op: impl Fn(u64, u64) -> u64,
    ) -> Result<(), String> {
        self.raw_atomic_operation(
            op_name,
            verification_context,
            width,
            fetch,
            dst,
            offset,
            src,
            |context: &ComputationContext, v1: Type, v2: Type| {
                Self::apply_computation(context, v1, v2, alu_type, op)
            },
        )
    }

    fn raw_atomic_cmpxchg(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        dst: Register,
        offset: i16,
        src: Register,
        jump_width: JumpWidth,
        op: impl Fn(u64, u64) -> bool,
    ) -> Result<(), String> {
        self.log_atomic_operation(op_name, verification_context, true, dst, offset, src);
        let width = match jump_width {
            JumpWidth::W32 => DataWidth::U32,
            JumpWidth::W64 => DataWidth::U64,
        };
        let addr = self.reg(dst)?;
        let field = Field::new(offset, width);
        let dst = self.load_memory(verification_context, &addr, field)?;
        let value = self.reg(src)?;
        let r0 = self.reg(0)?;
        let branch = self.compute_branch(jump_width, &dst, &r0, op)?;
        // r0 = dst
        if branch.unwrap_or(true) {
            let mut next = self.next()?;
            let (dst, r0) =
                Type::constraint(&mut next, JumpType::Eq, jump_width, dst.clone(), r0.clone())?;
            next.set_reg(0, dst)?;
            next.store_memory(verification_context, &addr, field, value)?;
            verification_context.states.push(next);
        }
        // r0 != dst
        if !branch.unwrap_or(false) {
            let mut next = self.next()?;
            let (dst, r0) = Type::constraint(&mut next, JumpType::Ne, jump_width, dst, r0)?;
            next.set_reg(0, dst.clone())?;
            next.store_memory(verification_context, &addr, field, dst)?;
            verification_context.states.push(next);
        }

        Ok(())
    }
    fn endianness<BO: ByteOrder>(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        bpf_log!(self, verification_context, "{op_name}{} {}", width.bits(), display_register(dst),);
        let bit_op = |value: u64| match width {
            DataWidth::U16 => BO::read_u16((value as u16).as_bytes()) as u64,
            DataWidth::U32 => BO::read_u32((value as u32).as_bytes()) as u64,
            DataWidth::U64 => BO::read_u64(value.as_bytes()),
            _ => {
                panic!("Unexpected bit width for endianness operation");
            }
        };
        let value = self.reg(dst)?;
        let new_value = match value {
            Type::ScalarValue { value, unknown_mask, unwritten_mask } => Type::ScalarValue {
                value: bit_op(value),
                unknown_mask: bit_op(unknown_mask),
                unwritten_mask: bit_op(unwritten_mask),
            },
            _ => Type::default(),
        };
        let mut next = self.next()?;
        next.set_reg(dst, new_value)?;
        verification_context.states.push(next);
        Ok(())
    }

    fn compute_branch(
        &self,
        jump_width: JumpWidth,
        op1: &Type,
        op2: &Type,
        op: impl Fn(u64, u64) -> bool,
    ) -> Result<Option<bool>, String> {
        match (jump_width, op1, op2) {
            (
                _,
                Type::ScalarValue { value: x, unknown_mask: 0, .. },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) => Ok(Some(op(*x, *y))),

            (
                _,
                Type::ScalarValue { unwritten_mask: 0, .. },
                Type::ScalarValue { unwritten_mask: 0, .. },
            )
            | (
                JumpWidth::W64,
                Type::ScalarValue { value: 0, unknown_mask: 0, .. },
                Type::NullOr { .. },
            )
            | (
                JumpWidth::W64,
                Type::NullOr { .. },
                Type::ScalarValue { value: 0, unknown_mask: 0, .. },
            ) => Ok(None),

            (JumpWidth::W64, Type::PtrToStack { offset: x }, Type::PtrToStack { offset: y }) => {
                Ok(Some(op(x.reg(), y.reg())))
            }

            (
                JumpWidth::W64,
                Type::PtrToMemory { id: id1, offset: x, .. },
                Type::PtrToMemory { id: id2, offset: y, .. },
            )
            | (
                JumpWidth::W64,
                Type::PtrToStruct { id: id1, offset: x, .. },
                Type::PtrToStruct { id: id2, offset: y, .. },
            )
            | (
                JumpWidth::W64,
                Type::PtrToArray { id: id1, offset: x, .. },
                Type::PtrToArray { id: id2, offset: y, .. },
            ) if *id1 == *id2 => Ok(Some(op(*x as u64, *y as u64))),

            (JumpWidth::W64, Type::PtrToArray { id: id1, .. }, Type::PtrToEndArray { id: id2 })
            | (JumpWidth::W64, Type::PtrToEndArray { id: id1 }, Type::PtrToArray { id: id2, .. })
                if *id1 == *id2 =>
            {
                Ok(None)
            }

            _ => Err(format!("non permitted comparison at pc {}", self.pc)),
        }
    }

    fn conditional_jump(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        dst: Register,
        src: Source,
        offset: i16,
        jump_type: JumpType,
        jump_width: JumpWidth,
        op: impl Fn(u64, u64) -> bool,
    ) -> Result<(), String> {
        bpf_log!(
            self,
            verification_context,
            "{op_name} {}, {}, {}",
            display_register(dst),
            display_source(src),
            if offset == 0 { format!("0") } else { print_offset(offset) },
        );
        let op1 = self.reg(dst)?;
        let op2 = self.compute_source(src.clone())?;
        let apply_constraints_and_register = |mut next: Self,
                                              jump_type: JumpType|
         -> Result<Self, String> {
            if jump_type != JumpType::Unknown {
                let (new_op1, new_op2) =
                    Type::constraint(&mut next, jump_type, jump_width, op1.clone(), op2.clone())?;
                if dst < REGISTER_COUNT {
                    next.set_reg(dst, new_op1)?;
                }
                match src {
                    Source::Reg(r) => {
                        next.set_reg(r, new_op2)?;
                    }
                    _ => {
                        // Nothing to do
                    }
                }
            }
            Ok(next)
        };
        let branch = self.compute_branch(jump_width, &op1, &op2, op)?;
        let parent = Some(Arc::new(self.clone()));
        if branch.unwrap_or(true) {
            // Do the jump
            verification_context.states.push(apply_constraints_and_register(
                self.jump_with_offset(offset, parent.clone())?,
                jump_type,
            )?);
        }
        if !branch.unwrap_or(false) {
            // Skip the jump
            verification_context.states.push(apply_constraints_and_register(
                self.jump_with_offset(0, parent)?,
                jump_type.invert(),
            )?);
        }
        Ok(())
    }

    /// Handles the termination of a `ComputationContext`, performing branch cutting optimization.
    ///
    /// This method is called when it has been proven that the current context terminates (e.g.,
    /// reaches an `exit` instruction).
    ///
    /// The following steps are performed:
    /// 1. **Dependency Calculation:** The data dependencies of the context are computed based on
    ///    the dependencies of its terminated children and the instruction at the current PC.
    /// 2. **Context Broadening:** The context's state is broadened by clearing registers and stack
    ///    slots that are *not* in the calculated data dependencies. This optimization assumes that
    ///    data not used by the terminated branch is irrelevant for future execution paths.
    /// 3. **Termination Registration:** The broadened context is added to the set of terminating
    ///    contexts if it is not less than any existing terminating context at the same PC.
    /// 4. **Parent Termination:** If all the children of the current context have terminated,
    ///    its parent context is recursively terminated.
    fn terminate(self, verification_context: &mut VerificationContext<'_>) -> Result<(), String> {
        let mut next = Some(self);
        // Because of the potential length of the parent chain, do not use recursion.
        while let Some(mut current) = next.take() {
            // Take the parent to process it at the end and not keep it in the terminating
            // contexts.
            let parent = current.parent.take();

            // 1. Compute the dependencies of the context using the dependencies of its children
            //    and the actual operation.
            let mut dependencies = DataDependencies::default();
            for dependency in current.dependencies.get_mut().iter() {
                dependencies.merge(dependency);
            }

            dependencies.visit(
                &mut DataDependenciesVisitorContext {
                    calling_context: &verification_context.calling_context,
                    computation_context: &current,
                },
                &verification_context.code[current.pc..],
            )?;

            // 2. Clear the state depending on the dependencies states
            for register in 0..GENERAL_REGISTER_COUNT {
                if !dependencies.registers.contains(&register) {
                    current.set_reg(register, Default::default())?;
                }
            }
            current.stack.data.retain(|k, _| dependencies.stack.contains(k));

            // 3. Add the cleared state to the set of `terminating_contexts`
            let terminating_contexts =
                verification_context.terminating_contexts.entry(current.pc).or_default();
            let mut is_dominated = false;
            terminating_contexts.retain(|c| match c.computation_context.partial_cmp(&current) {
                Some(Ordering::Less) => false,
                Some(Ordering::Equal) | Some(Ordering::Greater) => {
                    // If the list contains a context greater or equal to the current one, it
                    // should not be added.
                    is_dominated = true;
                    true
                }
                _ => true,
            });
            if !is_dominated {
                terminating_contexts.push(TerminatingContext {
                    computation_context: current,
                    dependencies: dependencies.clone(),
                });
            }

            // 4. Register the computed dependencies in our parent, and terminate it if all
            //    dependencies has been computed.
            if let Some(parent) = parent {
                parent.dependencies.lock().push(dependencies);
                // To check whether all dependencies have been computed, rely on the fact that the Arc
                // count of the parent keep track of how many dependencies are left.
                next = Arc::into_inner(parent);
            }
        }
        Ok(())
    }
}

impl Drop for ComputationContext {
    fn drop(&mut self) {
        let mut next = self.parent.take().and_then(Arc::into_inner);
        // Because of the potential length of the parent chain, do not use recursion.
        while let Some(mut current) = next {
            next = current.parent.take().and_then(Arc::into_inner);
        }
    }
}

/// Two types are ordered with `t1` > `t2` if a proof that a program in a state `t1` finish is also
/// a proof that a program in a state `t2` finish.
impl PartialOrd for ComputationContext {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.pc != other.pc || self.resources != other.resources {
            return None;
        }
        let mut result = self.stack.partial_cmp(&other.stack)?;
        result = associate_orderings(
            result,
            Type::compare_list(self.registers.iter(), other.registers.iter())?,
        )?;
        let mut array_bound_iter1 = self.array_bounds.iter().peekable();
        let mut array_bound_iter2 = other.array_bounds.iter().peekable();
        let result = loop {
            match (array_bound_iter1.peek().cloned(), array_bound_iter2.peek().cloned()) {
                (None, None) => break result,
                (None, _) => break associate_orderings(result, Ordering::Greater)?,
                (_, None) => break associate_orderings(result, Ordering::Less)?,
                (Some((k1, v1)), Some((k2, v2))) => match k1.cmp(k2) {
                    Ordering::Equal => {
                        array_bound_iter1.next();
                        array_bound_iter2.next();
                        result = associate_orderings(result, v2.cmp(v1))?;
                    }
                    v @ Ordering::Less => {
                        array_bound_iter1.next();
                        result = associate_orderings(result, v)?;
                    }
                    v @ Ordering::Greater => {
                        array_bound_iter2.next();
                        result = associate_orderings(result, v)?;
                    }
                },
            }
        };
        Some(result)
    }
}

/// Represents the read data dependencies of an eBPF program branch.
///
/// This struct tracks which registers and stack positions are *read* by the
/// instructions within a branch of the eBPF program.  This information is used
/// during branch cutting optimization to broaden terminating contexts.
///
/// The verifier assumes that data not read by a terminated branch is irrelevant
/// for future execution paths and can be safely cleared.
#[derive(Clone, Debug, Default)]
struct DataDependencies {
    /// The set of registers read by the children of a context.
    registers: HashSet<Register>,
    /// The stack positions read by the children of a context.
    stack: HashSet<usize>,
}

impl DataDependencies {
    fn merge(&mut self, other: &DataDependencies) {
        self.registers.extend(other.registers.iter());
        self.stack.extend(other.stack.iter());
    }

    fn alu(&mut self, dst: Register, src: Source) -> Result<(), String> {
        // Only do something if the dst is read, otherwise the computation doesn't matter.
        if self.registers.contains(&dst) {
            if let Source::Reg(src) = src {
                self.registers.insert(src);
            }
        }
        Ok(())
    }

    fn jmp(&mut self, dst: Register, src: Source) -> Result<(), String> {
        self.registers.insert(dst);
        if let Source::Reg(src) = src {
            self.registers.insert(src);
        }
        Ok(())
    }

    fn atomic(
        &mut self,
        context: &ComputationContext,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
        width: DataWidth,
        is_cmpxchg: bool,
    ) -> Result<(), String> {
        let mut is_read = false;
        if is_cmpxchg && self.registers.contains(&0) {
            is_read = true;
        }
        if fetch && self.registers.contains(&src) {
            is_read = true;
        }
        let addr = context.reg(dst)?;
        if let Type::PtrToStack { offset: stack_offset } = addr {
            let stack_offset = stack_offset.checked_add(offset).unwrap_or(StackOffset::INVALID);
            if !stack_offset.is_valid() {
                return Err(format!("Invalid stack offset at {}", context.pc));
            }
            if is_read || self.stack.contains(&stack_offset.array_index()) {
                is_read = true;
                self.stack.insert(stack_offset.array_index());
            }
        }
        if is_read {
            self.registers.insert(0);
            self.registers.insert(src);
        }
        self.registers.insert(dst);
        Ok(())
    }
}

struct DataDependenciesVisitorContext<'a> {
    calling_context: &'a CallingContext,
    computation_context: &'a ComputationContext,
}

impl BpfVisitor for DataDependencies {
    type Context<'a> = DataDependenciesVisitorContext<'a>;

    fn add<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn add64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn and<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn and64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn arsh<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn arsh64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn div<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn div64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn lsh<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn lsh64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn r#mod<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn mod64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn mul<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn mul64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn or<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn or64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn rsh<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn rsh64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn sub<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn sub64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn xor<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }
    fn xor64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src)
    }

    fn mov<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        if src == Source::Reg(dst) || !self.registers.contains(&dst) {
            return Ok(());
        }
        if let Source::Reg(src) = src {
            self.registers.insert(src);
        }
        self.registers.remove(&dst);
        Ok(())
    }
    fn mov64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.mov(context, dst, src)
    }

    fn neg<'a>(&mut self, _context: &mut Self::Context<'a>, _dst: Register) -> Result<(), String> {
        // This is reading and writing the same value. This induces no change in dependencies.
        Ok(())
    }
    fn neg64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        _dst: Register,
    ) -> Result<(), String> {
        // This is reading and writing the same value. This induces no change in dependencies.
        Ok(())
    }

    fn be<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        _dst: Register,
        _width: DataWidth,
    ) -> Result<(), String> {
        // This is reading and writing the same value. This induces no change in dependencies.
        Ok(())
    }
    fn le<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        _dst: Register,
        _width: DataWidth,
    ) -> Result<(), String> {
        // This is reading and writing the same value. This induces no change in dependencies.
        Ok(())
    }

    fn call_external<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        index: u32,
    ) -> Result<(), String> {
        let Some(signature) = context.calling_context.helpers.get(&index).cloned() else {
            return Err(format!("unknown external function {}", index));
        };
        // 0 is overwritten and 1 to 5 are scratch registers
        for register in 0..signature.args.len() + 1 {
            self.registers.remove(&(register as Register));
        }
        // 1 to k are parameters.
        for register in 0..signature.args.len() {
            self.registers.insert((register + 1) as Register);
        }
        Ok(())
    }

    fn exit<'a>(&mut self, _context: &mut Self::Context<'a>) -> Result<(), String> {
        // This read r0 unconditionally.
        self.registers.insert(0);
        Ok(())
    }

    fn jump<'a>(&mut self, _context: &mut Self::Context<'a>, _offset: i16) -> Result<(), String> {
        // This doesn't do anything with values.
        Ok(())
    }

    fn jeq<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jeq64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jne<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jne64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jge<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jge64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jgt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jgt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jle<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jle64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jlt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jlt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsge<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsge64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsgt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsgt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsle<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jsle64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jslt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jslt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jset<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }
    fn jset64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.jmp(dst, src)
    }

    fn atomic_add<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_add64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_and<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_and64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_or<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_or64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_xor<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_xor64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_xchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U32, false)
    }

    fn atomic_xchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, fetch, dst, offset, src, DataWidth::U64, false)
    }

    fn atomic_cmpxchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, true, dst, offset, src, DataWidth::U32, true)
    }

    fn atomic_cmpxchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic(&context.computation_context, true, dst, offset, src, DataWidth::U64, true)
    }

    fn load<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        let context = &context.computation_context;
        if self.registers.contains(&dst) {
            let addr = context.reg(src)?;
            if let Type::PtrToStack { offset: stack_offset } = addr {
                let stack_offset = stack_offset.checked_add(offset).unwrap_or(StackOffset::INVALID);
                if !stack_offset.is_valid() {
                    return Err(format!("Invalid stack offset at {}", context.pc));
                }
                self.stack.insert(stack_offset.array_index());
            }
        }
        self.registers.insert(src);
        Ok(())
    }

    fn load64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        _value: u64,
        _jump_offset: i16,
    ) -> Result<(), String> {
        self.registers.remove(&dst);
        Ok(())
    }

    fn load_map_ptr<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        _map_index: u32,
        _jump_offset: i16,
    ) -> Result<(), String> {
        self.registers.remove(&dst);
        Ok(())
    }

    fn load_from_packet<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Register,
        _offset: i32,
        register_offset: Option<Register>,
        _width: DataWidth,
    ) -> Result<(), String> {
        // 1 to 5 are scratch registers
        for register in 1..6 {
            self.registers.remove(&(register as Register));
        }
        // Only do something if the dst is read, otherwise the computation doesn't matter.
        if self.registers.remove(&dst) {
            self.registers.insert(src);
            if let Some(reg) = register_offset {
                self.registers.insert(reg);
            }
        }
        Ok(())
    }

    fn store<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Source,
        width: DataWidth,
    ) -> Result<(), String> {
        let context = &context.computation_context;
        let addr = context.reg(dst)?;
        if let Type::PtrToStack { offset: stack_offset } = addr {
            let stack_offset = stack_offset.checked_add(offset).unwrap_or(StackOffset::INVALID);
            if !stack_offset.is_valid() {
                return Err(format!("Invalid stack offset at {}", context.pc));
            }
            if self.stack.remove(&stack_offset.array_index()) {
                if let Source::Reg(src) = src {
                    self.registers.insert(src);
                }
            }
        } else {
            if let Source::Reg(src) = src {
                self.registers.insert(src);
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
struct TerminatingContext {
    computation_context: ComputationContext,
    dependencies: DataDependencies,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AluType {
    Plain,
    Bitwise,
    Shift,
    Arsh,
    Sub,
    Add,
}

impl AluType {
    /// Can this operation be done one a pointer and a scalar.
    fn is_ptr_compatible(&self) -> bool {
        match self {
            Self::Sub | Self::Add => true,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JumpWidth {
    W32,
    W64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JumpType {
    Eq,
    Ge,
    Gt,
    Le,
    LooseComparaison,
    Lt,
    Ne,
    StrictComparaison,
    Unknown,
}

impl JumpType {
    fn invert(&self) -> Self {
        match self {
            Self::Eq => Self::Ne,
            Self::Ge => Self::Lt,
            Self::Gt => Self::Le,
            Self::Le => Self::Gt,
            Self::LooseComparaison => Self::StrictComparaison,
            Self::Lt => Self::Ge,
            Self::Ne => Self::Eq,
            Self::StrictComparaison => Self::LooseComparaison,
            Self::Unknown => Self::Unknown,
        }
    }

    fn is_strict(&self) -> bool {
        match self {
            Self::Gt | Self::Lt | Self::Ne | Self::StrictComparaison => true,
            _ => false,
        }
    }
}

fn display_register(register: Register) -> String {
    format!("%r{register}")
}

fn display_source(src: Source) -> String {
    match src {
        Source::Reg(r) => display_register(r),
        Source::Value(v) => format!("0x{v:x}"),
    }
}

impl BpfVisitor for ComputationContext {
    type Context<'a> = VerificationContext<'a>;

    fn add<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("add32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| x.overflowing_add(y).0)
        })
    }
    fn add64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("add"), context, dst, src, AluType::Add, |x, y| x.overflowing_add(y).0)
    }
    fn and<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("and32"), context, dst, src, AluType::Bitwise, |x, y| {
            alu32(x, y, |x, y| x & y)
        })
    }
    fn and64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("and"), context, dst, src, AluType::Bitwise, |x, y| x & y)
    }
    fn arsh<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("arsh32"), context, dst, src, AluType::Arsh, |x, y| {
            alu32(x, y, |x, y| {
                let x = x as i32;
                x.overflowing_shr(y).0 as u32
            })
        })
    }
    fn arsh64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("arsh"), context, dst, src, AluType::Arsh, |x, y| {
            let x = x as i64;
            x.overflowing_shr(y as u32).0 as u64
        })
    }
    fn div<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("div32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| if y == 0 { 0 } else { x / y })
        })
    }
    fn div64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(
            Some("div"),
            context,
            dst,
            src,
            AluType::Plain,
            |x, y| if y == 0 { 0 } else { x / y },
        )
    }
    fn lsh<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("lsh32"), context, dst, src, AluType::Shift, |x, y| {
            alu32(x, y, |x, y| x.overflowing_shl(y).0)
        })
    }
    fn lsh64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("lsh"), context, dst, src, AluType::Shift, |x, y| {
            x.overflowing_shl(y as u32).0
        })
    }
    fn r#mod<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("mod32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| if y == 0 { x } else { x % y })
        })
    }
    fn mod64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(
            Some("mod"),
            context,
            dst,
            src,
            AluType::Plain,
            |x, y| if y == 0 { x } else { x % y },
        )
    }
    fn mov<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        bpf_log!(self, context, "mov32 {}, {}", display_register(dst), display_source(src));
        let src = self.compute_source(src)?;
        let value = match src {
            Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                let value = (value as u32) as u64;
                let unknown_mask = (unknown_mask as u32) as u64;
                let unwritten_mask = (unwritten_mask as u32) as u64;
                Type::ScalarValue { value, unknown_mask, unwritten_mask }
            }
            _ => Type::default(),
        };
        let mut next = self.next()?;
        next.set_reg(dst, value)?;
        context.states.push(next);
        Ok(())
    }
    fn mov64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        bpf_log!(self, context, "mov {}, {}", display_register(dst), display_source(src));
        let src = self.compute_source(src)?;
        let mut next = self.next()?;
        next.set_reg(dst, src)?;
        context.states.push(next);
        Ok(())
    }
    fn mul<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("mul32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| x.overflowing_mul(y).0)
        })
    }
    fn mul64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("mul"), context, dst, src, AluType::Plain, |x, y| x.overflowing_mul(y).0)
    }
    fn or<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("or32"), context, dst, src, AluType::Bitwise, |x, y| {
            alu32(x, y, |x, y| x | y)
        })
    }
    fn or64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("or"), context, dst, src, AluType::Bitwise, |x, y| x | y)
    }
    fn rsh<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("rsh32"), context, dst, src, AluType::Shift, |x, y| {
            alu32(x, y, |x, y| x.overflowing_shr(y).0)
        })
    }
    fn rsh64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("rsh"), context, dst, src, AluType::Shift, |x, y| {
            x.overflowing_shr(y as u32).0
        })
    }
    fn sub<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("sub32"), context, dst, src, AluType::Plain, |x, y| {
            alu32(x, y, |x, y| x.overflowing_sub(y).0)
        })
    }
    fn sub64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("sub"), context, dst, src, AluType::Sub, |x, y| x.overflowing_sub(y).0)
    }
    fn xor<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("xor32"), context, dst, src, AluType::Bitwise, |x, y| {
            alu32(x, y, |x, y| x ^ y)
        })
    }
    fn xor64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(Some("xor"), context, dst, src, AluType::Bitwise, |x, y| x ^ y)
    }

    fn neg<'a>(&mut self, context: &mut Self::Context<'a>, dst: Register) -> Result<(), String> {
        bpf_log!(self, context, "neg32 {}", display_register(dst));
        self.alu(None, context, dst, Source::Value(0), AluType::Plain, |x, y| {
            alu32(x, y, |x, _y| {
                let x = x as i32;
                let x = -x;
                x as u32
            })
        })
    }
    fn neg64<'a>(&mut self, context: &mut Self::Context<'a>, dst: Register) -> Result<(), String> {
        bpf_log!(self, context, "neg {}", display_register(dst));
        self.alu(None, context, dst, Source::Value(0), AluType::Plain, |x, _y| {
            let x = x as i64;
            let x = -x;
            x as u64
        })
    }

    fn be<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        self.endianness::<BigEndian>("be", context, dst, width)
    }

    fn le<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        self.endianness::<LittleEndian>("le", context, dst, width)
    }

    fn call_external<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        index: u32,
    ) -> Result<(), String> {
        bpf_log!(self, context, "call 0x{:x}", index);
        let Some(signature) = context.calling_context.helpers.get(&index).cloned() else {
            return Err(format!("unknown external function {} at pc {}", index, self.pc));
        };
        debug_assert!(signature.args.len() <= 5);
        let mut next = self.next()?;
        for (index, arg) in signature.args.iter().enumerate() {
            let reg = (index + 1) as u8;
            self.reg(reg)?.match_parameter_type(self, arg, index, &mut next)?;
        }
        // Parameters have been validated, specify the return value on return.
        if signature.invalidate_array_bounds {
            next.array_bounds.clear();
        }
        let value =
            self.resolve_return_value(context, &signature.return_value, &mut next, false)?;
        next.set_reg(0, value)?;
        for i in 1..=5 {
            next.set_reg(i, Type::default())?;
        }
        context.states.push(next);
        Ok(())
    }

    fn exit<'a>(&mut self, context: &mut Self::Context<'a>) -> Result<(), String> {
        bpf_log!(self, context, "exit");
        if !matches!(self.reg(0)?, Type::ScalarValue { unwritten_mask: 0, .. }) {
            return Err(format!("register 0 is incorrect at exit time at pc {}", self.pc));
        }
        if !self.resources.is_empty() {
            return Err(format!(
                "some resources have not been released at exit time at pc {}",
                self.pc
            ));
        }
        let this = self.clone();
        this.terminate(context)?;
        // Nothing to do, the program terminated with a valid scalar value.
        Ok(())
    }

    fn jump<'a>(&mut self, context: &mut Self::Context<'a>, offset: i16) -> Result<(), String> {
        bpf_log!(self, context, "ja {}", offset);
        let parent = Some(Arc::new(self.clone()));
        context.states.push(self.jump_with_offset(offset, parent)?);
        Ok(())
    }

    fn jeq<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jeq32",
            context,
            dst,
            src,
            offset,
            JumpType::Eq,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x == y),
        )
    }
    fn jeq64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jeq",
            context,
            dst,
            src,
            offset,
            JumpType::Eq,
            JumpWidth::W64,
            |x, y| x == y,
        )
    }
    fn jne<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jne32",
            context,
            dst,
            src,
            offset,
            JumpType::Ne,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x != y),
        )
    }
    fn jne64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jne",
            context,
            dst,
            src,
            offset,
            JumpType::Ne,
            JumpWidth::W64,
            |x, y| x != y,
        )
    }
    fn jge<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jge32",
            context,
            dst,
            src,
            offset,
            JumpType::Ge,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x >= y),
        )
    }
    fn jge64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jge",
            context,
            dst,
            src,
            offset,
            JumpType::Ge,
            JumpWidth::W64,
            |x, y| x >= y,
        )
    }
    fn jgt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jgt32",
            context,
            dst,
            src,
            offset,
            JumpType::Gt,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x > y),
        )
    }
    fn jgt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jgt",
            context,
            dst,
            src,
            offset,
            JumpType::Gt,
            JumpWidth::W64,
            |x, y| x > y,
        )
    }
    fn jle<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jle32",
            context,
            dst,
            src,
            offset,
            JumpType::Le,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x <= y),
        )
    }
    fn jle64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jle",
            context,
            dst,
            src,
            offset,
            JumpType::Le,
            JumpWidth::W64,
            |x, y| x <= y,
        )
    }
    fn jlt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jlt32",
            context,
            dst,
            src,
            offset,
            JumpType::Lt,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x < y),
        )
    }
    fn jlt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jlt",
            context,
            dst,
            src,
            offset,
            JumpType::Lt,
            JumpWidth::W64,
            |x, y| x < y,
        )
    }
    fn jsge<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsge32",
            context,
            dst,
            src,
            offset,
            JumpType::LooseComparaison,
            JumpWidth::W32,
            |x, y| scomp32(x, y, |x, y| x >= y),
        )
    }
    fn jsge64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsge",
            context,
            dst,
            src,
            offset,
            JumpType::LooseComparaison,
            JumpWidth::W64,
            |x, y| scomp64(x, y, |x, y| x >= y),
        )
    }
    fn jsgt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsgt32",
            context,
            dst,
            src,
            offset,
            JumpType::StrictComparaison,
            JumpWidth::W32,
            |x, y| scomp32(x, y, |x, y| x > y),
        )
    }
    fn jsgt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsgt",
            context,
            dst,
            src,
            offset,
            JumpType::StrictComparaison,
            JumpWidth::W64,
            |x, y| scomp64(x, y, |x, y| x > y),
        )
    }
    fn jsle<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsle32",
            context,
            dst,
            src,
            offset,
            JumpType::LooseComparaison,
            JumpWidth::W32,
            |x, y| scomp32(x, y, |x, y| x <= y),
        )
    }
    fn jsle64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jsle",
            context,
            dst,
            src,
            offset,
            JumpType::LooseComparaison,
            JumpWidth::W64,
            |x, y| scomp64(x, y, |x, y| x <= y),
        )
    }
    fn jslt<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jslt32",
            context,
            dst,
            src,
            offset,
            JumpType::StrictComparaison,
            JumpWidth::W32,
            |x, y| scomp32(x, y, |x, y| x < y),
        )
    }
    fn jslt64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jslt",
            context,
            dst,
            src,
            offset,
            JumpType::StrictComparaison,
            JumpWidth::W64,
            |x, y| scomp64(x, y, |x, y| x < y),
        )
    }
    fn jset<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jset32",
            context,
            dst,
            src,
            offset,
            JumpType::Unknown,
            JumpWidth::W32,
            |x, y| comp32(x, y, |x, y| x & y != 0),
        )
    }
    fn jset64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(
            "jset",
            context,
            dst,
            src,
            offset,
            JumpType::Unknown,
            JumpWidth::W64,
            |x, y| x & y != 0,
        )
    }

    fn atomic_add<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "add32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Add,
            |x, y| alu32(x, y, |x, y| x + y),
        )
    }

    fn atomic_add64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "add",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            AluType::Add,
            |x, y| x + y,
        )
    }

    fn atomic_and<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "and32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| alu32(x, y, |x, y| x & y),
        )
    }

    fn atomic_and64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "and",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| x & y,
        )
    }

    fn atomic_or<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "or32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| alu32(x, y, |x, y| x | y),
        )
    }

    fn atomic_or64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "or",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| x | y,
        )
    }

    fn atomic_xor<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "xor32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| alu32(x, y, |x, y| x ^ y),
        )
    }

    fn atomic_xor64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "xor",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            AluType::Bitwise,
            |x, y| x ^ y,
        )
    }

    fn atomic_xchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.atomic_operation(
            "xchg32",
            context,
            DataWidth::U32,
            fetch,
            dst,
            offset,
            src,
            AluType::Plain,
            |_, x| x,
        )
    }

    fn atomic_xchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        fetch: bool,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.raw_atomic_operation(
            "xchg",
            context,
            DataWidth::U64,
            fetch,
            dst,
            offset,
            src,
            |_, _, x| Ok(x),
        )
    }

    fn atomic_cmpxchg<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.raw_atomic_cmpxchg("cmpxchg32", context, dst, offset, src, JumpWidth::W32, |x, y| {
            comp32(x, y, |x, y| x == y)
        })
    }

    fn atomic_cmpxchg64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
    ) -> Result<(), String> {
        self.raw_atomic_cmpxchg("cmpxchg", context, dst, offset, src, JumpWidth::W64, |x, y| x == y)
    }

    fn load<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        bpf_log!(
            self,
            context,
            "ldx{} {}, [{}{}]",
            width.str(),
            display_register(dst),
            display_register(src),
            print_offset(offset)
        );
        let addr = self.reg(src)?;
        let loaded_type = self.load_memory(context, &addr, Field::new(offset, width))?;
        let mut next = self.next()?;
        next.set_reg(dst, loaded_type)?;
        context.states.push(next);
        Ok(())
    }

    fn load64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        value: u64,
        jump_offset: i16,
    ) -> Result<(), String> {
        bpf_log!(self, context, "lddw {}, 0x{:x}", display_register(dst), value);
        let value = Type::from(value);
        let parent = Some(Arc::new(self.clone()));
        let mut next = self.jump_with_offset(jump_offset, parent)?;
        next.set_reg(dst, value.into())?;
        context.states.push(next);
        Ok(())
    }

    fn load_map_ptr<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        map_index: u32,
        jump_offset: i16,
    ) -> Result<(), String> {
        bpf_log!(self, context, "lddw {}, map_by_index({:x})", display_register(dst), map_index);
        let value = context
            .calling_context
            .maps
            .get(usize::try_from(map_index).unwrap())
            .map(|schema| Type::ConstPtrToMap { id: map_index.into(), schema: *schema })
            .ok_or_else(|| format!("lddw with invalid map index: {}", map_index))?;
        let parent = Some(Arc::new(self.clone()));
        let mut next = self.jump_with_offset(jump_offset, parent)?;
        next.set_reg(dst, value.into())?;
        context.states.push(next);
        Ok(())
    }

    fn load_from_packet<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Register,
        offset: i32,
        register_offset: Option<Register>,
        width: DataWidth,
    ) -> Result<(), String> {
        bpf_log!(
            self,
            context,
            "ldp{} {}{}",
            width.str(),
            register_offset.map(display_register).unwrap_or_else(Default::default),
            print_offset(offset)
        );

        // Verify that `src` refers to a packet.
        let src_type = self.reg(src)?;
        let src_is_packet = match &context.calling_context.packet_type {
            Some(packet_type) => src_type == *packet_type,
            None => false,
        };
        if !src_is_packet {
            return Err(format!("R{} is not a packet at pc {}", src, self.pc));
        }

        if let Some(reg) = register_offset {
            let reg = self.reg(reg)?;
            if !matches!(reg, Type::ScalarValue { unwritten_mask: 0, .. }) {
                return Err(format!("access to unwritten offset at pc {}", self.pc));
            }
        }
        // Handle the case where the load succeed.
        let mut next = self.next()?;
        next.set_reg(dst, Type::unknown_written_scalar_value())?;
        for i in 1..=5 {
            next.set_reg(i, Type::default())?;
        }
        context.states.push(next);
        // Handle the case where the load fails.
        if !matches!(self.reg(0)?, Type::ScalarValue { unwritten_mask: 0, .. }) {
            return Err(format!("register 0 is incorrect at exit time at pc {}", self.pc));
        }
        if !self.resources.is_empty() {
            return Err(format!(
                "some resources have not been released at exit time at pc {}",
                self.pc
            ));
        }
        let this = self.clone();
        this.terminate(context)?;
        // Nothing to do, the program terminated with a valid scalar value.
        Ok(())
    }

    fn store<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Source,
        width: DataWidth,
    ) -> Result<(), String> {
        let value = match src {
            Source::Reg(r) => {
                bpf_log!(
                    self,
                    context,
                    "stx{} [{}{}], {}",
                    width.str(),
                    display_register(dst),
                    print_offset(offset),
                    display_register(r),
                );
                self.reg(r)?
            }
            Source::Value(v) => {
                bpf_log!(
                    self,
                    context,
                    "st{} [{}{}], 0x{:x}",
                    width.str(),
                    display_register(dst),
                    print_offset(offset),
                    v,
                );
                Type::from(v & Type::mask(width))
            }
        };
        let mut next = self.next()?;
        let addr = self.reg(dst)?;
        next.store_memory(context, &addr, Field::new(offset, width), value)?;
        context.states.push(next);
        Ok(())
    }
}

fn alu32(x: u64, y: u64, op: impl FnOnce(u32, u32) -> u32) -> u64 {
    op(x as u32, y as u32) as u64
}

fn comp32(x: u64, y: u64, op: impl FnOnce(u32, u32) -> bool) -> bool {
    op(x as u32, y as u32)
}

fn scomp64(x: u64, y: u64, op: impl FnOnce(i64, i64) -> bool) -> bool {
    op(x as i64, y as i64)
}

fn scomp32(x: u64, y: u64, op: impl FnOnce(i32, i32) -> bool) -> bool {
    op(x as i32, y as i32)
}

fn print_offset<T: Into<i32>>(offset: T) -> String {
    let offset: i32 = offset.into();
    if offset == 0 {
        String::new()
    } else if offset > 0 {
        format!("+{offset}")
    } else {
        format!("{offset}")
    }
}

fn run_on_stack_offset<F>(v: StackOffset, f: F) -> StackOffset
where
    F: FnOnce(u64) -> u64,
{
    StackOffset(f(v.reg()) as i64)
}

fn error_and_log<T>(
    logger: &mut dyn VerifierLogger,
    msg: impl std::string::ToString,
) -> Result<T, EbpfError> {
    let msg = msg.to_string();
    logger.log(msg.as_bytes());
    return Err(EbpfError::ProgramVerifyError(msg));
}

fn associate_orderings(o1: Ordering, o2: Ordering) -> Option<Ordering> {
    match (o1, o2) {
        (o1, o2) if o1 == o2 => Some(o1),
        (o, Ordering::Equal) | (Ordering::Equal, o) => Some(o),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_ordering() {
        let t0 = Type::from(0);
        let t1 = Type::from(1);
        let random = Type::AliasParameter { parameter_index: 8 };
        let unknown_written = Type::unknown_written_scalar_value();
        let unwritten = Type::default();

        assert_eq!(t0.partial_cmp(&t0), Some(Ordering::Equal));
        assert_eq!(t0.partial_cmp(&t1), None);
        assert_eq!(t0.partial_cmp(&random), None);
        assert_eq!(t0.partial_cmp(&unknown_written), Some(Ordering::Less));
        assert_eq!(t0.partial_cmp(&unwritten), Some(Ordering::Less));

        assert_eq!(t1.partial_cmp(&t0), None);
        assert_eq!(t1.partial_cmp(&t1), Some(Ordering::Equal));
        assert_eq!(t1.partial_cmp(&random), None);
        assert_eq!(t1.partial_cmp(&unknown_written), Some(Ordering::Less));
        assert_eq!(t1.partial_cmp(&unwritten), Some(Ordering::Less));

        assert_eq!(random.partial_cmp(&t0), None);
        assert_eq!(random.partial_cmp(&t1), None);
        assert_eq!(random.partial_cmp(&random), Some(Ordering::Equal));
        assert_eq!(random.partial_cmp(&unknown_written), None);
        assert_eq!(random.partial_cmp(&unwritten), Some(Ordering::Less));

        assert_eq!(unknown_written.partial_cmp(&t0), Some(Ordering::Greater));
        assert_eq!(unknown_written.partial_cmp(&t1), Some(Ordering::Greater));
        assert_eq!(unknown_written.partial_cmp(&random), None);
        assert_eq!(unknown_written.partial_cmp(&unknown_written), Some(Ordering::Equal));
        assert_eq!(unknown_written.partial_cmp(&unwritten), Some(Ordering::Less));

        assert_eq!(unwritten.partial_cmp(&t0), Some(Ordering::Greater));
        assert_eq!(unwritten.partial_cmp(&t1), Some(Ordering::Greater));
        assert_eq!(unwritten.partial_cmp(&random), Some(Ordering::Greater));
        assert_eq!(unwritten.partial_cmp(&unknown_written), Some(Ordering::Greater));
        assert_eq!(unwritten.partial_cmp(&unwritten), Some(Ordering::Equal));
    }

    #[test]
    fn test_stack_ordering() {
        let mut s1 = Stack::default();
        let mut s2 = Stack::default();

        assert_eq!(s1.partial_cmp(&s2), Some(Ordering::Equal));
        s1.set(0, 0.into());
        assert_eq!(s1.partial_cmp(&s2), Some(Ordering::Less));
        assert_eq!(s2.partial_cmp(&s1), Some(Ordering::Greater));
        s2.set(1, 1.into());
        assert_eq!(s1.partial_cmp(&s2), None);
        assert_eq!(s2.partial_cmp(&s1), None);
    }

    #[test]
    fn test_context_ordering() {
        let mut c1 = ComputationContext::default();
        let mut c2 = ComputationContext::default();

        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Equal));

        c1.array_bounds.insert(1.into(), 5);
        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Less));
        assert_eq!(c2.partial_cmp(&c1), Some(Ordering::Greater));

        c2.array_bounds.insert(1.into(), 7);
        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Greater));
        assert_eq!(c2.partial_cmp(&c1), Some(Ordering::Less));

        c1.array_bounds.insert(2.into(), 9);
        assert_eq!(c1.partial_cmp(&c2), None);
        assert_eq!(c2.partial_cmp(&c1), None);

        c2.array_bounds.insert(2.into(), 9);
        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Greater));
        assert_eq!(c2.partial_cmp(&c1), Some(Ordering::Less));

        c2.array_bounds.insert(3.into(), 12);
        assert_eq!(c1.partial_cmp(&c2), Some(Ordering::Greater));
        assert_eq!(c2.partial_cmp(&c1), Some(Ordering::Less));

        c1.pc = 8;
        assert_eq!(c1.partial_cmp(&c2), None);
        assert_eq!(c2.partial_cmp(&c1), None);
    }

    #[test]
    fn test_stack_access() {
        let mut s = Stack::default();

        // Store data in the range [8, 26) and verify that `read_data_ptr()` fails for any
        // reads outside of that range.
        assert!(s
            .store(1, StackOffset(8), Type::unknown_written_scalar_value(), DataWidth::U64)
            .is_ok());
        assert!(s
            .store(1, StackOffset(16), Type::unknown_written_scalar_value(), DataWidth::U64)
            .is_ok());
        assert!(s
            .store(1, StackOffset(24), Type::unknown_written_scalar_value(), DataWidth::U16)
            .is_ok());

        for offset in 0..32 {
            for end in (offset + 1)..32 {
                assert_eq!(
                    s.read_data_ptr(2, StackOffset(offset), (end - offset) as u64).is_ok(),
                    offset >= 8 && end <= 26
                );
            }
        }

        // Verify that overflows are handled properly.
        assert!(s.read_data_ptr(2, StackOffset(12), u64::MAX - 2).is_err());
    }
}
