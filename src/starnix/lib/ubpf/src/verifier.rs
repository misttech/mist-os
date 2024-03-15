// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    visitor::{BpfVisitor, DataWidth, Register, Source},
    MapSchema, UbpfError,
};
use byteorder::{BigEndian, ByteOrder, LittleEndian, NativeEndian};
use linux_uapi::bpf_insn;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use zerocopy::AsBytes;

pub type ProgramCounter = usize;

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

// The ubpf limits
const UBPF_STACK_SIZE: usize = crate::ubpf::UBPF_STACK_SIZE as usize;
// The number of registers
const REGISTER_COUNT: u8 = 11;
// The number of general r/w registers.
const GENERAL_REGISTER_COUNT: u8 = 10;

#[derive(Clone, Debug)]
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
    PtrToMemory { id: u64, offset: u64, buffer_size: u64 },
    /// A pointer that might be null and must be validated before being referenced.
    NullOr(Box<Type>),
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
}

const NotInit: Type = Type::ScalarValue {
    value: 0,
    unknown_mask: u64::max_value(),
    unwritten_mask: u64::max_value(),
};

impl From<u64> for Type {
    fn from(value: u64) -> Self {
        Self::ScalarValue { value, unknown_mask: 0, unwritten_mask: 0 }
    }
}

impl Default for Type {
    /// A new instance of `Type` where no bit has been written yet.
    fn default() -> Self {
        NotInit.clone()
    }
}

impl Type {
    /// A new instance of `Type` where the data is usable by userspace, but the value is unknown
    /// for the verifier.
    pub const fn unknown_written_scalar_value() -> Self {
        Self::ScalarValue { value: 0, unknown_mask: u64::max_value(), unwritten_mask: 0 }
    }

    /// The mask associated with a data of size `width`.
    fn mask(width: DataWidth) -> u64 {
        if width == DataWidth::U64 {
            u64::max_value()
        } else {
            (1 << width.bits()) - 1
        }
    }

