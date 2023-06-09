// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{FdNumber, WdNumber},
    types::*,
};

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct SyscallArg(u64);

impl SyscallArg {
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn raw(&self) -> u64 {
        self.0
    }
}

impl std::fmt::LowerHex for SyscallArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerHex::fmt(&self.0, f)
    }
}

pub trait IntoSyscallArg<T> {
    fn into_arg(self) -> T;
}
impl<T> IntoSyscallArg<T> for SyscallArg
where
    T: FromSyscallArg,
{
    fn into_arg(self) -> T {
        T::from_arg(self)
    }
}
pub trait FromSyscallArg {
    fn from_arg(arg: SyscallArg) -> Self;
}
macro_rules! impl_from_syscall_arg {
    { for $ty:ty: $arg:ident => $($body:tt)* } => {
        impl FromSyscallArg for $ty {
            fn from_arg($arg: SyscallArg) -> Self { $($body)* }
        }
    }
}

impl_from_syscall_arg! { for SyscallArg: arg => arg }
impl_from_syscall_arg! { for i32: arg => arg.raw() as Self }
impl_from_syscall_arg! { for i64: arg => arg.raw() as Self }
impl_from_syscall_arg! { for u32: arg => arg.raw() as Self }
impl_from_syscall_arg! { for usize: arg => arg.raw() as Self }
impl_from_syscall_arg! { for u64: arg => arg.raw() as Self }
impl_from_syscall_arg! { for UserAddress: arg => Self::from(arg.raw()) }
impl_from_syscall_arg! { for UserCString: arg => Self::new(arg.into_arg()) }

impl_from_syscall_arg! { for FdNumber: arg => Self::from_raw(arg.raw() as i32) }
impl_from_syscall_arg! { for FileMode: arg => Self::from_bits(arg.raw() as u32) }
impl_from_syscall_arg! { for DeviceType: arg => Self::from_bits(arg.raw()) }
impl_from_syscall_arg! { for UncheckedSignal: arg => Self::new(arg.raw()) }
impl_from_syscall_arg! { for WdNumber: arg => Self::from_raw(arg.raw() as i32) }

impl<T> FromSyscallArg for UserRef<T> {
    fn from_arg(arg: SyscallArg) -> UserRef<T> {
        Self::new(UserAddress::from(arg.raw()))
    }
}
