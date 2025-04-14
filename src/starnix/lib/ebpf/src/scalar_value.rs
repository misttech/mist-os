// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use byteorder::{ByteOrder, NativeEndian};
use std::cmp::Ordering;
use zerocopy::IntoBytes;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range<T: Clone + Copy + std::fmt::Debug + PartialOrd + Ord + PartialEq + Eq> {
    pub min: T,
    pub max: T,
}

impl<T: Clone + Copy + std::fmt::Debug + PartialOrd + Ord + PartialEq + Eq> Range<T> {
    pub const fn new(min: T, max: T) -> Self {
        Self { min, max }
    }
}

impl<T: Clone + Copy + std::fmt::Debug + PartialOrd + Ord + PartialEq + Eq> From<T> for Range<T> {
    fn from(value: T) -> Self {
        Self::new(value, value)
    }
}

macro_rules! make_from {
    ($target_type:ty : $base_from_type:ty {$($types:ty),*}) => {
        $(
            impl From<$types> for $target_type {
                fn from(v: $types) -> Self {
                    (v as $base_from_type).into()
                }
            }
        )*
    }
}

macro_rules! make_scalar_value_data {
    ($t:ident ($($types:ty),*)) => { paste::paste! {

make_from!([< $t:upper Range>]: [< $t >] {$($types),*});
make_from!([< $t:upper ScalarValueData>]: [< $t >] {$($types),*});

pub type [< $t:upper Range>] = Range<[< $t >]>;

impl [< $t:upper Range>] {
    pub const fn max() -> Self {
        Self::new(0, [< $t >]::MAX)
    }

    pub fn extract_slices(value: [< $t >], offset: usize, byte_count: usize) -> ([< $t >], [< $t >], [< $t >]) {
        let v1 = if offset > 0 { NativeEndian::read_uint(&value.as_bytes(), offset) as [< $t >] } else { 0 };
        let v2 = NativeEndian::read_uint(&value.as_bytes()[offset..], byte_count) as [< $t >];
        let v3 = if offset + byte_count < std::mem::size_of::<[< $t >]>() {
            NativeEndian::read_uint(
                &value.as_bytes()[(offset + byte_count)..],
                std::mem::size_of::<[< $t >]>() - offset - byte_count,
            ) as [< $t >]
        } else {
            0
        };
        if cfg!(target_endian = "little") {
            (v1, v2, v3)
        } else {
            (v3, v2, v1)
        }
    }

    pub fn assemble_slices(values: ([< $t >], [< $t >], [< $t >]), offset: usize, byte_count: usize) -> [< $t >] {
        let mut result: [< $t >] = 0;
        let (v1, v2, v3) =
            if cfg!(target_endian = "little") { values } else { (values.2, values.1, values.0) };
        if offset > 0 {
            result.as_mut_bytes()[..offset].copy_from_slice(&v1.as_bytes()[..offset]);
        }
        result.as_mut_bytes()[offset..(offset + byte_count)]
            .copy_from_slice(&v2.as_bytes()[..byte_count]);
        result.as_mut_bytes()[(offset + byte_count)..]
            .copy_from_slice(&v3.as_bytes()[..(8 - offset - byte_count)]);
        result
    }

    /// Given a target and source values, each in the `target` and `source` ranges. Compute the
    /// range of the result of the operation where `byte_count` bytes from `source` at offset
    /// `source_offset` replace `byte_count` bytes from `target` at offset `target_offset`.
    pub fn compute_range_for_bytes_swap(
        target: [< $t:upper Range>],
        source: [< $t:upper Range>],
        target_offset: usize,
        source_offset: usize,
        byte_count: usize,
    ) -> [< $t:upper Range>] {
        let (target_umin1, target_umin2, target_umin3) =
            Self::extract_slices(target.min, target_offset, byte_count);
        let (target_umax1, target_umax2, target_umax3) =
            Self::extract_slices(target.max, target_offset, byte_count);
        let (_, source_umin2, source_umin3) =
            Self::extract_slices(source.min, source_offset, byte_count);
        let (_, source_umax2, source_umax3) =
            Self::extract_slices(source.max, source_offset, byte_count);

        let (final_umin3, final_umax3) = (target_umin3, target_umax3);
        let (final_umin2, final_umax2) =
            if source_umax3 > source_umin3 { (0, [< $t >]::MAX) } else { (source_umin2, source_umax2) };
        let (final_umin1, final_umax1) =
            if target_umax3 > target_umin3 || target_umax2 > target_umin2 {
                (0, [< $t >]::MAX)
            } else {
                (target_umin1, target_umax1)
            };

        let final_min = Self::assemble_slices(
            (final_umin1, final_umin2, final_umin3),
            target_offset,
            byte_count,
        );
        let final_max = Self::assemble_slices(
            (final_umax1, final_umax2, final_umax3),
            target_offset,
            byte_count,
        );

        [< $t:upper Range>]::new(final_min, final_max)
    }

    pub fn checked_add<T: Into<[< $t:upper Range>]>>(self, rhs: T) -> Option<Self> {
        let rhs = rhs.into();
        let min = self.min.checked_add(rhs.min)?;
        let max = self.max.checked_add(rhs.max)?;
        Some(Self {min, max})
    }

    pub fn checked_sub<T: Into<[< $t:upper Range>]>>(self, rhs: T) -> Option<Self> {
        let rhs = rhs.into();
        let min = self.min.checked_sub(rhs.min)?;
        let max = self.max.checked_sub(rhs.max)?;
        Some(Self {min, max})
    }
}

impl PartialOrd for [< $t:upper Range>] {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self == other {
            return Some(Ordering::Equal);
        }
        if self.min <= other.min && self.max >= other.max {
            return Some(Ordering::Greater);
        }
        if self.min >= other.min && self.max <= other.max {
            return Some(Ordering::Less);
        }
        None
    }
}

impl<T: Into<[< $t:upper Range>]>> std::ops::Add<T> for [< $t:upper Range>] {
    type Output = Self;
    fn add(self, rhs: T) -> Self {
        let rhs = rhs.into();
        let (min, min_overflowed) = self.min.overflowing_add(rhs.min);
        let (max, max_overflowed) = self.max.overflowing_add(rhs.max);
        if min_overflowed != max_overflowed {
            Self::max()
        } else {
            Self {min, max}
        }
    }
}

impl std::ops::Div for [< $t:upper Range>] {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        if rhs.max == 0 {
            return Self::new(0, 0);
        }
        let min = self.min / rhs.max;
        let max = self.max / std::cmp::max(1, rhs.min);
        Self { min, max }
    }
}

impl std::ops::Mul for [< $t:upper Range>] {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        let Some(min) = self.min.checked_mul(rhs.min) else {
            return Self::max();
        };
        let Some(max) = self.max.checked_mul(rhs.max) else {
            return Self::max();
        };
        Self { min, max }
    }
}

