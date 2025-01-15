// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::executor::execute;
use crate::verifier::VerifiedEbpfProgram;
use crate::{
    CbpfConfig, DataWidth, EbpfError, EbpfInstruction, MapSchema, MemoryId, StructAccess, Type,
    BPF_CALL, BPF_DW, BPF_JMP, BPF_LDDW, BPF_PSEUDO_MAP_IDX, BPF_SIZE_MASK,
};
use derivative::Derivative;
use std::collections::HashMap;
use std::fmt::Formatter;
use std::mem::size_of;
use zerocopy::{FromBytes, Immutable, IntoBytes};

/// Trait that should be implemented for arguments passed to eBPF programs.
pub trait ProgramArgument: Into<BpfValue> {
    /// Returns eBPF type that corresponds to `Self`. Used when program argument types
    /// are checked statically.
    fn get_type() -> &'static Type;

    /// Returns eBPF type for a specific value of `Self`. For most types this is the
    /// same type that's returned by `get_type()`, but that's not always the case.
    /// In particular for scalar values this will return `Type::ScalarValue` with
    /// the actual value of the scalar and with `unknown_mask = 0`.
    fn get_value_type(&self) -> Type {
        Self::get_type().clone()
    }
}

/// Trait that should be implemented for types that can be converted from `BpfValue`.
/// Used to get a `Packet` when loading a value from the packet.
pub trait FromBpfValue<C>: Sized {
    /// # Safety
    /// Should be called only by the eBPF interpreter when executing verified eBPF code.
    unsafe fn from_bpf_value(context: &mut C, v: BpfValue) -> Self;
}

impl ProgramArgument for () {
    fn get_type() -> &'static Type {
        &Type::UNINITIALIZED
    }
}

impl<C> FromBpfValue<C> for () {
    unsafe fn from_bpf_value(_context: &mut C, _v: BpfValue) -> Self {
        unreachable!();
    }
}

impl ProgramArgument for usize {
    fn get_type() -> &'static Type {
        &Type::UNKNOWN_SCALAR
    }

    fn get_value_type(&self) -> Type {
        Type::from(*self as u64)
    }
}

impl<'a, T, C> FromBpfValue<C> for &'a mut T
where
    &'a mut T: ProgramArgument,
{
    unsafe fn from_bpf_value(_context: &mut C, v: BpfValue) -> Self {
        &mut *v.as_ptr::<T>()
    }
}

impl<'a, T, C> FromBpfValue<C> for &'a T
where
    &'a T: ProgramArgument,
{
    unsafe fn from_bpf_value(_context: &mut C, v: BpfValue) -> Self {
        &*v.as_ptr::<T>()
    }
}

pub trait EbpfProgramContext {
    /// Context for an invocation of an eBPF program.
    type RunContext<'a>;

    /// Packet used by the program.
    type Packet<'a>: Packet + FromBpfValue<Self::RunContext<'a>>;

    /// Arguments passed to the program
    type Arg1<'a>: ProgramArgument;
    type Arg2<'a>: ProgramArgument;
    type Arg3<'a>: ProgramArgument;
    type Arg4<'a>: ProgramArgument;
    type Arg5<'a>: ProgramArgument;
}

/// Trait that should be implemented by packets passed to eBPF programs.
pub trait Packet {
    fn load(&self, offset: i32, width: DataWidth) -> Option<BpfValue>;
}

impl Packet for () {
    fn load(&self, _offset: i32, _width: DataWidth) -> Option<BpfValue> {
        None
    }
}

/// Simple `Packet` implementation for packets that can be accessed directly.
impl<P: IntoBytes + Immutable> Packet for &P {
    fn load(&self, offset: i32, width: DataWidth) -> Option<BpfValue> {
        let data = (*self).as_bytes();
        if offset < 0 || offset as usize >= data.len() {
            return None;
        }
        let slice = &data[(offset as usize)..];
        match width {
            DataWidth::U8 => u8::read_from_prefix(slice).ok().map(|(v, _)| v.into()),
            DataWidth::U16 => u16::read_from_prefix(slice).ok().map(|(v, _)| v.into()),
            DataWidth::U32 => u32::read_from_prefix(slice).ok().map(|(v, _)| v.into()),
            DataWidth::U64 => u64::read_from_prefix(slice).ok().map(|(v, _)| v.into()),
        }
    }
}

/// A context for a BPF program that's compatible with eBPF and cBPF.
pub trait BpfProgramContext {
    type RunContext<'a>;
    type Packet<'a>: ProgramArgument + Packet + FromBpfValue<Self::RunContext<'a>>;

    const CBPF_CONFIG: &'static CbpfConfig;

    fn get_arg_types() -> Vec<Type> {
        vec![<Self::Packet<'_> as ProgramArgument>::get_type().clone()]
    }
}

impl<T: BpfProgramContext + ?Sized> EbpfProgramContext for T {
    type RunContext<'a> = <T as BpfProgramContext>::RunContext<'a>;
    type Packet<'a> = T::Packet<'a>;
    type Arg1<'a> = T::Packet<'a>;
    type Arg2<'a> = ();
    type Arg3<'a> = ();
    type Arg4<'a> = ();
    type Arg5<'a> = ();
}

#[derive(Clone, Copy, Debug)]
pub struct BpfValue(u64);

static_assertions::const_assert_eq!(size_of::<BpfValue>(), size_of::<*const u8>());

impl Default for BpfValue {
    fn default() -> Self {
        Self::from(0)
    }
}

impl From<()> for BpfValue {
    fn from(_v: ()) -> Self {
        Self(0)
    }
}

impl From<i32> for BpfValue {
    fn from(v: i32) -> Self {
        Self((v as u32) as u64)
    }
}

impl From<u8> for BpfValue {
    fn from(v: u8) -> Self {
        Self::from(v as u64)
    }
}

impl From<u16> for BpfValue {
    fn from(v: u16) -> Self {
        Self::from(v as u64)
    }
}

impl From<u32> for BpfValue {
    fn from(v: u32) -> Self {
        Self::from(v as u64)
    }
}
impl From<u64> for BpfValue {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl From<usize> for BpfValue {
    fn from(v: usize) -> Self {
        Self(v as u64)
    }
}

impl<T> From<*const T> for BpfValue {
    fn from(v: *const T) -> Self {
        Self(v as u64)
    }
}

impl<T> From<*mut T> for BpfValue {
    fn from(v: *mut T) -> Self {
        Self(v as u64)
    }
}

impl<T> From<&'_ T> for BpfValue {
    fn from(v: &'_ T) -> Self {
        Self((v as *const T) as u64)
    }
}

impl<T> From<&'_ mut T> for BpfValue {
    fn from(v: &'_ mut T) -> Self {
        Self((v as *const T) as u64)
    }
}
impl From<BpfValue> for u8 {
    fn from(v: BpfValue) -> u8 {
        v.0 as u8
    }
}

