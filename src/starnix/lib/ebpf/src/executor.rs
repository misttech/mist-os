// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    visitor::{BpfVisitor, DataWidth, ProgramCounter, Register, Source},
    EbpfProgram, EpbfRunContext, BPF_STACK_SIZE, GENERAL_REGISTER_COUNT,
};
use byteorder::{BigEndian, ByteOrder, LittleEndian};
use std::{mem::MaybeUninit, pin::Pin};
use zerocopy::AsBytes;

pub fn execute_with_arguments<C: EpbfRunContext>(
    program: &EbpfProgram<C>,
    run_context: &mut C::Context<'_>,
    arguments: &[u64],
) -> u64 {
    let arguments = arguments.iter().map(|v| BpfValue::from(*v)).collect::<Vec<_>>();
    execute_impl(program, run_context, &arguments)
}

pub fn execute<C: EpbfRunContext>(
    program: &EbpfProgram<C>,
    run_context: &mut C::Context<'_>,
    data: &mut [u8],
) -> u64 {
    execute_impl(
        program,
        run_context,
        &[BpfValue::from(data.as_mut_ptr()), BpfValue::from(data.len())],
    )
}

fn execute_impl<C: EpbfRunContext>(
    program: &EbpfProgram<C>,
    run_context: &mut C::Context<'_>,
    arguments: &[BpfValue],
) -> u64 {
    assert!(arguments.len() < 5);
    let mut context = ComputationContext {
        program,
        registers: Default::default(),
        stack: vec![MaybeUninit::uninit(); BPF_STACK_SIZE / std::mem::size_of::<BpfValue>()]
            .into_boxed_slice()
            .into(),
        pc: 0,
        result: None,
    };
    for (i, v) in arguments.iter().enumerate() {
        // Arguments are in registers r1 to r5.
        context.set_reg((i as u8) + 1, *v);
    }
    loop {
        if let Some(result) = context.result {
            return result;
        }
        context
            .visit(run_context, &program.code[context.pc..])
            .expect("verifier should have found an issue");
    }
}

#[derive(Clone, Copy, Debug)]
struct BpfValue(*mut u8);

static_assertions::const_assert_eq!(std::mem::size_of::<BpfValue>(), std::mem::size_of::<u64>());

impl Default for BpfValue {
    fn default() -> Self {
        Self::from(0)
    }
}

impl From<i32> for BpfValue {
    fn from(v: i32) -> Self {
        Self(((v as u32) as u64) as *mut u8)
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
        Self(v as *mut u8)
    }
}

impl From<usize> for BpfValue {
    fn from(v: usize) -> Self {
        Self(v as *mut u8)
    }
}

impl From<*mut u8> for BpfValue {
    fn from(v: *mut u8) -> Self {
        Self(v)
    }
}

impl BpfValue {
    fn as_u64(&self) -> u64 {
        self.0 as u64
    }

    fn as_u8_ptr(&self) -> *mut u8 {
        self.0
    }

    fn add(&self, offset: u64) -> Self {
        Self((self.0 as u64).overflowing_add(offset).0 as *mut u8)
    }
}

/// The state of the computation as known by the interpreter at a given point in time.
#[derive(Debug)]
struct ComputationContext<'a, C: EpbfRunContext> {
    /// The program to execute.
    program: &'a EbpfProgram<C>,
    /// Register 0 to 9.
    registers: [BpfValue; GENERAL_REGISTER_COUNT as usize],
    /// The state of the stack.
    stack: Pin<Box<[MaybeUninit<BpfValue>]>>,
    /// The program counter.
    pc: ProgramCounter,
    /// The result, set to Some(value) when the program terminates.
    result: Option<u64>,
}