impl std::ops::Neg for [< $t:upper Range>] {
    type Output = Self;
    fn neg(self) -> Self {
        if self.max == 0 {
            return self
        }
        if self.min == 0 {
            return Self::max()
        }
        Self::new(self.max.wrapping_neg(), self.min.wrapping_neg())
    }
}

impl std::ops::Rem for [< $t:upper Range>] {
    type Output = Self;
    fn rem(self, rhs: Self) -> Self {
        if rhs.max == 0 {
            return self;
        }
        let min = 0;
        let max = std::cmp::min(self.max, rhs.max - 1);
        Self { min, max }
    }
}

impl std::ops::Sub for [< $t:upper Range>] {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        let (min, min_overflowed) = self.min.overflowing_sub(rhs.max);
        let (max, max_overflowed) = self.max.overflowing_sub(rhs.min);
        if min_overflowed != max_overflowed {
            Self::max()
        } else {
            Self {min, max}
        }
    }
}

impl std::ops::Shl for [< $t:upper Range>] {
    type Output = Self;
    fn shl(self, rhs: Self) -> Self {
        #[allow(irrefutable_let_patterns)]
        let Ok(rhs_max) = u32::try_from(rhs.max) else {
            return Self::max();
        };
        #[allow(irrefutable_let_patterns)]
        let Ok(rhs_min) = u32::try_from(rhs.min) else {
            return Self::max();
        };
        if self.max == 0 {
            return self;
        }
        if rhs_max > self.max.leading_zeros() {
            return Self::max();
        }
        let min = self.min.overflowing_shl(rhs_min).0;
        let max = self.max.overflowing_shl(rhs_max).0;
        Self { min, max }
    }
}

