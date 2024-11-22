// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::converter::cbpf_to_ebpf;
use crate::executor::execute;
use crate::verifier::{
    verify, CallingContext, FunctionSignature, NullVerifierLogger, Type, VerifiedEbpfProgram,
    VerifierLogger,
};
use crate::{
    DataWidth, EbpfError, EbpfInstruction, MapSchema, MemoryId, StructAccess, BPF_DW, BPF_LDDW,
    BPF_PSEUDO_MAP_IDX, BPF_SIZE_MASK,
};
use derivative::Derivative;
use linux_uapi::sock_filter;
use std::collections::HashMap;
use std::fmt::Formatter;
use std::sync::Arc;
use zerocopy::{FromBytes, Immutable, IntoBytes};

/// A counter that allows to generate new ids for parameters. The namespace is the same as for id
/// generated for types while verifying an ebpf program, but it is started a u64::MAX / 2 and so is
/// guaranteed to never collide because the number of instruction of an ebpf program are bounded.
static BPF_TYPE_IDENTIFIER_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX / 2);

pub fn new_bpf_type_identifier() -> MemoryId {
    BPF_TYPE_IDENTIFIER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed).into()
}

pub trait EbpfRunContext {
    type Context<'a>;
}

impl EbpfRunContext for () {
    type Context<'a> = ();
}

#[derive(Clone, Copy, Debug)]
pub struct BpfValue(u64);

static_assertions::const_assert_eq!(
    std::mem::size_of::<BpfValue>(),
    std::mem::size_of::<*const u8>()
);

impl Default for BpfValue {
    fn default() -> Self {
        Self::from(0)
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

pub trait PacketAccessor<C: EbpfRunContext> {
    fn load<'a>(
        &self,
        context: &mut C::Context<'a>,
        packet_ptr: BpfValue,
        offset: i32,
        width: DataWidth,
    ) -> Option<BpfValue>;
    fn packet_len<'a>(&self, context: &mut C::Context<'a>, packet_ptr: BpfValue) -> usize;
}

impl<'a, C: EbpfRunContext> std::fmt::Debug for &'a dyn PacketAccessor<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("PacketAccessor").finish()
    }
}

/// A `PacketAccessor` that reads the data from a value of type `T`.
#[derive(Default)]
pub struct DirectPacketAccessor<T: IntoBytes + Immutable> {
    _phantom: std::marker::PhantomData<T>,
}

impl<C: EbpfRunContext, T: IntoBytes + Immutable> PacketAccessor<C> for DirectPacketAccessor<T> {
    fn load<'a>(
        &self,
        _context: &mut C::Context<'a>,
        packet_ptr: BpfValue,
        offset: i32,
        width: DataWidth,
    ) -> Option<BpfValue> {
        let data = unsafe { packet_ptr.as_ptr::<T>().as_ref()? }.as_bytes();
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

    fn packet_len<'a>(&self, _context: &mut C::Context<'a>, _packet_ptr: BpfValue) -> usize {
        std::mem::size_of::<T>()
    }
}

/// A `PacketAccessor` for the case when the is no packet.
#[derive(Default)]
pub struct EmptyPacketAccessor {}

impl<C: EbpfRunContext> PacketAccessor<C> for EmptyPacketAccessor {
    fn load<'a>(
        &self,
        _context: &mut C::Context<'a>,
        _packet_ptr: BpfValue,
        _offset: i32,
        _width: DataWidth,
    ) -> Option<BpfValue> {
        None
    }
    fn packet_len<'a>(&self, _context: &mut C::Context<'a>, _packet_ptr: BpfValue) -> usize {
        0
    }
}

pub struct EbpfHelper<C: EbpfRunContext> {
    pub index: u32,
    pub name: &'static str,
    pub function_pointer: Arc<
        dyn Fn(&mut C::Context<'_>, BpfValue, BpfValue, BpfValue, BpfValue, BpfValue) -> BpfValue
            + Send
            + Sync,
    >,
    pub signature: FunctionSignature,
}

impl<C: EbpfRunContext> Clone for EbpfHelper<C> {
    fn clone(&self) -> Self {
        Self {
            index: self.index,
            name: self.name,
            function_pointer: Arc::clone(&self.function_pointer),
            signature: self.signature.clone(),
        }
    }
}

impl<C: EbpfRunContext> std::fmt::Debug for EbpfHelper<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("EbpfHelper")
            .field("index", &self.index)
            .field("name", &self.name)
            .field("signature", &self.signature)
            .finish()
    }
}

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

