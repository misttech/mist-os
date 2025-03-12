// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::errors::Errno;
use super::math::round_up_to_increment;
use super::uapi;
use crate::{errno, uref};
use std::marker::PhantomData;
use std::{fmt, mem, ops};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};
use zx_types::zx_vaddr_t;

#[derive(
    Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd, IntoBytes, KnownLayout, FromBytes, Immutable,
)]
#[repr(transparent)]
pub struct UserAddress(u64);

#[derive(
    Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd, IntoBytes, KnownLayout, FromBytes, Immutable,
)]
#[repr(transparent)]
pub struct UserAddress32(u32);

impl UserAddress32 {
    const NULL_PTR: u32 = 0;

    pub const NULL: Self = Self(Self::NULL_PTR);
}

impl UserAddress {
    const NULL_PTR: u64 = 0;

    pub const NULL: Self = Self(Self::NULL_PTR);

    // TODO(lindkvist): Remove this in favor of marking the From<u64> trait const once feature is
    // stabilized.
    pub const fn const_from(value: u64) -> Self {
        UserAddress(value)
    }

    pub fn from_ptr(ptr: zx_vaddr_t) -> Self {
        UserAddress(ptr as u64)
    }

    pub fn ptr(&self) -> zx_vaddr_t {
        self.0 as zx_vaddr_t
    }

    pub fn round_up(&self, increment: u64) -> Result<UserAddress, Errno> {
        Ok(UserAddress(round_up_to_increment(self.0 as usize, increment as usize)? as u64))
    }

    pub fn is_aligned(&self, alignment: u64) -> bool {
        self.0 % alignment == 0
    }

    pub fn is_null(&self) -> bool {
        self.0 == UserAddress::NULL_PTR
    }

    pub fn checked_add(&self, rhs: usize) -> Option<UserAddress> {
        self.0.checked_add(rhs as u64).map(UserAddress)
    }

    pub fn checked_add_signed(&self, rhs: isize) -> Option<UserAddress> {
        self.0.checked_add_signed(rhs as i64).map(UserAddress)
    }

    pub fn checked_sub(&self, rhs: usize) -> Option<UserAddress> {
        self.0.checked_sub(rhs as u64).map(UserAddress)
    }

    pub fn saturating_add(&self, rhs: usize) -> Self {
        UserAddress(self.0.saturating_add(rhs as u64))
    }

    pub fn saturating_sub(&self, rhs: usize) -> Self {
        UserAddress(self.0.saturating_sub(rhs as u64))
    }

    pub fn is_lower_32bit(&self) -> bool {
        self.0 < (1 << 32)
    }
}

impl Default for UserAddress {
    fn default() -> Self {
        Self::NULL
    }
}

impl From<u64> for UserAddress {
    fn from(value: u64) -> Self {
        UserAddress(value)
    }
}

impl From<UserAddress> for u64 {
    fn from(value: UserAddress) -> Self {
        value.0
    }
}

impl From<uapi::uaddr> for UserAddress {
    fn from(value: uapi::uaddr) -> Self {
        UserAddress(value.addr)
    }
}

impl From<UserAddress> for uapi::uaddr {
    fn from(value: UserAddress) -> Self {
        Self { addr: value.0 }
    }
}

impl ops::Add<u32> for UserAddress {
    type Output = UserAddress;

    fn add(self, rhs: u32) -> UserAddress {
        UserAddress(self.0 + (rhs as u64))
    }
}

impl ops::Add<u64> for UserAddress {
    type Output = UserAddress;

    fn add(self, rhs: u64) -> UserAddress {
        UserAddress(self.0 + rhs)
    }
}

impl ops::Add<usize> for UserAddress {
    type Output = UserAddress;

    fn add(self, rhs: usize) -> UserAddress {
        UserAddress(self.0 + (rhs as u64))
    }
}

impl ops::Sub<u32> for UserAddress {
    type Output = UserAddress;

    fn sub(self, rhs: u32) -> UserAddress {
        UserAddress(self.0 - (rhs as u64))
    }
}

impl ops::Sub<u64> for UserAddress {
    type Output = UserAddress;

    fn sub(self, rhs: u64) -> UserAddress {
        UserAddress(self.0 - rhs)
    }
}

impl ops::Sub<usize> for UserAddress {
    type Output = UserAddress;