impl std::ops::Shr for [< $t:upper Range>] {
    type Output = Self;
    fn shr(self, rhs: Self) -> Self {
        #[allow(irrefutable_let_patterns)]
        let Ok(rhs_max) = u32::try_from(rhs.max) else {
            return Self::max();
        };
        #[allow(irrefutable_let_patterns)]
        let Ok(rhs_min) = u32::try_from(rhs.min) else {
            return Self::max();
        };
        let min = self.min.overflowing_shr(rhs_max).0;
        let max = self.min.overflowing_shr(rhs_min).0;
        Self { min, max }
    }
}


#[derive(Clone, Copy, Debug, PartialEq)]
pub struct [< $t:upper ScalarValueData>] {
    /// The value. Its interpresentation depends on `unknown_mask` and `unwritten_mask`.
    pub value: [< $t >],
    /// A bit mask of unknown bits. A bit in `value` is valid (and can be used by the verifier)
    /// if the equivalent mask in unknown_mask is 0.
    pub unknown_mask: [< $t >],
    /// A bit mask of unwritten bits. A bit in `value` is written (and can be sent back to
    /// userspace) if the equivalent mask in unwritten_mask is 0. `unwritten_mask` must always be a
    /// subset of `unknown_mask`.
    pub unwritten_mask: [< $t >],
    /// The range of possible unsigned values of this scalar.
    pub urange: [< $t:upper Range >],
    /// Prevent instantiation without using new.
    _guard: (),
}

/// Defines a partial ordering on `ScalarValueData` instances, capturing the notion of how "broad"
/// a scalar value is in terms of the set of potential values it represents.
///
/// The ordering is defined such that `s1 > s2` if a proof that an eBPF program terminates
/// in a state where a register or memory location has a scalar value `s1` is also a proof that
/// the program terminates in a state where that location has scalar value `s2`.
///
/// In other words, a "broader" value represents a larger set of possible values, and
/// proving termination with a broader type implies termination with any narrower value.
///
/// Examples:
/// * `ScalarValueData { unknown_mask: 0, .. }` (a known scalar value) is less than
///   `ScalarValueData { unknown_mask: u64::MAX, .. }` (an unknown scalar value).
impl PartialOrd for [< $t:upper ScalarValueData>] {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        fn mask_is_larger(m1: [< $t >], m2: [< $t >]) -> bool {
            m1 & m2 == m2
        }

        // If the values are equals, return the known result.
        if self == other {
            return Some(Ordering::Equal);
        }

        if mask_is_larger(self.unwritten_mask, other.unwritten_mask)
            && mask_is_larger(self.unknown_mask, other.unknown_mask)
            && self.value & !self.unknown_mask == other.value & !self.unknown_mask
            && self.urange >= other.urange
        {
            return Some(Ordering::Greater);
        }
        if mask_is_larger(other.unwritten_mask, self.unwritten_mask)
            && mask_is_larger(other.unknown_mask, self.unknown_mask)
            && self.value & !other.unknown_mask == other.value & !other.unknown_mask
            && self.urange <= other.urange
        {
            return Some(Ordering::Less);
        }
        None
    }
}

impl From<&[< $t:upper ScalarValueData>]> for [< $t:upper ScalarValueData>] {
    fn from(value: &Self) -> Self {
        value.clone()
    }
}

impl From<[< $t >]> for [< $t:upper ScalarValueData>] {
    fn from(value: [< $t >]) -> Self {
        Self::new(value, 0, 0, value.into())
    }
}

impl [< $t:upper ScalarValueData>] {
    pub const UNINITIALIZED: Self =
        Self::new(0, [< $t >]::MAX, [< $t >]::MAX, [< $t:upper Range >]::max());
    pub const UNKNOWN_WRITTEN: Self =
        Self::new(0, [< $t >]::MAX, 0, [< $t:upper Range >]::max());

    pub const fn new(value: [< $t >], unknown_mask: [< $t >], unwritten_mask: [< $t >], urange: [< $t:upper Range >]) -> Self {
        debug_assert!(value <= urange.max);
        debug_assert!(unknown_mask & unwritten_mask == unwritten_mask);
        debug_assert!(value & unknown_mask == 0);
        let urange = if unknown_mask == 0 {
            [< $t:upper Range >]::new(value, value)
        } else {
            let mut min = value;
            if urange.min > min {
                min = urange.min;
            }
            let mut max = value | unknown_mask;
            if urange.max < max {
                max = urange.max;
            }
            [< $t:upper Range >]::new(min, max)
        };
        Self { value, unknown_mask, unwritten_mask, urange, _guard: () }
    }