pub struct MapDescriptor {
    pub schema: MapSchema,
    pub ptr: BpfValue,
}

/// Rewrites the code to ensure mapped fields are correctly handled. Returns
/// runnable `EbpfProgram<C>`.
fn link_program<C: EbpfRunContext>(
    program: &VerifiedEbpfProgram,
    struct_mappings: &[StructMapping],
    maps: &Vec<MapDescriptor>,
    helpers: HashMap<u32, EbpfHelper<C>>,
) -> Result<EbpfProgram<C>, EbpfError> {
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

    // Link maps.
    for pc in 0..code.len() {
        let instruction = &mut code[pc];
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

    Ok(EbpfProgram { code, helpers })
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct EbpfProgramBuilder<C: EbpfRunContext> {
    calling_context: CallingContext,
    struct_mappings: Vec<StructMapping>,
    maps: Vec<MapDescriptor>,
    helpers: HashMap<u32, EbpfHelper<C>>,
}

impl<C: EbpfRunContext> std::fmt::Debug for EbpfProgramBuilder<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("EbpfProgramBuilder")
            .field("helpers", &self.helpers)
            .field("calling_context", &self.calling_context)
            .finish()
    }
}

impl<C: EbpfRunContext> EbpfProgramBuilder<C> {
    // Adds a new map to the calling context. Returns the index that should be used to reference the map.
    pub fn register_map(&mut self, desc: MapDescriptor) -> usize {
        let index = self.calling_context.register_map(desc.schema);
        assert_eq!(self.maps.len(), index);
        self.maps.push(desc);
        index
    }

    pub fn set_args(&mut self, args: &[Type]) {
        self.calling_context.set_args(args);
    }

    pub fn set_packet_memory_id(&mut self, packet_memory_id: MemoryId) {
        self.calling_context.set_packet_memory_id(packet_memory_id);
    }

    pub fn register_struct_mapping(&mut self, mapping: StructMapping) {
        self.struct_mappings.push(mapping);
    }

    // This function signature will need more parameters eventually. The client needs to be able to
    // supply a real callback and it's type. The callback will be needed to actually call the
    // callback. The type will be needed for the verifier.
    pub fn register_helper(&mut self, helper: EbpfHelper<C>) -> Result<(), EbpfError> {
        self.calling_context.register_function(helper.index, helper.signature.clone());
        self.helpers.insert(helper.index, helper);
        Ok(())
    }

    pub fn load(
        self,
        code: Vec<EbpfInstruction>,
        logger: &mut dyn VerifierLogger,
    ) -> Result<EbpfProgram<C>, EbpfError> {
        let Self { calling_context, struct_mappings, maps, helpers } = self;
        let verified_program = verify(code, calling_context, logger)?;
        link_program(&verified_program, &struct_mappings, &maps, helpers)
    }
}

/// An abstraction over an eBPF program and its registered helper functions.
#[derive(Debug)]
pub struct EbpfProgram<C: EbpfRunContext> {
    pub code: Vec<EbpfInstruction>,
    pub helpers: HashMap<u32, EbpfHelper<C>>,
}

impl<C: EbpfRunContext> EbpfProgram<C> {
    /// Executes the current program on the specified `data`.
    /// The program receives a pointer to the `data` and the size of the packet (provided by the
    /// `PacketAccessor`) as the first two arguments.
    ///
    /// Warning: If this program was a cBPF program then the `data` must be the
    /// packet. It's passed to the `PacketAccessor` as the packet. `packet_size`
    /// specified the value loaded by `BPF_LD | BPF_LEN`.
    ///
    /// TODO(https://fxbug.dev/376284982) Currently there is nothing to ensure that the type of
    /// the argument matches the type specified when the program was verified and that it matches
    /// the type of the packet expected by the `packet_accessor`.
    pub fn run<T>(
        &self,
        run_context: &mut C::Context<'_>,
        packet_accessor: &dyn PacketAccessor<C>,
        data: &mut T,
    ) -> u64 {
        let packet_ptr: BpfValue = (data as *mut T).into();
        let packet_len = packet_accessor.packet_len(run_context, packet_ptr);
        self.run_with_arguments(run_context, packet_accessor, &[packet_ptr, packet_len.into()])
    }