impl<C: EpbfRunContext> ComputationContext<'_, C> {
    fn reg(&mut self, index: Register) -> BpfValue {
        if index < GENERAL_REGISTER_COUNT {
            self.registers[index as usize]
        } else {
            debug_assert!(index == GENERAL_REGISTER_COUNT);
            BpfValue::from((self.stack.as_mut_ptr() as u64) + (BPF_STACK_SIZE as u64))
        }
    }

    fn set_reg(&mut self, index: Register, value: BpfValue) {
        self.registers[index as usize] = value;
    }

    fn next(&mut self) {
        self.jump_with_offset(0)
    }

    /// Update the `ComputationContext` `pc` to `pc + offset + 1`. In particular, the next
    /// instruction is reached with `jump_with_offset(0)`.
    fn jump_with_offset(&mut self, offset: i16) {
        let mut pc = self.pc as i64;
        pc += (offset as i64) + 1;
        self.pc = pc as usize;
    }

    fn store_memory(
        &mut self,
        addr: BpfValue,
        value: BpfValue,
        instruction_offset: u64,
        width: DataWidth,
    ) {
        // SAFETY
        //
        // The address has been verified by the verifier that ensured the memory is valid for
        // writing.
        let addr = addr.add(instruction_offset).as_u8_ptr();
        match width {
            DataWidth::U8 => unsafe {
                std::ptr::write_unaligned(addr, width.cast(value.as_u64()) as u8)
            },
            DataWidth::U16 => unsafe {
                std::ptr::write_unaligned(addr as *mut u16, width.cast(value.as_u64()) as u16)
            },
            DataWidth::U32 => unsafe {
                std::ptr::write_unaligned(addr as *mut u32, width.cast(value.as_u64()) as u32)
            },
            DataWidth::U64 => unsafe {
                std::ptr::write_unaligned(addr as *mut *mut u8, value.as_u8_ptr())
            },
        }
    }

    fn load_memory(&self, addr: BpfValue, instruction_offset: u64, width: DataWidth) -> BpfValue {
        // SAFETY
        //
        // The address has been verified by the verifier that ensured the memory is valid for
        // reading.
        let addr = addr.add(instruction_offset).as_u8_ptr();
        match width {
            DataWidth::U8 => BpfValue::from(unsafe { std::ptr::read_unaligned(addr) }),
            DataWidth::U16 => {
                BpfValue::from(unsafe { std::ptr::read_unaligned(addr as *const u16) })
            }
            DataWidth::U32 => {
                BpfValue::from(unsafe { std::ptr::read_unaligned(addr as *const u32) })
            }
            DataWidth::U64 => {
                BpfValue::from(unsafe { std::ptr::read_unaligned(addr as *const *mut u8) })
            }
        }
    }

    fn compute_source(&mut self, src: Source) -> BpfValue {
        match src {
            Source::Reg(reg) => self.reg(reg),
            Source::Value(v) => v.into(),
        }
    }

    fn alu(
        &mut self,
        dst: Register,
        src: Source,
        op: impl Fn(u64, u64) -> u64,
    ) -> Result<(), String> {
        let op1 = self.reg(dst).as_u64();
        let op2 = self.compute_source(src).as_u64();
        let result = op(op1, op2);
        self.next();
        self.set_reg(dst, result.into());
        Ok(())
    }

    fn endianness<BO: ByteOrder>(&mut self, dst: Register, width: DataWidth) -> Result<(), String> {
        let value = self.reg(dst);
        let new_value = match width {
            DataWidth::U16 => BO::read_u16((value.as_u64() as u16).as_bytes()) as u64,
            DataWidth::U32 => BO::read_u32((value.as_u64() as u32).as_bytes()) as u64,
            DataWidth::U64 => BO::read_u64(value.as_u64().as_bytes()),
            _ => {
                panic!("Unexpected bit width for endianness operation");
            }
        };
        self.next();
        self.set_reg(dst, new_value.into());
        Ok(())
    }

    fn conditional_jump(
        &mut self,
        dst: Register,
        src: Source,
        offset: i16,
        op: impl Fn(u64, u64) -> bool,
    ) -> Result<(), String> {
        let op1 = self.reg(dst).as_u64();
        let op2 = self.compute_source(src.clone()).as_u64();
        if op(op1, op2) {
            self.jump_with_offset(offset);
        } else {
            self.next();
        }
        Ok(())
    }
}