    pub const fn is_known(&self) -> bool {
        self.unknown_mask == 0
    }

    pub const fn is_fully_initialized(&self) -> bool {
        self.unwritten_mask == 0
    }

    pub const fn is_uninitialized(&self) -> bool {
        self.unwritten_mask == [< $t >]::MAX
    }

    pub const fn is_zero(&self) -> bool {
        self.is_known() && self.value == 0
    }

    pub const fn min(&self) -> [< $t >] {
        self.urange.min
    }

    pub const fn max(&self) -> [< $t >] {
        self.urange.max
    }

    pub fn update_range(self, urange: [< $t:upper Range >]) -> Self {
        Self::new(self.value, self.unknown_mask, self.unwritten_mask, urange)
    }

    pub fn checked_add<T: Into<[< $t:upper ScalarValueData>]>>(self, rhs: T) -> Option<Self> {
        let rhs = rhs.into();
        if self.is_known() && rhs.is_known() {
            return self.value.checked_add(rhs.value).map(Into::into);
        }
        Some(Self {
            urange: self.urange.checked_add(rhs.urange)?,
            .. self.base(rhs)
        })
    }

    pub fn checked_sub<T: Into<[< $t:upper ScalarValueData>]>>(self, rhs: T) -> Option<Self> {
        let rhs = rhs.into();
        if self.is_known() && rhs.is_known() {
            return self.value.checked_sub(rhs.value).map(Into::into);
        }
        Some(Self {
            urange: self.urange.checked_sub(rhs.urange)?,
            .. self.base(rhs)
        })
    }

    /// Arithmetic right shift.
    pub fn ashr(self, rhs: Self) -> Self {
        fn ashr(x: [< $t >], y: [< $t >]) -> [< $t >] {
            let x = x.cast_signed();
            if y > u32::MAX.into() {
                if x >= 0 {
                    0
                } else {
                    [< $t >]::MAX
                }
            } else {
                x.overflowing_shr(y as u32).0.cast_unsigned()
            }
        }
        fn shr(x: [< $t >], y: [< $t >]) -> [< $t >] {
            if y > u32::MAX.into() {
                0
            } else {
                x.overflowing_shr(y as u32).0
            }
        }
        if !rhs.is_known() {
            return self.base(rhs);
        }
        if self.is_known() {
            return ashr(self.value, rhs.value).into();
        }
        let unknown_mask = shr(self.unknown_mask, rhs.value);
        let unwritten_mask = shr(self.unwritten_mask, rhs.value);
        let value = ashr(self.value, rhs.value) & !unknown_mask;
        let urange = [< $t:upper Range >]::max();
        Self::new(
            value, unknown_mask, unwritten_mask, urange
        )
    }

    fn bitwise_operation(self,
                        rhs: Self,
                        urange: [< $t:upper Range >],
                        op: impl Fn([< $t >], [< $t >]) -> [< $t >]) -> Self {
        let unknown_mask = self.unknown_mask | rhs.unknown_mask;
        let unwritten_mask = self.unwritten_mask | rhs.unwritten_mask;
        let value = op(self.value, rhs.value) & !unknown_mask;
        Self::new(value, unknown_mask, unwritten_mask, urange)
    }

    fn shift_operation(self, rhs: [< $t >], urange: [< $t:upper Range >], op: impl Fn([< $t >], [< $t >]) -> [< $t >]) -> Self {
        let value = op(self.value, rhs);
        let unknown_mask = op(self.unknown_mask, rhs);
        let unwritten_mask = op(self.unwritten_mask, rhs);
        Self::new(value, unknown_mask, unwritten_mask, urange)
    }

    fn base(&self, rhs: Self) -> Self {
        if !self.is_fully_initialized() || !rhs.is_fully_initialized() {
            Self::UNINITIALIZED
        } else {
            Self::UNKNOWN_WRITTEN
        }
    }
}

impl<T: Into<[< $t:upper ScalarValueData>]>> std::ops::Add<T> for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn add(self, rhs: T) -> Self {
        let rhs = rhs.into();
        if self.is_known() && rhs.is_known() {
            return self.value.overflowing_add(rhs.value).0.into();
        }
        Self {
            urange: self.urange + rhs.urange,
            .. self.base(rhs)
        }
    }
}