    /// Executes the current program on the provided data.
    pub fn run_with_slice(
        &self,
        run_context: &mut C::Context<'_>,
        packet_accessor: &dyn PacketAccessor<C>,
        data: &mut [u8],
    ) -> u64 {
        self.run_with_arguments(
            run_context,
            packet_accessor,
            &[data.as_mut_ptr().into(), data.len().into()],
        )
    }

    pub fn run_with_arguments(
        &self,
        run_context: &mut C::Context<'_>,
        packet_accessor: &dyn PacketAccessor<C>,
        arguments: &[BpfValue],
    ) -> u64 {
        execute(self, run_context, packet_accessor, arguments)
    }
}

impl EbpfProgram<()> {
    pub fn from_verified_code(code: Vec<EbpfInstruction>) -> Self {
        Self { code, helpers: Default::default() }
    }

    pub fn code(&self) -> &[EbpfInstruction] {
        &self.code[..]
    }

    /// This method instantiates an EbpfProgram given a cbpf original.
    pub fn from_cbpf(bpf_code: &[sock_filter]) -> Result<Self, EbpfError> {
        let mut builder = EbpfProgramBuilder::default();
        let packet_memory_id = new_bpf_type_identifier();
        builder.set_args(&[
            // Pointer to the packet (e.g. `__sk_buff`). `buffer_size` is set to 0 because the
            // packet is supposed to be access only through the `PacketAccessor` which validates
            // the offset in runtime.
            Type::PtrToMemory { id: packet_memory_id.clone(), offset: 0, buffer_size: 0 },
            // Packet size (see `BPF_LEN` in cBPF). This may be different from the size of the
            // value pointed by the first argument.
            Type::ScalarValueParameter,
        ]);
        builder.set_packet_memory_id(packet_memory_id);
        builder.load(cbpf_to_ebpf(bpf_code)?, &mut NullVerifierLogger)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::conformance::test::parse_asm;
    use crate::{FieldDescriptor, FieldMapping, FieldType, NullVerifierLogger, StructDescriptor};
    use linux_uapi::*;
    use std::sync::LazyLock;
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

    fn with_prg_assert_result(
        prg: &EbpfProgram<()>,
        mut data: seccomp_data,
        result: u32,
        msg: &str,
    ) {
        let return_value =
            prg.run(&mut (), &DirectPacketAccessor::<seccomp_data>::default(), &mut data);
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

        let prg = EbpfProgram::<()>::from_cbpf(&test_prg).expect("Error parsing program");

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

            let prg = EbpfProgram::<()>::from_cbpf(&test_prg).expect("Error parsing program");

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

            let prg = EbpfProgram::<()>::from_cbpf(&test_prg).expect("Error parsing program");

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

        let prg = EbpfProgram::<()>::from_cbpf(&test_prg).expect("Error parsing program");

        for i in [0x00, 0x01, 0x07, 0x15, 0xff].iter() {
            with_prg_assert_result(
                &prg,
                seccomp_data { nr: *i, ..Default::default() },
                4 * (*i & 0xf) as u32,
                "BPF math does not work",
            )
        }
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone, IntoBytes, Immutable, KnownLayout, FromBytes)]
    struct ProgramArgument {
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

    // A version of ProgramArgument with 32-bit remapped pointers.
    struct ProgramArgument32 {
        pub read_only_field: u32,
        pub data: u32,
        pub data_end: u32,
        pub mutable_field: u32,
    }

    impl Default for ProgramArgument {
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

    static ARG_MEMORY_ID: LazyLock<MemoryId> = LazyLock::new(|| new_bpf_type_identifier());