    fn sub(self, rhs: usize) -> UserAddress {
        UserAddress(self.0 - (rhs as u64))
    }
}

impl ops::AddAssign<usize> for UserAddress {
    fn add_assign(&mut self, rhs: usize) {
        *self = *self + rhs;
    }
}

impl ops::SubAssign<usize> for UserAddress {
    fn sub_assign(&mut self, rhs: usize) {
        *self = *self - rhs;
    }
}

impl ops::Sub<UserAddress> for UserAddress {
    type Output = usize;

    fn sub(self, rhs: UserAddress) -> usize {
        self.ptr() - rhs.ptr()
    }
}

impl fmt::Display for UserAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl fmt::Debug for UserAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("UserAddress").field(&format_args!("{:#x}", self.0)).finish()
    }
}

impl fmt::Debug for UserAddress32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("UserAddress32").field(&format_args!("{:#x}", self.0)).finish()
    }
}

impl Default for UserAddress32 {
    fn default() -> Self {
        Self::NULL
    }
}

impl From<u32> for UserAddress32 {
    fn from(value: u32) -> Self {
        UserAddress32(value)
    }
}

impl TryFrom<UserAddress> for UserAddress32 {
    type Error = Errno;
    fn try_from(value: UserAddress) -> Result<Self, Self::Error> {
        Ok(UserAddress32(u32::try_from(value.0).map_err(|_| errno!(EFAULT))?))
    }
}

impl From<UserAddress32> for UserAddress {
    fn from(value: UserAddress32) -> Self {
        UserAddress(value.0 as u64)
    }
}

#[derive(Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[repr(transparent)]
pub struct UserRef<T> {
    addr: UserAddress,
    phantom: PhantomData<T>,
}

impl<T> UserRef<T> {
    pub fn new(addr: UserAddress) -> Self {
        Self { addr, phantom: PhantomData }
    }

    pub fn addr(&self) -> UserAddress {
        self.addr
    }

    pub fn next(&self) -> UserRef<T> {
        Self::new(self.addr() + mem::size_of::<T>())
    }

    pub fn at(&self, index: usize) -> Self {
        UserRef::<T>::new(self.addr() + index * mem::size_of::<T>())
    }

    pub fn cast<S>(&self) -> UserRef<S> {
        UserRef::<S>::new(self.addr)
    }
}

impl<T> Clone for UserRef<T> {
    fn clone(&self) -> Self {
        Self { addr: self.addr, phantom: Default::default() }
    }
}

impl<T> Copy for UserRef<T> {}

impl<T> From<UserAddress> for UserRef<T> {
    fn from(user_address: UserAddress) -> Self {
        Self::new(user_address)
    }
}

impl<T> From<UserRef<T>> for UserAddress {
    fn from(user: UserRef<T>) -> UserAddress {
        user.addr
    }
}

impl<T> From<uapi::uref<T>> for UserRef<T> {
    fn from(value: uapi::uref<T>) -> Self {
        Self::new(value.addr.into())
    }
}

impl<T> From<UserRef<T>> for uapi::uref<T> {
    fn from(value: UserRef<T>) -> Self {
        uapi::uaddr::from(value.addr).into()
    }
}

impl<T> ops::Deref for UserRef<T> {
    type Target = UserAddress;

    fn deref(&self) -> &UserAddress {
        &self.addr
    }
}

impl<T> fmt::Display for UserRef<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.addr().fmt(f)
    }
}

pub trait ArchSpecific {
    fn is_arch32(&self) -> bool;
}

pub trait MultiArchFrom<T>: Sized {
    fn from_64(value: T) -> Self;
    fn from_32(value: T) -> Self;
}

impl<T, U: From<T>> MultiArchFrom<T> for U {
    fn from_64(value: T) -> Self {
        Self::from(value)
    }
    fn from_32(value: T) -> Self {
        Self::from(value)
    }
}

pub trait Into64<T>: Sized {
    fn into_64(self) -> T;
}

impl<T, U: MultiArchFrom<T>> Into64<U> for T {
    fn into_64(self) -> U {
        U::from_64(self)
    }
}

pub trait Into32<T>: Sized {
    fn into_32(self) -> T;
}

impl<T, U: MultiArchFrom<T>> Into32<U> for T {
    fn into_32(self) -> U {
        U::from_32(self)
    }
}