impl From<BpfValue> for u16 {
    fn from(v: BpfValue) -> u16 {
        v.0 as u16
    }
}

impl From<BpfValue> for u32 {
    fn from(v: BpfValue) -> u32 {
        v.0 as u32
    }
}

impl From<BpfValue> for u64 {
    fn from(v: BpfValue) -> u64 {
        v.0
    }
}

impl From<BpfValue> for usize {
    fn from(v: BpfValue) -> usize {
        v.0 as usize
    }
}

impl BpfValue {
    pub fn as_u8(&self) -> u8 {
        self.0 as u8
    }

    pub fn as_u16(&self) -> u16 {
        self.0 as u16
    }

    pub fn as_u32(&self) -> u32 {
        self.0 as u32
    }

    pub fn as_i32(&self) -> i32 {
        self.0 as i32
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn as_usize(&self) -> usize {
        self.0 as usize
    }

    pub fn as_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
    }
}

impl From<BpfValue> for () {
    fn from(_v: BpfValue) -> Self {
        ()
    }
}

#[derive(Derivative)]
#[derivative(Clone(bound = ""))]
pub struct EbpfHelperImpl<C: EbpfProgramContext>(
    pub  for<'a> fn(
        &mut C::RunContext<'a>,
        BpfValue,
        BpfValue,
        BpfValue,
        BpfValue,
        BpfValue,
    ) -> BpfValue,
);

/// A mapping for a field in a struct where the original ebpf program knows a different offset and
/// data size than the one it receives from the kernel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldMapping {
    /// The offset of the field as known by the original ebpf program.
    pub source_offset: usize,
    /// The actual offset of the field in the data provided by the kernel.
    pub target_offset: usize,
}

#[derive(Clone, Debug)]
pub struct StructMapping {
    /// Memory ID used in the struct definition.
    pub memory_id: MemoryId,

    /// The list of mappings in the buffer. The verifier must rewrite the actual ebpf to ensure
    /// the right offset and operand are use to access the mapped fields. Mappings are allowed
    /// only for pointer fields.
    pub fields: Vec<FieldMapping>,
}

#[derive(Clone, Debug)]
pub struct MapDescriptor {
    pub schema: MapSchema,
    pub ptr: BpfValue,
}

pub trait ArgumentTypeChecker<C: EbpfProgramContext>: Sized {
    fn link(program: &VerifiedEbpfProgram) -> Result<Self, EbpfError>;
    fn run_time_check<'a>(
        &self,
        arg1: &C::Arg1<'a>,
        arg2: &C::Arg2<'a>,
        arg3: &C::Arg3<'a>,
        arg4: &C::Arg4<'a>,
        arg5: &C::Arg5<'a>,
    ) -> Result<(), EbpfError>;
}

pub struct StaticTypeChecker();

impl<C: EbpfProgramContext> ArgumentTypeChecker<C> for StaticTypeChecker {
    fn link(program: &VerifiedEbpfProgram) -> Result<Self, EbpfError> {
        let arg_types = [
            C::Arg1::get_type(),
            C::Arg2::get_type(),
            C::Arg3::get_type(),
            C::Arg4::get_type(),
            C::Arg5::get_type(),
        ];
        for i in 0..5 {
            let verified_type = program.args.get(i).unwrap_or(&Type::UNINITIALIZED);
            if !arg_types[i].is_subtype(verified_type) {
                return Err(EbpfError::ProgramLinkError(format!(
                    "Type of argument {} doesn't match. Verified type: {:?}. Context type: {:?}",
                    i + 1,
                    verified_type,
                    arg_types[i],
                )));
            }
        }

        Ok(Self())
    }

    fn run_time_check<'a>(
        &self,
        _arg1: &C::Arg1<'a>,
        _arg2: &C::Arg2<'a>,
        _arg3: &C::Arg3<'a>,
        _arg4: &C::Arg4<'a>,
        _arg5: &C::Arg5<'a>,
    ) -> Result<(), EbpfError> {
        // No-op since argument types were checked in `link()`.
        Ok(())
    }
}

pub struct DynamicTypeChecker {
    types: Vec<Type>,
}

impl<C: EbpfProgramContext> ArgumentTypeChecker<C> for DynamicTypeChecker {
    fn link(program: &VerifiedEbpfProgram) -> Result<Self, EbpfError> {
        Ok(Self { types: program.args.clone() })
    }

    fn run_time_check<'a>(
        &self,
        arg1: &C::Arg1<'a>,
        arg2: &C::Arg2<'a>,
        arg3: &C::Arg3<'a>,
        arg4: &C::Arg4<'a>,
        arg5: &C::Arg5<'a>,
    ) -> Result<(), EbpfError> {
        let arg_types = [
            arg1.get_value_type(),
            arg2.get_value_type(),
            arg3.get_value_type(),
            arg4.get_value_type(),
            arg5.get_value_type(),
        ];
        for i in 0..5 {
            let verified_type = self.types.get(i).unwrap_or(&Type::UNINITIALIZED);
            if !&arg_types[i].is_subtype(verified_type) {
                return Err(EbpfError::ProgramLinkError(format!(
                    "Type of argument {} doesn't match. Verified type: {:?}. Value type: {:?}",
                    i + 1,
                    verified_type,
                    arg_types[i],
                )));
            }
        }

        Ok(())
    }
}