    /// Given the given conditional jump `jump_type` having been tasken, constraint the type of
    /// `type1` and `type2`.
    fn constraint(jump_type: JumpType, type1: Self, type2: Self) -> (Self, Self) {
        match (jump_type, &type1, &type2) {
            (
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
                JumpType::Eq,
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
                Self::NullOr(_),
            )
            | (
                JumpType::Eq,
                Self::NullOr(_),
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
            ) => {
                let zero = Type::from(0);
                (zero.clone(), zero)
            }
            (JumpType::Eq, _, _) => (type1.clone(), type1),
            (
                JumpType::Ne | JumpType::StrictComparaison,
                Self::NullOr(t),
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
            ) => (*t.clone(), type2),
            (
                JumpType::Ne | JumpType::StrictComparaison,
                Self::ScalarValue { value: 0, unknown_mask: 0, .. },
                Self::NullOr(t),
            ) => (type1, *t.clone()),
            _ => (type1, type2),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FunctionSignature {
    pub args: &'static [Type],
    pub return_value: Type,
}

#[derive(Debug, Default)]
pub struct CallingContext {
    /// For all pc that represents the load of a map address, keep track of the schema of the
    /// associated map.
    map_references: HashMap<ProgramCounter, MapSchema>,
    /// The registered external functions.
    functions: HashMap<u32, FunctionSignature>,
    /// The args of the program.
    args: Vec<Type>,
}

impl CallingContext {
    pub fn register_map_reference(&mut self, pc: ProgramCounter, schema: MapSchema) {
        self.map_references.insert(pc, schema);
    }
    pub fn register_function(&mut self, index: u32, signature: FunctionSignature) {
        self.functions.insert(index, signature);
    }
    pub fn set_args(&mut self, args: &[Type]) {
        assert!(args.len() <= 5);
        self.args = args.to_vec();
    }
}

/// Verify the given code depending on the type of the parameters and the registered external
/// functions.
pub fn verify(
    code: &[bpf_insn],
    calling_context: CallingContext,
    logger: &mut dyn VerifierLogger,
) -> Result<(), UbpfError> {
    let mut context = ComputationContext::default();
    for (i, t) in calling_context.args.iter().enumerate() {
        // The parameter registers are r1 to r5.
        context.set_reg((i + 1) as u8, t.clone()).map_err(UbpfError::ProgramLoadError)?;
    }
    let states = vec![context];
    let mut verification_context =
        VerificationContext { calling_context, logger, states, code, counter: 0, iteration: 0 };
    while let Some(mut context) = verification_context.states.pop() {
        if verification_context.iteration > 10 * (crate::ubpf::UBPF_MAX_INSTS as usize) {
            return error_and_log(&mut verification_context, "bpf byte code does not terminate");
        }
        if context.pc >= code.len() {
            return error_and_log(&mut verification_context, "pc out of bounds");
        }
        let visit_result = context.visit(&mut verification_context, &code[context.pc..]);
        match visit_result {
            Err(message) => {
                return error_and_log(&mut verification_context, message);
            }
            _ => {}
        }
        verification_context.iteration += 1;
    }
    Ok(())
}

struct VerificationContext<'a> {
    /// The type information for the program arguments and the registered functions.
    calling_context: CallingContext,
    /// The logger to use.
    logger: &'a mut dyn VerifierLogger,
    /// The `ComputationContext` yet to be validated.
    states: Vec<ComputationContext>,
    /// The program being analyzed.
    code: &'a [bpf_insn],
    /// A counter used to generated unique ids for memory buffers and maps.
    counter: u64,
    /// The current iteration of the verifier. Used to ensure termination by limiting the number of
    /// iteration before bailing out.
    iteration: usize,
}

impl<'a> VerificationContext<'a> {
    fn next_id(&mut self) -> u64 {
        let id = self.counter;
        self.counter += 1;
        id
    }

    fn log(&mut self, line: &[u8]) {
        self.logger.log(line);
    }
}

/// An offset inside the stack. The offset is from the end of the stack.
/// downward.
#[derive(Clone, Copy, Debug)]
pub struct StackOffset(u64);

impl Default for StackOffset {
    fn default() -> Self {
        Self(UBPF_STACK_SIZE as u64)
    }
}

impl StackOffset {
    /// The value of the register.
    fn reg(&self) -> u64 {
        self.0
    }

    /// The offset into the equivalent byte array.
    fn offset(&self) -> usize {
        self.0 as usize
    }

    /// The index into the stack array this offset points to.
    fn array_index(&self) -> usize {
        self.offset() / std::mem::size_of::<u64>()
    }

    /// The offset inside the aligned u64 in the stack.
    fn sub_index(&self) -> usize {
        self.offset() % std::mem::size_of::<u64>()
    }
}

impl std::ops::Add<u64> for StackOffset {
    type Output = Self;

    fn add(self, rhs: u64) -> Self {
        Self(self.reg().overflowing_add(rhs).0)
    }
}

impl std::ops::AddAssign<u64> for StackOffset {
    fn add_assign(&mut self, rhs: u64) {
        self.0 = self.reg().overflowing_add(rhs).0
    }
}

impl std::ops::Sub<u64> for StackOffset {
    type Output = Self;

    fn sub(self, rhs: u64) -> Self {
        Self(self.reg().overflowing_sub(rhs).0)
    }
}

impl std::ops::SubAssign<u64> for StackOffset {
    fn sub_assign(&mut self, rhs: u64) {
        self.0 = self.reg().overflowing_sub(rhs).0
    }
}

/// The state of the stack
#[derive(Clone, Debug)]
struct Stack {
    data: Vec<Type>,
}

impl Default for Stack {
    fn default() -> Self {
        Self { data: vec![Type::default(); UBPF_STACK_SIZE / std::mem::size_of::<u64>()] }
    }
}

impl Stack {
    fn extract_sub_value(value: u64, offset: usize, byte_count: usize) -> u64 {
        NativeEndian::read_uint(&value.as_bytes()[offset..], byte_count)
    }

    fn insert_sub_value(mut original: u64, value: u64, width: DataWidth, offset: usize) -> u64 {
        let byte_count = width.bytes();
        let original_buf = original.as_bytes_mut();
        let value_buf = value.as_bytes();
        for i in 0..byte_count {
            original_buf[i + offset] = value_buf[i + offset];
        }
        original
    }

    fn can_read_data_ptr(&self, mut offset: StackOffset, bytes: u64) -> bool {
        let can_read = |t: &Type, start_offset: usize, mut end_offset: usize| -> bool {
            match t {
                Type::ScalarValue { unwritten_mask, .. } => {
                    if start_offset == 0 && end_offset == 0 {
                        return *unwritten_mask == 0;
                    }
                    if end_offset == 0 {
                        end_offset = std::mem::size_of::<u64>();
                    }
                    debug_assert!(end_offset > start_offset);
                    Self::extract_sub_value(
                        *unwritten_mask,
                        start_offset,
                        end_offset - start_offset,
                    ) == 0
                }
                _ => false,
            }
        };
        if bytes == 0 {
            return true;
        }
        let mut end_offset = offset + bytes;
        if (end_offset - 1).array_index() as usize >= self.data.len() {
            return false;
        }
        // Handle the case where all the data is contained in a single u64.
        if offset.array_index() == (end_offset - 1).array_index() {
            return can_read(
                &self.data[offset.array_index()],
                offset.sub_index(),
                end_offset.sub_index(),
            );
        }

        // Handle the first element, that might be partial
        if offset.sub_index() != 0 {
            if !can_read(
                &self.data[offset.array_index()],
                offset.sub_index(),
                std::mem::size_of::<u64>(),
            ) {
                return false;
            }
            offset += (std::mem::size_of::<u64>() - offset.sub_index()) as u64;
        }

        // Handle the last element, that might be partial
        if end_offset.sub_index() != 0 {
            if !can_read(&self.data[end_offset.array_index()], 0, end_offset.sub_index()) {
                return false;
            }
            end_offset -= end_offset.sub_index() as u64;
        }

        // Handle the any full type between beginning and end.
        for i in offset.array_index()..end_offset.array_index() {
            if !can_read(&self.data[i], 0, std::mem::size_of::<u64>()) {
                return false;
            }
        }

        true
    }

    fn store(&mut self, offset: StackOffset, value: Type, width: DataWidth) -> Result<(), String> {
        if offset.array_index() >= self.data.len() {
            return Err(format!("out of bound store: {offset:?}"));
        }
        if offset.sub_index() % width.bytes() != 0 {
            return Err(format!("misaligned access"));
        }

        let index = offset.array_index();
        if width == DataWidth::U64 {
            self.data[index] = value;
        } else {
            match value {
                Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                    let (old_value, old_unknown_mask, old_unwritten_mask) = match self.data[index] {
                        Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                            (value, unknown_mask, unwritten_mask)
                        }
                        _ => {
                            // The value in the stack is not a scalar. Let consider it an scalar
                            // value with no written bits.
                            let Type::ScalarValue { value, unknown_mask, unwritten_mask } = NotInit
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
                    self.data[index] = Type::ScalarValue { value, unknown_mask, unwritten_mask };
                }
                _ => {
                    return Err(format!("cannot store part of a non scalar value on the stack"));
                }
            }
        }
        Ok(())
    }

    fn load(&self, offset: StackOffset, width: DataWidth) -> Result<Type, String> {
        if offset.array_index() >= self.data.len() {
            return Err(format!("out of bound load"));
        }
        if offset.sub_index() % width.bytes() != 0 {
            return Err(format!("misaligned access"));
        }

        let index = offset.array_index();
        let loaded_type = &self.data[index];
        if width == DataWidth::U64 {
            Ok(loaded_type.clone())
        } else {
            match loaded_type {
                Type::ScalarValue { value, unknown_mask, unwritten_mask } => {
                    let sub_index = offset.sub_index();
                    let value = Self::extract_sub_value(*value, sub_index, width.bytes());
                    let unknown_mask =
                        Self::extract_sub_value(*unknown_mask, sub_index, width.bytes());
                    let unwritten_mask =
                        Self::extract_sub_value(*unwritten_mask, sub_index, width.bytes());
                    Ok(Type::ScalarValue { value, unknown_mask, unwritten_mask })
                }
                _ => Err(format!("incorrect load of {} bytes", width.bytes())),
            }
        }
    }
}

/// The state of the computation as known by the verifier at a given point in time.
#[derive(Clone, Debug, Default)]
struct ComputationContext {
    /// Register 0 to 9.
    registers: [Type; GENERAL_REGISTER_COUNT as usize],
    /// The state of the stack.
    stack: Stack,
    /// The program counter.
    pc: ProgramCounter,
}

macro_rules! bpf_log {
    ($context:ident, $verification_context:ident, $($msg:tt)*) => {
        let prefix = format!("{}: ({:02x})", $context.pc, $verification_context.code[$context.pc].code);
        let suffix = format!($($msg)*);
        $verification_context.log(format!("{prefix} {suffix}").as_bytes());
    }
}

impl ComputationContext {
    fn reg(&self, index: Register) -> Result<&Type, String> {
        static StackTop: Lazy<Type> =
            Lazy::new(|| Type::PtrToStack { offset: StackOffset::default() });

        if index >= REGISTER_COUNT {
            return Err(format!("R{index} is invalid"));
        }
        if index < GENERAL_REGISTER_COUNT {
            Ok(&self.registers[index as usize])
        } else {
            Ok(&StackTop)
        }
    }

    fn set_reg(&mut self, index: Register, reg_type: Type) -> Result<(), String> {
        if index >= REGISTER_COUNT {
            return Err(format!("R{index} is invalid"));
        }
        self.registers[index as usize] = reg_type;
        Ok(())
    }

    fn get_map_schema(&self, argument: u8) -> Result<&MapSchema, String> {
        match self.reg(argument + 1)? {
            Type::ConstPtrToMap { schema, .. } => Ok(schema),
            _ => Err(format!("No map found at argument {argument}")),
        }
    }

    fn next(&self) -> Result<Self, String> {
        self.jump_with_offset(0)
    }

    /// Returns a new `ComputationContext` where the pc has jump by `offset + 1`. In particular,
    /// the next instruction is reached with `jump_with_offset(0)`.
    fn jump_with_offset(&self, offset: i16) -> Result<Self, String> {
        let pc = self
            .pc
            .checked_add_signed((offset + 1).into())
            .ok_or_else(|| format!("jump outside of program at pc {}", self.pc))?;
        let mut result = self.clone();
        result.pc = pc;
        Ok(result)
    }

    fn check_memory_access(
        &self,
        dst_offset: u64,
        dst_buffer_size: u64,
        instruction_offset: u64,
        width: usize,
    ) -> Result<(), String> {
        let final_offset = dst_offset.overflowing_add(instruction_offset).0;
        if final_offset.checked_add(width as u64).ok_or_else(|| format!("out of bound access"))?
            > dst_buffer_size
        {
            return Err(format!("out of bound access"));
        }
        Ok(())
    }

    fn store_memory(
        &mut self,
        dst: &Type,
        value: Type,
        instruction_offset: u64,
        width: DataWidth,
    ) -> Result<(), String> {
        match dst {
            Type::PtrToStack { offset } => {
                self.stack.store(*offset + instruction_offset, value, width)?;
            }
            Type::PtrToMemory { offset, buffer_size, .. } => {
                self.check_memory_access(*offset, *buffer_size, instruction_offset, width.bytes())?;
                match value {
                    Type::ScalarValue { unwritten_mask: 0, .. } => {}
                    // Private data should not be leaked.
                    _ => return Err(format!("incorrect store")),
                }
            }
            _ => return Err(format!("incorrect store")),
        }
        Ok(())
    }

    fn load_memory(
        &self,
        addr: &Type,
        instruction_offset: u64,
        width: DataWidth,
    ) -> Result<Type, String> {
        Ok(match addr {
            Type::PtrToStack { offset } => self.stack.load(*offset + instruction_offset, width)?,
            Type::PtrToMemory { offset, buffer_size, .. } => {
                self.check_memory_access(*offset, *buffer_size, instruction_offset, width.bytes())?;
                Type::unknown_written_scalar_value()
            }
            _ => return Err(format!("incorrect load")),
        })
    }

    fn resolve_return_value(
        &self,
        verification_context: &mut VerificationContext<'_>,
        return_value: &Type,
    ) -> Result<Type, String> {
        match return_value {
            Type::NullOr(t) => {
                Ok(Type::NullOr(Box::new(self.resolve_return_value(verification_context, t)?)))
            }
            Type::MapValueParameter { map_ptr_index } => {
                let schema = self.get_map_schema(*map_ptr_index)?;
                let id = verification_context.next_id();
                Ok(Type::PtrToMemory { id, offset: 0, buffer_size: schema.value_size as u64 })
            }
            t => Ok(t.clone()),
        }
    }

    fn compute_source(&self, src: Source) -> Result<Type, String> {
        match src {
            Source::Reg(reg) => self.reg(reg).cloned(),
            Source::Value(v) => Ok(v.into()),
        }
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
                "{op_name} {} {}",
                display_register(dst),
                display_source(src)
            );
        }
        let op1 = self.reg(dst)?;
        let op2 = self.compute_source(src)?;
        let result: Type = match (alu_type, op1, op2) {
            (
                _,
                Type::ScalarValue { value: value1, unknown_mask: 0, .. },
                Type::ScalarValue { value: value2, unknown_mask: 0, .. },
            ) => op(*value1, value2).into(),
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
                let unknown_mask = *unknown_mask1 | unknown_mask2;
                let unwritten_mask = *unwritten_mask1 | unwritten_mask2;
                let value = op(*value1, value2) & !unknown_mask;
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
                let value = op(*value1, value2);
                let unknown_mask = op(*unknown_mask1, value2);
                let unwritten_mask = op(*unwritten_mask1, value2);
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
                let unknown_mask = unknown_mask1.overflowing_shr(value2 as u32).0;
                let unwritten_mask = unwritten_mask1.overflowing_shr(value2 as u32).0;
                let value = op(*value1, value2) & !unknown_mask;
                Type::ScalarValue { value, unknown_mask, unwritten_mask }
            }
            (
                AluType::PtrCompatible,
                Type::PtrToStack { offset: x },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) => Type::PtrToStack { offset: run_on_stack_offset(*x, |x| op(x, y)) },
            (
                AluType::PtrCompatible,
                Type::PtrToMemory { id, offset: x, buffer_size },
                Type::ScalarValue { value: y, unknown_mask: 0, .. },
            ) => {
                let offset = op(*x, y);
                Type::PtrToMemory { id: *id, offset, buffer_size: *buffer_size }
            }
            (
                _,
                Type::ScalarValue { unwritten_mask: 0, .. },
                Type::ScalarValue { unwritten_mask: 0, .. },
            ) => Type::unknown_written_scalar_value(),
            _ => Type::default(),
        };
        let mut next = self.next()?;
        next.set_reg(dst, result)?;
        verification_context.states.push(next);
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
                value: bit_op(*value),
                unknown_mask: bit_op(*unknown_mask),
                unwritten_mask: bit_op(*unwritten_mask),
            },
            _ => Type::default(),
        };
        let mut next = self.next()?;
        next.set_reg(dst, new_value)?;
        verification_context.states.push(next);
        Ok(())
    }

    fn conditional_jump(
        &mut self,
        op_name: &str,
        verification_context: &mut VerificationContext<'_>,
        dst: Register,
        src: Source,
        offset: i16,
        jump_type: JumpType,
        op: impl Fn(u64, u64) -> bool,
    ) -> Result<(), String> {
        bpf_log!(
            self,
            verification_context,
            "{op_name} {} {} {}",
            display_register(dst),
            display_source(src),
            if offset == 0 { format!("0") } else { print_offset(offset) },
        );
        let op1 = self.reg(dst)?;
        let op2 = self.compute_source(src.clone())?;
        let apply_constraints_and_register =
            |mut next: Self, jump_type: JumpType| -> Result<Self, String> {
                if jump_type != JumpType::Unknown {
                    let (new_op1, new_op2) = Type::constraint(jump_type, op1.clone(), op2.clone());
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
        let branch = {
            match (op1, &op2) {
                (
                    Type::ScalarValue { value: x, unknown_mask: 0, .. },
                    Type::ScalarValue { value: y, unknown_mask: 0, .. },
                ) => Some(op(*x, *y)),
                (
                    Type::ScalarValue { unwritten_mask: 0, .. },
                    Type::ScalarValue { unwritten_mask: 0, .. },
                )
                | (Type::ScalarValue { value: 0, unknown_mask: 0, .. }, Type::NullOr(_))
                | (Type::NullOr(_), Type::ScalarValue { value: 0, unknown_mask: 0, .. }) => None,
                (Type::PtrToStack { offset: x }, Type::PtrToStack { offset: y }) => {
                    Some(op(x.reg(), y.reg()))
                }
                (
                    Type::PtrToMemory { id: id1, offset: x, .. },
                    Type::PtrToMemory { id: id2, offset: y, .. },
                ) if *id1 == *id2 => Some(op(*x, *y)),
                _ => return Err(format!("non permitted comparaison")),
            }
        };
        if branch.unwrap_or(true) {
            // Do the jump
            verification_context
                .states
                .push(apply_constraints_and_register(self.jump_with_offset(offset)?, jump_type)?);
        }
        if !branch.unwrap_or(false) {
            // Skip the jump
            verification_context
                .states
                .push(apply_constraints_and_register(self.next()?, jump_type.invert())?);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AluType {
    Plain,
    Bitwise,
    Shift,
    Arsh,
    PtrCompatible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JumpType {
    Unknown,
    Eq,
    Ne,
    StrictComparaison,
    LooseComparaison,
}

impl JumpType {
    fn invert(&self) -> Self {
        match self {
            Self::Unknown => Self::Unknown,
            Self::Eq => Self::Ne,
            Self::Ne => Self::Eq,
            Self::StrictComparaison => Self::LooseComparaison,
            Self::LooseComparaison => Self::StrictComparaison,
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
        self.alu(Some("add"), context, dst, src, AluType::PtrCompatible, |x, y| {
            x.overflowing_add(y).0
        })
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
        self.alu(Some("sub"), context, dst, src, AluType::PtrCompatible, |x, y| {
            x.overflowing_sub(y).0
        })
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
        let Some(signature) = context.calling_context.functions.get(&index).cloned() else {
            return Err(format!("unknown external function {}", index));
        };
        debug_assert!(signature.args.len() <= 5);
        for (index, arg) in signature.args.iter().enumerate() {
            let index = (index + 1) as u8;
            match (arg, self.reg(index)?) {
                (Type::ScalarValueParameter, Type::ScalarValue { unwritten_mask: 0, .. })
                | (Type::ConstPtrToMapParameter, Type::ConstPtrToMap { .. }) => Ok(()),
                (
                    Type::MapKeyParameter { map_ptr_index },
                    Type::PtrToMemory { offset, buffer_size, .. },
                ) => {
                    let schema = self.get_map_schema(*map_ptr_index)?;
                    self.check_memory_access(*offset, *buffer_size, 0, schema.key_size as usize)
                }
                (Type::MapKeyParameter { map_ptr_index }, Type::PtrToStack { offset }) => {
                    let schema = self.get_map_schema(*map_ptr_index)?;
                    if !self.stack.can_read_data_ptr(*offset, schema.key_size as u64) {
                        Err(format!("cannot read key buffer from the stack"))
                    } else {
                        Ok(())
                    }
                }
                (
                    Type::MapValueParameter { map_ptr_index },
                    Type::PtrToMemory { offset, buffer_size, .. },
                ) => {
                    let schema = self.get_map_schema(*map_ptr_index)?;
                    self.check_memory_access(*offset, *buffer_size, 0, schema.value_size as usize)
                }
                (Type::MapValueParameter { map_ptr_index }, Type::PtrToStack { offset }) => {
                    let schema = self.get_map_schema(*map_ptr_index)?;
                    if !self.stack.can_read_data_ptr(*offset, schema.value_size as u64) {
                        Err(format!("cannot read value buffer from the stack"))
                    } else {
                        Ok(())
                    }
                }
                _ => Err(format!("incorrect parameter")),
            }?;
        }
        // Parameters have been validated, specify the return value on return.
        let mut next = self.next()?;
        let value = self.resolve_return_value(context, &signature.return_value)?;
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
            return Err(format!("register 0 is incorrect at exit time"));
        }
        // Nothing to do, the program terminated with a valid scalar value.
        Ok(())
    }

    fn jump<'a>(&mut self, context: &mut Self::Context<'a>, offset: i16) -> Result<(), String> {
        bpf_log!(self, context, "ja {}", offset);
        context.states.push(self.jump_with_offset(offset)?);
        Ok(())
    }

    fn jeq<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump("jeq32", context, dst, src, offset, JumpType::Eq, |x, y| {
            comp32(x, y, |x, y| x == y)
        })
    }
    fn jeq64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump("jeq", context, dst, src, offset, JumpType::Eq, |x, y| x == y)
    }
    fn jne<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump("jne32", context, dst, src, offset, JumpType::Ne, |x, y| {
            comp32(x, y, |x, y| x != y)
        })
    }
    fn jne64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump("jne", context, dst, src, offset, JumpType::Ne, |x, y| x != y)
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
            JumpType::LooseComparaison,
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
            JumpType::LooseComparaison,
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
            JumpType::StrictComparaison,
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
            JumpType::StrictComparaison,
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
            JumpType::LooseComparaison,
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
            JumpType::LooseComparaison,
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
            JumpType::StrictComparaison,
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
            JumpType::StrictComparaison,
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
        self.conditional_jump("jset32", context, dst, src, offset, JumpType::Unknown, |x, y| {
            comp32(x, y, |x, y| x & y != 0)
        })
    }
    fn jset64<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump("jset", context, dst, src, offset, JumpType::Unknown, |x, y| {
            x & y != 0
        })
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
            print_offset(offset),
        );
        let addr = self.reg(src)?;
        let loaded_type = self.load_memory(addr, offset as u64, width)?;
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
        let value =
            if let Some(schema) = context.calling_context.map_references.get(&self.pc).cloned() {
                let id = context.next_id();
                Type::ConstPtrToMap { id, schema }
            } else {
                Type::from(value)
            };
        let mut next = self.jump_with_offset(jump_offset)?;
        next.set_reg(dst, value.into())?;
        context.states.push(next);
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
                self.reg(r)?.clone()
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
        let dst = self.reg(dst)?;
        next.store_memory(dst, value, offset as u64, width)?;
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

fn print_offset(offset: i16) -> String {
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
    StackOffset(f(v.reg()))
}

fn error_and_log<T>(
    verification_context: &mut VerificationContext<'_>,
    msg: impl std::string::ToString,
) -> Result<T, UbpfError> {
    let msg = msg.to_string();
    verification_context.log(msg.as_bytes());
    return Err(UbpfError::ProgramLoadError(msg));
}