    impl ProgramArgument {
        fn get_type() -> Type {
            let array_id = new_bpf_type_identifier();

            let descriptor = Arc::new(StructDescriptor {
                fields: vec![
                    FieldDescriptor {
                        offset: std::mem::offset_of!(ProgramArgument, read_only_field),
                        field_type: FieldType::Scalar { size: 4 },
                    },
                    FieldDescriptor {
                        offset: std::mem::offset_of!(ProgramArgument, data),
                        field_type: FieldType::PtrToArray {
                            is_32_bit: false,
                            id: array_id.clone(),
                        },
                    },
                    FieldDescriptor {
                        offset: std::mem::offset_of!(ProgramArgument, data_end),
                        field_type: FieldType::PtrToEndArray { is_32_bit: false, id: array_id },
                    },
                    FieldDescriptor {
                        offset: std::mem::offset_of!(ProgramArgument, mutable_field),
                        field_type: FieldType::MutableScalar { size: 4 },
                    },
                ],
            });

            Type::PtrToStruct { id: (*ARG_MEMORY_ID).clone(), offset: 0, descriptor }
        }

        // Returns type def for a program that takes `ProgramArgument32`, but remaps access to `ProgramArgument`.
        fn get_type_32bit() -> Type {
            let array_id = new_bpf_type_identifier();

            let descriptor = Arc::new(StructDescriptor {
                fields: vec![
                    FieldDescriptor {
                        offset: std::mem::offset_of!(ProgramArgument32, read_only_field),
                        field_type: FieldType::Scalar { size: 4 },
                    },
                    FieldDescriptor {
                        offset: std::mem::offset_of!(ProgramArgument32, data),
                        field_type: FieldType::PtrToArray { is_32_bit: true, id: array_id.clone() },
                    },
                    FieldDescriptor {
                        offset: std::mem::offset_of!(ProgramArgument32, data_end),
                        field_type: FieldType::PtrToEndArray { is_32_bit: true, id: array_id },
                    },
                    FieldDescriptor {
                        offset: std::mem::offset_of!(ProgramArgument32, mutable_field),
                        field_type: FieldType::MutableScalar { size: 4 },
                    },
                ],
            });

            Type::PtrToStruct { id: (*ARG_MEMORY_ID).clone(), offset: 0, descriptor }
        }

        fn get_32bit_mapping() -> StructMapping {
            StructMapping {
                memory_id: ARG_MEMORY_ID.clone(),
                fields: vec![
                    FieldMapping {
                        source_offset: std::mem::offset_of!(ProgramArgument32, data),
                        target_offset: std::mem::offset_of!(ProgramArgument, data),
                    },
                    FieldMapping {
                        source_offset: std::mem::offset_of!(ProgramArgument32, data_end),
                        target_offset: std::mem::offset_of!(ProgramArgument, data_end),
                    },
                    FieldMapping {
                        source_offset: std::mem::offset_of!(ProgramArgument32, mutable_field),
                        target_offset: std::mem::offset_of!(ProgramArgument, mutable_field),
                    },
                ],
            }
        }

        fn from_data(data: &[u64]) -> Self {
            let ptr_range = data.as_ptr_range();
            Self {
                data: ptr_range.start as u64,
                data_end: ptr_range.end as u64,
                ..Default::default()
            }
        }
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
        let code = parse_asm(program);

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let v = [42];
        let mut data = ProgramArgument::from_data(&v[..]);
        assert_eq!(program.run(&mut (), &EmptyPacketAccessor::default(), &mut data), v[0]);
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
        let code = parse_asm(program);

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        builder.load(code, &mut NullVerifierLogger).expect_err("incorrect program");
    }

    #[test]
    fn test_mapping() {
        let program = r#"
          # Return `ProgramArgument32.mutable_field`
          ldxw %r0, [%r1+12]
          exit
        "#;
        let code = parse_asm(program);
        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type_32bit()]);
        builder.register_struct_mapping(ProgramArgument::get_32bit_mapping());
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let mut data = ProgramArgument::default();
        assert_eq!(
            program.run(&mut (), &EmptyPacketAccessor::default(), &mut data),
            data.mutable_field as u64
        );
    }