/// An abstraction over an eBPF program and its registered helper functions.
pub struct EbpfProgram<C: EbpfProgramContext, T: ArgumentTypeChecker<C> = StaticTypeChecker> {
    pub(crate) code: Vec<EbpfInstruction>,
    pub(crate) helpers: HashMap<u32, EbpfHelperImpl<C>>,
    type_checker: T,
}

impl<C: EbpfProgramContext, T: ArgumentTypeChecker<C>> std::fmt::Debug for EbpfProgram<C, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("EbpfProgram").field("code", &self.code).finish()
    }
}

impl<C: EbpfProgramContext, T: ArgumentTypeChecker<C>> EbpfProgram<C, T> {
    pub fn code(&self) -> &[EbpfInstruction] {
        &self.code[..]
    }
}

impl<C, T: ArgumentTypeChecker<C>> EbpfProgram<C, T>
where
    C: for<'a> EbpfProgramContext<Arg2<'a> = (), Arg3<'a> = (), Arg4<'a> = (), Arg5<'a> = ()>,
{
    pub fn run_with_1_argument<'a>(
        &self,
        run_context: &mut C::RunContext<'a>,
        arg1: C::Arg1<'a>,
    ) -> u64 {
        self.type_checker
            .run_time_check(&arg1, &(), &(), &(), &())
            .expect("Failed argument type check");
        execute(&self.code[..], &self.helpers, run_context, &[arg1.into()])
    }
}

impl<C, T: ArgumentTypeChecker<C>> EbpfProgram<C, T>
where
    C: for<'a> EbpfProgramContext<Arg3<'a> = (), Arg4<'a> = (), Arg5<'a> = ()>,
{
    pub fn run_with_2_arguments<'a>(
        &self,
        run_context: &mut C::RunContext<'a>,
        arg1: C::Arg1<'a>,
        arg2: C::Arg2<'a>,
    ) -> u64 {
        self.type_checker
            .run_time_check(&arg1, &arg2, &(), &(), &())
            .expect("Failed argument type check");
        execute(&self.code[..], &self.helpers, run_context, &[arg1.into(), arg2.into()])
    }
}

impl<C: BpfProgramContext, T: ArgumentTypeChecker<C>> EbpfProgram<C, T>
where
    for<'a> C: BpfProgramContext,
{
    /// Executes the current program on the specified `packet`.
    /// The program receives a pointer to the `packet` and the size of the packet as the first
    /// two arguments.
    pub fn run<'a>(
        &self,
        run_context: &mut <C as EbpfProgramContext>::RunContext<'a>,
        packet: C::Packet<'a>,
    ) -> u64 {
        self.run_with_1_argument(run_context, packet)
    }
}

/// Rewrites the code to ensure mapped fields are correctly handled. Returns
/// runnable `EbpfProgram<C>`.
pub fn link_program_internal<C: EbpfProgramContext, T: ArgumentTypeChecker<C>>(
    program: &VerifiedEbpfProgram,
    struct_mappings: &[StructMapping],
    maps: &[MapDescriptor],
    helpers: HashMap<u32, EbpfHelperImpl<C>>,
) -> Result<EbpfProgram<C, T>, EbpfError> {
    let type_checker = T::link(program)?;

    let mut code = program.code.clone();

    // Update offsets in the instructions that access structs.
    for StructAccess { pc, memory_id, field_offset, is_32_bit_ptr_load } in
        program.struct_access_instructions.iter()
    {
        let field_mapping =
            struct_mappings.iter().find(|m| m.memory_id == *memory_id).and_then(|struct_map| {
                struct_map.fields.iter().find(|m| m.source_offset == *field_offset)
            });

        if let Some(field_mapping) = field_mapping {
            let instruction = &mut code[*pc];

            // Note that `instruction.off` may be different from `field.source_offset`. It's adjuststed
            // by the difference between `target_offset` and `source_offset` to ensure the instructions
            // will access the right field.
            let offset_diff = i16::try_from(
                i64::try_from(field_mapping.target_offset).unwrap()
                    - i64::try_from(field_mapping.source_offset).unwrap(),
            )
            .unwrap();

            instruction.off = instruction.off.checked_add(offset_diff).ok_or_else(|| {
                EbpfError::ProgramLinkError(format!("Struct field offset overflow at PC {}", *pc))
            })?;

            // 32-bit pointer loads must be updated to 64-bit loads.
            if *is_32_bit_ptr_load {
                instruction.code = (instruction.code & !BPF_SIZE_MASK) | BPF_DW;
            }
        } else {
            if *is_32_bit_ptr_load {
                return Err(EbpfError::ProgramLinkError(format!(
                    "32-bit field isn't mapped at pc  {}",
                    *pc,
                )));
            }
        }
    }

    for pc in 0..code.len() {
        let instruction = &mut code[pc];

        // Check that we have implementations for all helper calls.
        if instruction.code == (BPF_JMP | BPF_CALL) {
            let helper_id = instruction.imm as u32;
            if helpers.get(&helper_id).is_none() {
                return Err(EbpfError::ProgramLinkError(format!(
                    "Missing implementation for helper with id={}",
                    helper_id,
                )));
            }
        }

        // Link maps.
        if instruction.code == BPF_LDDW {
            // If the instruction references BPF_PSEUDO_MAP_FD, then we need to look up the map fd
            // and create a reference from this program to that object.
            match instruction.src_reg() {
                0 => (),
                BPF_PSEUDO_MAP_IDX => {
                    let map_index = usize::try_from(instruction.imm)
                        .expect("negative map index in a verified program");
                    let MapDescriptor { schema, ptr: map_ptr } =
                        maps.get(map_index).ok_or_else(|| {
                            EbpfError::ProgramLinkError(format!("Invalid map_index: {}", map_index))
                        })?;
                    assert!(*schema == program.maps[map_index]);

                    let map_ptr = map_ptr.as_u64();
                    let (high, low) = ((map_ptr >> 32) as i32, map_ptr as i32);
                    instruction.set_src_reg(0);
                    instruction.imm = low;

                    // The code was verified, so this is not expected to overflow.
                    let next_instruction = &mut code[pc + 1];
                    next_instruction.imm = high;
                }
                value => {
                    return Err(EbpfError::ProgramLinkError(format!(
                        "Unsupported value for src_reg in lddw: {}",
                        value,
                    )));
                }
            }
        }
    }

    Ok(EbpfProgram { code, helpers, type_checker })
}