#[derive(Debug)]
pub enum MappingMultiArchUserRef<T, T64, T32> {
    Arch64(UserRef<T64>, core::marker::PhantomData<T>),
    Arch32(UserRef<T32>),
}

pub type MultiArchUserRef<T64, T32> = MappingMultiArchUserRef<T64, T64, T32>;

impl<T, T64, T32> MappingMultiArchUserRef<T, T64, T32> {
    pub fn new<Arch: ArchSpecific, Addr: Into<UserAddress>>(arch: &Arch, address: Addr) -> Self {
        if arch.is_arch32() {
            Self::Arch32(address.into().into())
        } else {
            Self::Arch64(address.into().into(), Default::default())
        }
    }

    pub fn null<Arch: ArchSpecific>(arch: &Arch) -> Self {
        Self::new(arch, UserAddress::NULL)
    }

    pub fn from_32(addr: UserRef<T32>) -> Self {
        Self::Arch32(addr)
    }

    pub fn is_null(&self) -> bool {
        self.addr() == UserAddress::NULL
    }

    pub fn addr(&self) -> UserAddress {
        match self {
            Self::Arch64(addr, _) => addr.addr(),
            Self::Arch32(addr) => addr.addr(),
        }
    }
}

impl<T, T64: IntoBytes, T32: IntoBytes> MappingMultiArchUserRef<T, T64, T32> {
    pub fn next(&self) -> Self {
        let offset =
            if self.is_arch32() { std::mem::size_of::<T32>() } else { std::mem::size_of::<T64>() };
        Self::new(self, self.addr() + offset)
    }
}

impl<T, T64, T32>
    MappingMultiArchUserRef<
        MappingMultiArchUserRef<T, T64, T32>,
        MappingMultiArchUserRef<T, T64, T32>,
        MappingMultiArchUserRef<T, T64, T32>,
    >
{
    pub fn next(&self) -> Self {
        let offset = if self.is_arch32() {
            std::mem::size_of::<UserAddress32>()
        } else {
            std::mem::size_of::<UserAddress>()
        };
        Self::new(self, self.addr() + offset)
    }
}

impl<T, T64, T32> Clone for MappingMultiArchUserRef<T, T64, T32> {
    fn clone(&self) -> Self {
        match self {
            Self::Arch64(ur, _) => Self::Arch64(*ur, Default::default()),
            Self::Arch32(ur) => Self::Arch32(*ur),
        }
    }
}

impl<T, T64, T32> Copy for MappingMultiArchUserRef<T, T64, T32> {}

impl<T, T64, T32> ArchSpecific for MappingMultiArchUserRef<T, T64, T32> {
    fn is_arch32(&self) -> bool {
        matches!(self, Self::Arch32(_))
    }
}

impl<T, T64, T32> ops::Deref for MappingMultiArchUserRef<T, T64, T32> {
    type Target = UserAddress;

    fn deref(&self) -> &UserAddress {
        match self {
            Self::Arch64(addr, _) => addr.deref(),
            Self::Arch32(addr) => addr.deref(),
        }
    }
}

impl<T, T64, T32> From<UserRef<T64>> for MappingMultiArchUserRef<T, T64, T32> {
    fn from(addr: UserRef<T64>) -> Self {
        Self::Arch64(addr, Default::default())
    }
}

impl<T, T64, T32> From<uref<T64>> for MappingMultiArchUserRef<T, T64, T32> {
    fn from(addr: uref<T64>) -> Self {
        Self::Arch64(addr.into(), Default::default())
    }
}

impl<T, T64, T32> From<crate::uref32<T32>> for MappingMultiArchUserRef<T, T64, T32> {
    fn from(addr: crate::uref32<T32>) -> Self {
        Self::Arch32(uref::from(addr).into())
    }
}

pub type UserCString = MultiArchUserRef<u8, u8>;
pub type UserCStringPtr = MultiArchUserRef<UserCString, UserCString>;

impl fmt::Display for UserCString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.addr().fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::{UserAddress, UserRef};

    #[test]
    fn test_into() {
        assert_eq!(UserRef::<u32>::default(), UserAddress::default().into());
        let user_address = UserAddress::from(32);
        assert_eq!(UserRef::<i32>::new(user_address), user_address.into());
    }
}