impl std::ops::Div for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        if self.is_known() && rhs.is_known() {
            if rhs.value  == 0 {
                return 0.into();
            } else {
                return (self.value / rhs.value).into();
            }
        }
        Self {
            urange: self.urange / rhs.urange,
            .. self.base(rhs)
        }
    }
}

impl std::ops::Mul for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        if self.is_known() && rhs.is_known() {
            return self.value.overflowing_mul(rhs.value).0.into();
        }
        Self {
            urange: self.urange * rhs.urange,
            .. self.base(rhs)
        }
    }
}

impl std::ops::Neg for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn neg(self) -> Self {
        if self.is_known() {
            return self.value.wrapping_neg().into();
        }
        let base = if !self.is_fully_initialized() {
            Self::UNINITIALIZED
        } else {
            Self::UNKNOWN_WRITTEN
        };
        Self {
            urange: -self.urange,
            .. base
        }
    }
}

impl std::ops::Rem for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn rem(self, rhs: Self) -> Self {
        if self.is_known() && rhs.is_known() {
            if rhs.value == 0 {
                return self;
            } else {
                return (self.value % rhs.value).into();
            }
        }
        Self {
            urange: self.urange % rhs.urange,
            .. self.base(rhs)
        }
    }
}

impl<T: Into<[< $t:upper ScalarValueData>]>> std::ops::Sub<T> for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn sub(self, rhs: T) -> Self {
        let rhs = rhs.into();
        if self.is_known() && rhs.is_known() {
            return self.value.overflowing_sub(rhs.value).0.into();
        }
        Self {
            urange: self.urange - rhs.urange,
            .. self.base(rhs)
        }
    }
}

impl std::ops::BitAnd for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        let urange = [< $t:upper Range >]::new(0, std::cmp::min(self.max(), rhs.max()));
        self.bitwise_operation(rhs, urange, |x, y| x & y)
    }
}

impl std::ops::BitOr for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        let urange = [< $t:upper Range >]::new( std::cmp::max(self.min(), rhs.min()), [< $t >]::MAX);
        self.bitwise_operation(rhs, urange, |x, y| x | y)
    }
}

impl std::ops::BitXor for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self {
        let urange = [< $t:upper Range >]::max();
        self.bitwise_operation(rhs, urange, |x, y| x ^ y)
    }
}

impl std::ops::Shl for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn shl(self, rhs: Self) -> Self {
        let urange = self.urange << rhs.urange;
        if rhs.is_known() {
            return self.shift_operation(rhs.value, urange, |x, y| {
                if y > u32::MAX.into() {
                    0
                } else {
                    x.overflowing_shl(y as u32).0
                }
            });
        }
        Self {
            urange,
            .. self.base(rhs)
        }
    }
}

impl std::ops::Shr for [< $t:upper ScalarValueData>] {
    type Output = Self;
    fn shr(self, rhs: Self) -> Self {
        let urange = self.urange >> rhs.urange;
        if rhs.is_known() {
            return self.shift_operation(rhs.value, urange, |x, y| {
                if y > u32::MAX.into() {
                    0
                } else {
                    x.overflowing_shr(y as u32).0
                }
            });
        }
        Self {
            urange,
            .. self.base(rhs)
        }
    }
}

}}}
make_scalar_value_data!(u32(i32, u16, i16, u8, i8));
make_scalar_value_data!(u64(i64, u32, i32, u16, i16, u8, i8, usize));
pub type ScalarValueData = U64ScalarValueData;

impl From<U64ScalarValueData> for U32ScalarValueData {
    fn from(v: U64ScalarValueData) -> Self {
        let urange = if v.max() >> 32 == v.min() >> 32 {
            U32Range::new(v.min() as u32, v.max() as u32)
        } else {
            U32Range::max()
        };
        Self::new(v.value as u32, v.unknown_mask as u32, v.unwritten_mask as u32, urange)
    }
}

impl From<U32ScalarValueData> for U64ScalarValueData {
    fn from(v: U32ScalarValueData) -> Self {
        Self::new(
            v.value.into(),
            v.unknown_mask.into(),
            v.unwritten_mask.into(),
            U64Range::new(v.min().into(), v.max().into()),
        )
    }
}