/// Rewrites the code to ensure mapped fields are correctly handled. Returns
/// runnable `EbpfProgram<C>`.
pub fn link_program<C: EbpfProgramContext>(
    program: &VerifiedEbpfProgram,
    struct_mappings: &[StructMapping],
    maps: &[MapDescriptor],
    helpers: HashMap<u32, EbpfHelperImpl<C>>,
) -> Result<EbpfProgram<C>, EbpfError> {
    link_program_internal::<C, StaticTypeChecker>(program, struct_mappings, maps, helpers)
}

/// Same as above, but allows to check argument types in runtime instead of in link time.
pub fn link_program_dynamic<C: EbpfProgramContext>(
    program: &VerifiedEbpfProgram,
    struct_mappings: &[StructMapping],
    maps: &[MapDescriptor],
    helpers: HashMap<u32, EbpfHelperImpl<C>>,
) -> Result<EbpfProgram<C, DynamicTypeChecker>, EbpfError> {
    link_program_internal::<C, DynamicTypeChecker>(program, struct_mappings, maps, helpers)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::api::*;
    use crate::conformance::test::parse_asm;
    use crate::{
        convert_and_link_cbpf, verify_program, CallingContext, CbpfLenInstruction, FieldDescriptor,
        FieldMapping, FieldType, NullVerifierLogger, ProgramArgument, StructDescriptor, Type,
    };
    use linux_uapi::{
        seccomp_data, sock_filter, AUDIT_ARCH_AARCH64, AUDIT_ARCH_X86_64, SECCOMP_RET_ALLOW,
        SECCOMP_RET_TRAP,
    };
    use std::mem::offset_of;
    use std::sync::{Arc, LazyLock};
    use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

    const BPF_ALU_ADD_K: u16 = (BPF_ALU | BPF_ADD | BPF_K) as u16;
    const BPF_ALU_SUB_K: u16 = (BPF_ALU | BPF_SUB | BPF_K) as u16;
    const BPF_ALU_MUL_K: u16 = (BPF_ALU | BPF_MUL | BPF_K) as u16;
    const BPF_ALU_DIV_K: u16 = (BPF_ALU | BPF_DIV | BPF_K) as u16;
    const BPF_ALU_AND_K: u16 = (BPF_ALU | BPF_AND | BPF_K) as u16;
    const BPF_ALU_OR_K: u16 = (BPF_ALU | BPF_OR | BPF_K) as u16;
    const BPF_ALU_XOR_K: u16 = (BPF_ALU | BPF_XOR | BPF_K) as u16;
    const BPF_ALU_LSH_K: u16 = (BPF_ALU | BPF_LSH | BPF_K) as u16;
    const BPF_ALU_RSH_K: u16 = (BPF_ALU | BPF_RSH | BPF_K) as u16;

    const BPF_ALU_OR_X: u16 = (BPF_ALU | BPF_OR | BPF_X) as u16;

    const BPF_LD_W_ABS: u16 = (BPF_LD | BPF_ABS | BPF_W) as u16;
    const BPF_LD_W_MEM: u16 = (BPF_LD | BPF_MEM | BPF_W) as u16;
    const BPF_JEQ_K: u16 = (BPF_JMP | BPF_JEQ | BPF_K) as u16;
    const BPF_JSET_K: u16 = (BPF_JMP | BPF_JSET | BPF_K) as u16;
    const BPF_RET_K: u16 = (BPF_RET | BPF_K) as u16;
    const BPF_RET_A: u16 = (BPF_RET | BPF_A) as u16;
    const BPF_ST_REG: u16 = BPF_ST as u16;
    const BPF_MISC_TAX: u16 = (BPF_MISC | BPF_TAX) as u16;

    pub const TEST_CBPF_CONFIG: CbpfConfig = CbpfConfig {
        len: CbpfLenInstruction::Static { len: size_of::<seccomp_data>() as i32 },
        allow_msh: true,
    };

    struct TestProgramContext {}

    impl BpfProgramContext for TestProgramContext {
        type RunContext<'a> = ();
        type Packet<'a> = &'a seccomp_data;
        const CBPF_CONFIG: &'static CbpfConfig = &TEST_CBPF_CONFIG;
    }

    static SECCOMP_DATA_TYPE: LazyLock<Type> =
        LazyLock::new(|| Type::PtrToMemory { id: MemoryId::new(), offset: 0, buffer_size: 0 });

    impl ProgramArgument for &'_ seccomp_data {
        fn get_type() -> &'static Type {
            &*SECCOMP_DATA_TYPE
        }
    }

    fn with_prg_assert_result(
        prg: &EbpfProgram<TestProgramContext>,
        mut data: seccomp_data,
        result: u32,
        msg: &str,
    ) {
        let return_value = prg.run(&mut (), &mut data);
        assert_eq!(return_value, result as u64, "{}: filter return value is {}", msg, return_value);
    }

    #[test]
    fn test_filter_with_dw_load() {
        let test_prg = [
            // Check data.arch
            sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 4 },
            sock_filter { code: BPF_JEQ_K, jt: 1, jf: 0, k: AUDIT_ARCH_X86_64 },
            // Return 1 if arch is wrong
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: 1 },
            // Load data.nr (the syscall number)
            sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 0 },
            // Always allow 41
            sock_filter { code: BPF_JEQ_K, jt: 0, jf: 1, k: 41 },
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_ALLOW },
            // Don't allow 115
            sock_filter { code: BPF_JEQ_K, jt: 0, jf: 1, k: 115 },
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_TRAP },
            // For other syscalls, check the args
            // A common hack to deal with 64-bit numbers in BPF: deal
            // with 32 bits at a time.
            // First, Load arg0's most significant 32 bits in M[0]
            sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 16 },
            sock_filter { code: BPF_ST_REG, jt: 0, jf: 0, k: 0 },
            // Load arg0's least significant 32 bits into M[1]
            sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 20 },
            sock_filter { code: BPF_ST_REG, jt: 0, jf: 0, k: 1 },
            // JSET is A & k.  Check the first 32 bits.  If the test
            // is successful, jump, otherwise, check the next 32 bits.
            sock_filter { code: BPF_LD_W_MEM, jt: 0, jf: 0, k: 0 },
            sock_filter { code: BPF_JSET_K, jt: 2, jf: 0, k: 4294967295 },
            sock_filter { code: BPF_LD_W_MEM, jt: 0, jf: 0, k: 1 },
            sock_filter { code: BPF_JSET_K, jt: 0, jf: 1, k: 4294967292 },
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_TRAP },
            sock_filter { code: BPF_RET_K, jt: 0, jf: 0, k: SECCOMP_RET_ALLOW },
        ];

        let prg =
            convert_and_link_cbpf::<TestProgramContext>(&test_prg).expect("Error parsing program");

        with_prg_assert_result(
            &prg,
            seccomp_data { arch: AUDIT_ARCH_AARCH64, ..Default::default() },
            1,
            "Did not reject incorrect arch",
        );

        with_prg_assert_result(
            &prg,
            seccomp_data { arch: AUDIT_ARCH_X86_64, nr: 41, ..Default::default() },
            SECCOMP_RET_ALLOW,
            "Did not pass simple RET_ALLOW",
        );

        with_prg_assert_result(
            &prg,
            seccomp_data {
                arch: AUDIT_ARCH_X86_64,
                nr: 100,
                args: [0xFF00000000, 0, 0, 0, 0, 0],
                ..Default::default()
            },
            SECCOMP_RET_TRAP,
            "Did not treat load of first 32 bits correctly",
        );

        with_prg_assert_result(
            &prg,
            seccomp_data {
                arch: AUDIT_ARCH_X86_64,
                nr: 100,
                args: [0x4, 0, 0, 0, 0, 0],
                ..Default::default()
            },
            SECCOMP_RET_TRAP,
            "Did not correctly reject load of second 32 bits",
        );

        with_prg_assert_result(
            &prg,
            seccomp_data {
                arch: AUDIT_ARCH_X86_64,
                nr: 100,
                args: [0x0, 0, 0, 0, 0, 0],
                ..Default::default()
            },
            SECCOMP_RET_ALLOW,
            "Did not correctly accept load of second 32 bits",
        );
    }

    #[test]
    fn test_alu_insns() {
        {
            let test_prg = [
                // Load data.nr (the syscall number)
                sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 0 }, // = 1, 11
                // Do some math.
                sock_filter { code: BPF_ALU_ADD_K, jt: 0, jf: 0, k: 3 }, // = 4, 14
                sock_filter { code: BPF_ALU_SUB_K, jt: 0, jf: 0, k: 2 }, // = 2, 12
                sock_filter { code: BPF_MISC_TAX, jt: 0, jf: 0, k: 0 },  // 2, 12 -> X
                sock_filter { code: BPF_ALU_MUL_K, jt: 0, jf: 0, k: 8 }, // = 16, 96
                sock_filter { code: BPF_ALU_DIV_K, jt: 0, jf: 0, k: 2 }, // = 8, 48
                sock_filter { code: BPF_ALU_AND_K, jt: 0, jf: 0, k: 15 }, // = 8, 0
                sock_filter { code: BPF_ALU_OR_K, jt: 0, jf: 0, k: 16 }, // = 24, 16
                sock_filter { code: BPF_ALU_XOR_K, jt: 0, jf: 0, k: 7 }, // = 31, 23
                sock_filter { code: BPF_ALU_LSH_K, jt: 0, jf: 0, k: 2 }, // = 124, 92
                sock_filter { code: BPF_ALU_OR_X, jt: 0, jf: 0, k: 1 },  // = 127, 92
                sock_filter { code: BPF_ALU_RSH_K, jt: 0, jf: 0, k: 1 }, // = 63, 46
                sock_filter { code: BPF_RET_A, jt: 0, jf: 0, k: 0 },
            ];

            let prg = convert_and_link_cbpf::<TestProgramContext>(&test_prg)
                .expect("Error parsing program");

            with_prg_assert_result(
                &prg,
                seccomp_data { nr: 1, ..Default::default() },
                63,
                "BPF math does not work",
            );

            with_prg_assert_result(
                &prg,
                seccomp_data { nr: 11, ..Default::default() },
                46,
                "BPF math does not work",
            );
        }

        {
            // Negative numbers simple check
            let test_prg = [
                // Load data.nr (the syscall number)
                sock_filter { code: BPF_LD_W_ABS, jt: 0, jf: 0, k: 0 }, // = -1
                sock_filter { code: BPF_ALU_SUB_K, jt: 0, jf: 0, k: 2 }, // = -3
                sock_filter { code: BPF_RET_A, jt: 0, jf: 0, k: 0 },
            ];

            let prg = convert_and_link_cbpf::<TestProgramContext>(&test_prg)
                .expect("Error parsing program");

            with_prg_assert_result(
                &prg,
                seccomp_data { nr: -1, ..Default::default() },
                u32::MAX - 2,
                "BPF math does not work",
            );
        }
    }

    // Test BPF_MSH cBPF instruction.
    #[test]
    fn test_ld_msh() {
        let test_prg = [
            // X <- 4 * (P[0] & 0xf)
            sock_filter { code: (BPF_LDX | BPF_MSH | BPF_B) as u16, jt: 0, jf: 0, k: 0 },
            // A <- X
            sock_filter { code: (BPF_MISC | BPF_TXA) as u16, jt: 0, jf: 0, k: 0 },
            // ret A
            sock_filter { code: BPF_RET_A, jt: 0, jf: 0, k: 0 },
        ];

        let prg =
            convert_and_link_cbpf::<TestProgramContext>(&test_prg).expect("Error parsing program");

        for i in [0x00, 0x01, 0x07, 0x15, 0xff].iter() {
            with_prg_assert_result(
                &prg,
                seccomp_data { nr: *i, ..Default::default() },
                4 * (*i & 0xf) as u32,
                "BPF math does not work",
            )
        }
    }

    #[test]
    fn test_static_packet_len() {
        let test_prg = [
            // A <- packet_len
            sock_filter { code: (BPF_LD | BPF_LEN | BPF_W) as u16, jt: 0, jf: 0, k: 0 },
            // ret A
            sock_filter { code: BPF_RET_A, jt: 0, jf: 0, k: 0 },
        ];

        let prg =
            convert_and_link_cbpf::<TestProgramContext>(&test_prg).expect("Error parsing program");

        let data = seccomp_data::default();
        assert_eq!(prg.run(&mut (), &data), size_of::<seccomp_data>() as u64);
    }

    // A packet used by `test_variable_packet_len()` below to verify the case when the packet
    // length is stored as a struct field.
    #[repr(C)]
    #[derive(Debug, Default, IntoBytes, Immutable, KnownLayout, FromBytes)]
    struct VariableLengthPacket {
        foo: u32,
        len: i32,
        bar: u64,
    }

    static VARIABLE_LENGTH_PACKET_TYPE: LazyLock<Type> = LazyLock::new(|| Type::PtrToMemory {
        id: MemoryId::new(),
        offset: 0,
        buffer_size: size_of::<VariableLengthPacket>() as u64,
    });

    impl ProgramArgument for &'_ VariableLengthPacket {
        fn get_type() -> &'static Type {
            &*VARIABLE_LENGTH_PACKET_TYPE
        }
    }

    pub const VARIABLE_LENGTH_CBPF_CONFIG: CbpfConfig = CbpfConfig {
        len: CbpfLenInstruction::ContextField {
            offset: offset_of!(VariableLengthPacket, len) as i16,
        },
        allow_msh: true,
    };

    struct VariableLengthPacketContext {}

    impl BpfProgramContext for VariableLengthPacketContext {
        type RunContext<'a> = ();
        type Packet<'a> = &'a VariableLengthPacket;
        const CBPF_CONFIG: &'static CbpfConfig = &VARIABLE_LENGTH_CBPF_CONFIG;
    }

    #[test]
    fn test_variable_packet_len() {
        let test_prg = [
            // A <- packet_len
            sock_filter { code: (BPF_LD | BPF_LEN | BPF_W) as u16, jt: 0, jf: 0, k: 0 },
            // ret A
            sock_filter { code: BPF_RET_A, jt: 0, jf: 0, k: 0 },
        ];

        let prg = convert_and_link_cbpf::<VariableLengthPacketContext>(&test_prg)
            .expect("Error parsing program");
        let data = VariableLengthPacket { len: 42, ..VariableLengthPacket::default() };
        assert_eq!(prg.run(&mut (), &data), data.len as u64);
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone, IntoBytes, Immutable, KnownLayout, FromBytes)]
    struct TestArgument {
        // A field that should not be writable by the program.
        pub read_only_field: u32,
        pub _padding1: u32,
        /// Pointer to an array.
        pub data: u64,
        /// End of the array.
        pub data_end: u64,
        // A field that can be updated by the program.
        pub mutable_field: u32,
        pub _padding2: u32,
    }

    static TEST_ARG_TYPE: LazyLock<Type> = LazyLock::new(|| {
        let data_memory_id = MemoryId::new();
        let descriptor = Arc::new(StructDescriptor {
            fields: vec![
                FieldDescriptor {
                    offset: std::mem::offset_of!(TestArgument, read_only_field),
                    field_type: FieldType::Scalar { size: 4 },
                },
                FieldDescriptor {
                    offset: std::mem::offset_of!(TestArgument, data),
                    field_type: FieldType::PtrToArray {
                        is_32_bit: false,
                        id: data_memory_id.clone(),
                    },
                },
                FieldDescriptor {
                    offset: std::mem::offset_of!(TestArgument, data_end),
                    field_type: FieldType::PtrToEndArray {
                        is_32_bit: false,
                        id: data_memory_id.clone(),
                    },
                },
                FieldDescriptor {
                    offset: std::mem::offset_of!(TestArgument, mutable_field),
                    field_type: FieldType::MutableScalar { size: 4 },
                },
            ],
        });

        Type::PtrToStruct { id: MemoryId::new(), offset: 0, descriptor }
    });

    impl Default for TestArgument {
        fn default() -> Self {
            Self {
                read_only_field: 1,
                _padding1: 0,
                data: 0,
                data_end: 0,
                mutable_field: 2,
                _padding2: 0,
            }
        }
    }

    impl TestArgument {
        fn from_data(data: &[u64]) -> Self {
            let ptr_range = data.as_ptr_range();
            Self {
                data: ptr_range.start as u64,
                data_end: ptr_range.end as u64,
                ..Default::default()
            }
        }
    }

    impl ProgramArgument for &'_ mut TestArgument {
        fn get_type() -> &'static Type {
            &*TEST_ARG_TYPE
        }
    }

    // A version of TestArgument with 32-bit remapped pointers. It's used to define struct layout
    // for eBPF programs, but not used in the Rust code directly.
    #[repr(C)]
    struct TestArgument32 {
        pub read_only_field: u32,
        pub data: u32,
        pub data_end: u32,
        pub mutable_field: u32,
    }

    #[repr(C)]
    struct TestArgument32BitMapped(TestArgument);

    static TEST_ARG_32_BIT_MEMORY_ID: LazyLock<MemoryId> = LazyLock::new(|| MemoryId::new());
    static TEST_ARG_32_BIT_TYPE: LazyLock<Type> = LazyLock::new(|| {
        let data_memory_id = MemoryId::new();
        let descriptor = Arc::new(StructDescriptor {
            fields: vec![
                FieldDescriptor {
                    offset: std::mem::offset_of!(TestArgument32, read_only_field),
                    field_type: FieldType::Scalar { size: 4 },
                },
                FieldDescriptor {
                    offset: std::mem::offset_of!(TestArgument32, data),
                    field_type: FieldType::PtrToArray {
                        is_32_bit: true,
                        id: data_memory_id.clone(),
                    },
                },
                FieldDescriptor {
                    offset: std::mem::offset_of!(TestArgument32, data_end),
                    field_type: FieldType::PtrToEndArray {
                        is_32_bit: true,
                        id: data_memory_id.clone(),
                    },
                },
                FieldDescriptor {
                    offset: std::mem::offset_of!(TestArgument32, mutable_field),
                    field_type: FieldType::MutableScalar { size: 4 },
                },
            ],
        });

        Type::PtrToStruct { id: TEST_ARG_32_BIT_MEMORY_ID.clone(), offset: 0, descriptor }
    });

    impl ProgramArgument for &'_ TestArgument32BitMapped {
        fn get_type() -> &'static Type {
            &*TEST_ARG_32_BIT_TYPE
        }
    }

    impl TestArgument32BitMapped {
        fn get_mapping() -> StructMapping {
            StructMapping {
                memory_id: TEST_ARG_32_BIT_MEMORY_ID.clone(),
                fields: vec![
                    FieldMapping {
                        source_offset: std::mem::offset_of!(TestArgument32, data),
                        target_offset: std::mem::offset_of!(TestArgument, data),
                    },
                    FieldMapping {
                        source_offset: std::mem::offset_of!(TestArgument32, data_end),
                        target_offset: std::mem::offset_of!(TestArgument, data_end),
                    },
                    FieldMapping {
                        source_offset: std::mem::offset_of!(TestArgument32, mutable_field),
                        target_offset: std::mem::offset_of!(TestArgument, mutable_field),
                    },
                ],
            }
        }
    }

    struct TestEbpfProgramContext {}

    impl EbpfProgramContext for TestEbpfProgramContext {
        type RunContext<'a> = ();

        type Packet<'a> = ();
        type Arg1<'a> = &'a mut TestArgument;
        type Arg2<'a> = ();
        type Arg3<'a> = ();
        type Arg4<'a> = ();
        type Arg5<'a> = ();
    }

    fn initialize_test_program(
        code: Vec<EbpfInstruction>,
    ) -> Result<EbpfProgram<TestEbpfProgramContext>, EbpfError> {
        let verified_program = verify_program(
            code,
            CallingContext { args: vec![TEST_ARG_TYPE.clone()], ..Default::default() },
            &mut NullVerifierLogger,
        )?;
        link_program(&verified_program, &[], &[], HashMap::default())
    }

    struct TestEbpfProgramContext32BitMapped {}

    impl EbpfProgramContext for TestEbpfProgramContext32BitMapped {
        type RunContext<'a> = ();

        type Packet<'a> = ();
        type Arg1<'a> = &'a TestArgument32BitMapped;
        type Arg2<'a> = ();
        type Arg3<'a> = ();
        type Arg4<'a> = ();
        type Arg5<'a> = ();
    }

    fn initialize_test_program_for_32bit_arg(
        code: Vec<EbpfInstruction>,
    ) -> Result<EbpfProgram<TestEbpfProgramContext32BitMapped>, EbpfError> {
        let verified_program = verify_program(
            code,
            CallingContext { args: vec![TEST_ARG_32_BIT_TYPE.clone()], ..Default::default() },
            &mut NullVerifierLogger,
        )?;
        link_program(
            &verified_program,
            &[TestArgument32BitMapped::get_mapping()],
            &[],
            HashMap::default(),
        )
    }

    #[test]
    fn test_data_end() {
        let program = r#"
        mov %r0, 0
        ldxdw %r2, [%r1+16]
        ldxdw %r1, [%r1+8]
        # ensure data contains at least 8 bytes
        mov %r3, %r1
        add %r3, 0x8
        jgt %r3, %r2, +1
        # read 8 bytes from data
        ldxdw %r0, [%r1]
        exit
        "#;
        let program = initialize_test_program(parse_asm(program)).expect("load");

        let v = [42];
        let mut data = TestArgument::from_data(&v[..]);
        assert_eq!(program.run_with_1_argument(&mut (), &mut data), v[0]);
    }

    #[test]
    fn test_past_data_end() {
        let program = r#"
        mov %r0, 0
        ldxdw %r2, [%r1+16]
        ldxdw %r1, [%r1+6]
        # ensure data contains at least 4 bytes
        mov %r3, %r1
        add %r3, 0x4
        jgt %r3, %r2, +1
        # read 8 bytes from data
        ldxdw %r0, [%r1]
        exit
        "#;
        initialize_test_program(parse_asm(program)).expect_err("incorrect program");
    }

    #[test]
    fn test_mapping() {
        let program = r#"
          # Return `TestArgument32.mutable_field`
          ldxw %r0, [%r1+12]
          exit
        "#;
        let program = initialize_test_program_for_32bit_arg(parse_asm(program)).expect("load");

        let mut data = TestArgument32BitMapped(TestArgument::default());
        assert_eq!(program.run_with_1_argument(&mut (), &mut data), data.0.mutable_field as u64);
    }

    #[test]
    fn test_mapping_partial_load() {
        // Verify that we can access middle of a remapped scalar field.
        let program = r#"
          # Returns two upper bytes of `TestArgument32.mutable_filed`
          ldxh %r0, [%r1+14]
          exit
        "#;
        let program = initialize_test_program_for_32bit_arg(parse_asm(program)).expect("load");

        let mut data = TestArgument32BitMapped(TestArgument::default());
        data.0.mutable_field = 0x12345678;
        assert_eq!(program.run_with_1_argument(&mut (), &mut data), 0x1234 as u64);
    }

    #[test]
    fn test_mapping_ptr() {
        let program = r#"
        mov %r0, 0
        # Load data and data_end as 32 bits pointers in TestArgument32
        ldxw %r2, [%r1+8]
        ldxw %r1, [%r1+4]
        # ensure data contains at least 8 bytes
        mov %r3, %r1
        add %r3, 0x8
        jgt %r3, %r2, +1
        # read 8 bytes from data
        ldxdw %r0, [%r1]
        exit
        "#;
        let program = initialize_test_program_for_32bit_arg(parse_asm(program)).expect("load");

        let v = [42];
        let mut data = TestArgument32BitMapped(TestArgument::from_data(&v[..]));
        assert_eq!(program.run_with_1_argument(&mut (), &mut data), v[0]);
    }

    #[test]
    fn test_mapping_with_offset() {
        let program = r#"
        mov %r0, 0
        add %r1, 0x8
        # Load data and data_end as 32 bits pointers in TestArgument32
        ldxw %r2, [%r1]
        ldxw %r1, [%r1-4]
        # ensure data contains at least 8 bytes
        mov %r3, %r1
        add %r3, 0x8
        jgt %r3, %r2, +1
        # read 8 bytes from data
        ldxdw %r0, [%r1]
        exit
        "#;
        let program = initialize_test_program_for_32bit_arg(parse_asm(program)).expect("load");

        let v = [42];
        let mut data = TestArgument32BitMapped(TestArgument::from_data(&v[..]));
        assert_eq!(program.run_with_1_argument(&mut (), &mut data), v[0]);
    }

    #[test]
    fn test_ptr_diff() {
        let program = r#"
          mov %r0, %r1
          add %r0, 0x2
          # Substract 2 ptr to memory
          sub %r0, %r1

          mov %r2, %r10
          add %r2, 0x3
          # Substract 2 ptr to stack
          sub %r2, %r10
          add %r0, %r2

          ldxdw %r2, [%r1+16]
          ldxdw %r1, [%r1+8]
          # Substract ptr to array and ptr to array end
          sub %r2, %r1
          add %r0, %r2

          mov %r2, %r1
          add %r2, 0x4
          # Substract 2 ptr to array
          sub %r2, %r1
          add %r0, %r2

          exit
        "#;
        let code = parse_asm(program);

        let program = initialize_test_program(code).expect("load");

        let v = [42];
        let mut data = TestArgument::from_data(&v[..]);
        assert_eq!(program.run_with_1_argument(&mut (), &mut data), 17);
    }

    #[test]
    fn test_invalid_packet_load() {
        let program = r#"
        mov %r6, %r2
        mov %r0, 0
        ldpw
        exit
        "#;
        let args = vec![
            Type::PtrToMemory { id: MemoryId::new(), offset: 0, buffer_size: 16 },
            Type::PtrToMemory { id: MemoryId::new(), offset: 0, buffer_size: 16 },
        ];
        let verify_result = verify_program(
            parse_asm(program),
            CallingContext { args, ..Default::default() },
            &mut NullVerifierLogger,
        );

        assert_eq!(
            verify_result.expect_err("validation should fail"),
            EbpfError::ProgramVerifyError("R6 is not a packet at pc 2".to_string())
        );
    }

    #[test]
    fn test_invalid_field_size() {
        // Load with a field size too large fails validation.
        let program = r#"
          ldxdw %r0, [%r1]
          exit
        "#;
        initialize_test_program(parse_asm(program)).expect_err("incorrect program");
    }

    #[test]
    fn test_unknown_field() {
        // Load outside of the know fields fails validation.
        let program = r#"
          ldxw %r0, [%r1 + 4]
          exit
        "#;
        initialize_test_program(parse_asm(program)).expect_err("incorrect program");
    }

    #[test]
    fn test_partial_ptr_field() {
        // Partial loads of ptr fields are not allowed.
        let program = r#"
          ldxw %r0, [%r1 + 8]
          exit
        "#;
        initialize_test_program(parse_asm(program)).expect_err("incorrect program");
    }

    #[test]
    fn test_readonly_field() {
        // Store to a read only field fails validation.
        let program = r#"
          stw [%r1], 0x42
          exit
        "#;
        initialize_test_program(parse_asm(program)).expect_err("incorrect program");
    }

    #[test]
    fn test_store_mutable_field() {
        // Store to a mutable field is allowed.
        let program = r#"
          stw [%r1 + 24], 0x42
          mov %r0, 1
          exit
        "#;
        let program = initialize_test_program(parse_asm(program)).expect("load");

        let mut data = TestArgument::default();
        assert_eq!(program.run_with_1_argument(&mut (), &mut data), 1);
        assert_eq!(data.mutable_field, 0x42);
    }

    #[test]
    fn test_fake_array_bounds_check() {
        // Verify that negative offsets in memory ptrs are handled properly and cannot be used to
        // bypass array bounds checks.
        let program = r#"
        mov %r0, 0
        ldxdw %r2, [%r1+16]
        ldxdw %r1, [%r1+8]
        # Subtract 8 from `data` and pretend checking array bounds.
        mov %r3, %r1
        sub %r3, 0x8
        jgt %r3, %r2, +1
        # Read 8 bytes from `data`. This should be rejected by the verifier.
        ldxdw %r0, [%r1]
        exit
        "#;
        initialize_test_program(parse_asm(program)).expect_err("incorrect program");
    }
}