impl<C: EpbfRunContext> BpfVisitor for ComputationContext<'_, C> {
    type Context<'a> = C::Context<'a>;

    fn add<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| x.overflowing_add(y).0))
    }
    fn add64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| x.overflowing_add(y).0)
    }
    fn and<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| x & y))
    }
    fn and64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| x & y)
    }
    fn arsh<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| {
            alu32(x, y, |x, y| {
                let x = x as i32;
                x.overflowing_shr(y).0 as u32
            })
        })
    }
    fn arsh64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| {
            let x = x as i64;
            x.overflowing_shr(y as u32).0 as u64
        })
    }
    fn div<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| if y == 0 { 0 } else { x / y }))
    }
    fn div64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| if y == 0 { 0 } else { x / y })
    }
    fn lsh<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| x.overflowing_shl(y).0))
    }
    fn lsh64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| x.overflowing_shl(y as u32).0)
    }
    fn r#mod<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| if y == 0 { x } else { x % y }))
    }
    fn mod64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| if y == 0 { x } else { x % y })
    }
    fn mov<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |_x, y| y))
    }
    fn mov64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |_x, y| y)
    }
    fn mul<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| x.overflowing_mul(y).0))
    }
    fn mul64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| x.overflowing_mul(y).0)
    }
    fn or<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| x | y))
    }
    fn or64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| x | y)
    }
    fn rsh<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| x.overflowing_shr(y).0))
    }
    fn rsh64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| x.overflowing_shr(y as u32).0)
    }
    fn sub<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| x.overflowing_sub(y).0))
    }
    fn sub64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| x.overflowing_sub(y).0)
    }
    fn xor<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| alu32(x, y, |x, y| x ^ y))
    }
    fn xor64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
    ) -> Result<(), String> {
        self.alu(dst, src, |x, y| x ^ y)
    }

    fn neg<'a>(&mut self, _context: &mut Self::Context<'a>, dst: Register) -> Result<(), String> {
        self.alu(dst, Source::Value(0), |x, y| {
            alu32(x, y, |x, _y| (x as i32).overflowing_neg().0 as u32)
        })
    }
    fn neg64<'a>(&mut self, _context: &mut Self::Context<'a>, dst: Register) -> Result<(), String> {
        self.alu(dst, Source::Value(0), |x, _y| (x as i64).overflowing_neg().0 as u64)
    }

    fn be<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        self.endianness::<BigEndian>(dst, width)
    }

    fn le<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        self.endianness::<LittleEndian>(dst, width)
    }

    fn call_external<'a>(
        &mut self,
        context: &mut Self::Context<'a>,
        index: u32,
    ) -> Result<(), String> {
        let helper = &self.program.helpers[&index];
        let result = (helper.function_pointer)(
            context,
            self.reg(1).as_u8_ptr(),
            self.reg(2).as_u8_ptr(),
            self.reg(3).as_u8_ptr(),
            self.reg(4).as_u8_ptr(),
            self.reg(5).as_u8_ptr(),
        );
        self.next();
        self.set_reg(0, result.into());
        Ok(())
    }

    fn exit<'a>(&mut self, _context: &mut Self::Context<'a>) -> Result<(), String> {
        self.result = Some(self.reg(0).as_u64());
        Ok(())
    }

    fn jump<'a>(&mut self, _context: &mut Self::Context<'a>, offset: i16) -> Result<(), String> {
        self.jump_with_offset(offset);
        Ok(())
    }

    fn jeq<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| comp32(x, y, |x, y| x == y))
    }
    fn jeq64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| x == y)
    }
    fn jne<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| comp32(x, y, |x, y| x != y))
    }
    fn jne64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| x != y)
    }
    fn jge<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| comp32(x, y, |x, y| x >= y))
    }
    fn jge64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| x >= y)
    }
    fn jgt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| comp32(x, y, |x, y| x > y))
    }
    fn jgt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| x > y)
    }
    fn jle<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| comp32(x, y, |x, y| x <= y))
    }
    fn jle64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| x <= y)
    }
    fn jlt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| comp32(x, y, |x, y| x < y))
    }
    fn jlt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| x < y)
    }
    fn jsge<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| scomp32(x, y, |x, y| x >= y))
    }
    fn jsge64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| scomp64(x, y, |x, y| x >= y))
    }
    fn jsgt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| scomp32(x, y, |x, y| x > y))
    }
    fn jsgt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| scomp64(x, y, |x, y| x > y))
    }
    fn jsle<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| scomp32(x, y, |x, y| x <= y))
    }
    fn jsle64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| scomp64(x, y, |x, y| x <= y))
    }
    fn jslt<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| scomp32(x, y, |x, y| x < y))
    }
    fn jslt64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| scomp64(x, y, |x, y| x < y))
    }
    fn jset<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| comp32(x, y, |x, y| x & y != 0))
    }
    fn jset64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        src: Source,
        offset: i16,
    ) -> Result<(), String> {
        self.conditional_jump(dst, src, offset, |x, y| x & y != 0)
    }

    fn load<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Register,
        width: DataWidth,
    ) -> Result<(), String> {
        let addr = self.reg(src);
        let loaded_type = self.load_memory(addr, offset as u64, width);
        self.next();
        self.set_reg(dst, loaded_type);
        Ok(())
    }

    fn load64<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        value: u64,
        jump_offset: i16,
    ) -> Result<(), String> {
        self.jump_with_offset(jump_offset);
        self.set_reg(dst, value.into());
        Ok(())
    }

    fn store<'a>(
        &mut self,
        _context: &mut Self::Context<'a>,
        dst: Register,
        offset: i16,
        src: Source,
        width: DataWidth,
    ) -> Result<(), String> {
        let src = self.compute_source(src);
        let dst = self.reg(dst);
        self.next();
        self.store_memory(dst, src, offset as u64, width);
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