    #[test]
    fn test_mapping_partial_load() {
        // Verify that we can access middle of a remapped scalar field.
        let program = r#"
          # Returns two upper bytes of `ProgramArgument32.mutable_filed`
          ldxh %r0, [%r1+14]
          exit
        "#;
        let code = parse_asm(program);
        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type_32bit()]);
        builder.register_struct_mapping(ProgramArgument::get_32bit_mapping());
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let mut data = ProgramArgument::default();
        data.mutable_field = 0x12345678;
        assert_eq!(program.run(&mut (), &EmptyPacketAccessor::default(), &mut data), 0x1234 as u64);
    }

    #[test]
    fn test_mapping_ptr() {
        let program = r#"
        mov %r0, 0
        # Load data and data_end as 32 bits pointers in ProgramArgument32
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
        let code = parse_asm(program);

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type_32bit()]);
        builder.register_struct_mapping(ProgramArgument::get_32bit_mapping());
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let v = [42];
        let mut data = ProgramArgument::from_data(&v[..]);
        assert_eq!(program.run(&mut (), &EmptyPacketAccessor::default(), &mut data), v[0]);
    }

    #[test]
    fn test_mapping_with_offset() {
        let program = r#"
        mov %r0, 0
        add %r1, 0x8
        # Load data and data_end as 32 bits pointers in ProgramArgument32
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
        let code = parse_asm(program);

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type_32bit()]);
        builder.register_struct_mapping(ProgramArgument::get_32bit_mapping());
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let v = [42];
        let mut data = ProgramArgument::from_data(&v[..]);
        assert_eq!(program.run(&mut (), &EmptyPacketAccessor::default(), &mut data), v[0]);
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

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let v = [42];
        let mut data = ProgramArgument::from_data(&v[..]);
        assert_eq!(program.run(&mut (), &EmptyPacketAccessor::default(), &mut data), 17);
    }

    #[test]
    fn test_invalid_packet_load() {
        let program = r#"
        mov %r6, %r2
        mov %r0, 0
        ldpw
        exit
        "#;
        let code = parse_asm(program);

        let mut builder = EbpfProgramBuilder::<()>::default();

        let packet_memory_id = new_bpf_type_identifier();
        builder.set_packet_memory_id(packet_memory_id.clone());
        let second_memory_id = new_bpf_type_identifier();
        builder.set_args(&[
            Type::PtrToMemory { id: packet_memory_id, offset: 0, buffer_size: 16 },
            Type::PtrToMemory { id: second_memory_id, offset: 0, buffer_size: 16 },
        ]);

        assert_eq!(
            builder.load(code, &mut NullVerifierLogger).expect_err("validation should fail"),
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
        let code = parse_asm(program);
        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        builder.load(code, &mut NullVerifierLogger).expect_err("incorrect program");
    }

    #[test]
    fn test_unknown_field() {
        // Load outside of the know fields fails validation.
        let program = r#"
          ldxw %r0, [%r1 + 4]
          exit
        "#;
        let code = parse_asm(program);
        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        builder.load(code, &mut NullVerifierLogger).expect_err("incorrect program");
    }

    #[test]
    fn test_partial_ptr_field() {
        // Partial loads of ptr fields are not allowed.
        let program = r#"
          ldxw %r0, [%r1 + 8]
          exit
        "#;
        let code = parse_asm(program);
        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        builder.load(code, &mut NullVerifierLogger).expect_err("incorrect program");
    }

    #[test]
    fn test_readonly_field() {
        // Store to a read only field fails validation.
        let program = r#"
          stw [%r1], 0x42
          exit
        "#;
        let code = parse_asm(program);
        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        builder.load(code, &mut NullVerifierLogger).expect_err("incorrect program");
    }

    #[test]
    fn test_store_mutable_field() {
        // Store to a mutable field is allowed.
        let program = r#"
          stw [%r1 + 24], 0x42
          mov %r0, 1
          exit
        "#;
        let code = parse_asm(program);
        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        let program = builder.load(code, &mut NullVerifierLogger).expect("load");

        let mut data = ProgramArgument::default();
        assert_eq!(program.run(&mut (), &EmptyPacketAccessor::default(), &mut data), 1);
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
        let code = parse_asm(program);

        let mut builder = EbpfProgramBuilder::<()>::default();
        builder.set_args(&[ProgramArgument::get_type()]);
        builder.load(code, &mut NullVerifierLogger).expect_err("incorrect program");
    }
}
