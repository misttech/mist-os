// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(not(target_endian = "little"))]
compile_error!("only little-endian targets are supported by FIDL");

macro_rules! impl_unop {
    ($trait:ident:: $fn:ident for $name:ident : $prim:ty) => {
        impl ::core::ops::$trait for $name {
            type Output = <$prim as ::core::ops::$trait>::Output;

            #[inline]
            fn $fn(self) -> Self::Output {
                self.0.$fn()
            }
        }
    };
}

macro_rules! impl_binop_one {
    ($trait:ident:: $fn:ident($self:ty, $other:ty) -> $output:ty) => {
        impl ::core::ops::$trait<$other> for $self {
            type Output = $output;

            #[inline]
            fn $fn(self, other: $other) -> Self::Output {
                self.0.$fn(other.0)
            }
        }
    };
}

macro_rules! impl_binop_both {
    ($trait:ident:: $fn:ident($self:ty, $other:ty) -> $output:ty) => {
        impl ::core::ops::$trait<$other> for $self {
            type Output = $output;

            #[inline]
            fn $fn(self, other: $other) -> Self::Output {
                self.0.$fn(other)
            }
        }

        impl ::core::ops::$trait<$self> for $other {
            type Output = $output;

            #[inline]
            fn $fn(self, other: $self) -> Self::Output {
                self.$fn(other.0)
            }
        }
    };
}

macro_rules! impl_binop {
    ($trait:ident::$fn:ident for $name:ident: $prim:ty) => {
        impl_binop_both!($trait::$fn ($name, $prim) -> $prim);
        impl_binop_both!($trait::$fn (&'_ $name, $prim) -> $prim);
        impl_binop_both!($trait::$fn ($name, &'_ $prim) -> $prim);
        impl_binop_both!($trait::$fn (&'_ $name, &'_ $prim) -> $prim);

        impl_binop_one!($trait::$fn ($name, $name) -> $prim);
        impl_binop_one!($trait::$fn (&'_ $name, $name) -> $prim);
        impl_binop_one!($trait::$fn ($name, &'_ $name) -> $prim);
        impl_binop_one!($trait::$fn (&'_ $name, &'_ $name) -> $prim);
    };
}

macro_rules! impl_binassign {
    ($trait:ident:: $fn:ident for $name:ident : $prim:ty) => {
        impl ::core::ops::$trait<$prim> for $name {
            #[inline]
            fn $fn(&mut self, other: $prim) {
                let mut value = self.0;
                value.$fn(other);
                *self = Self(value);
            }
        }

        impl ::core::ops::$trait<$name> for $name {
            #[inline]
            fn $fn(&mut self, other: $name) {
                let mut value = self.0;
                value.$fn(other.0);
                *self = Self(value);
            }
        }

        impl ::core::ops::$trait<&'_ $prim> for $name {
            #[inline]
            fn $fn(&mut self, other: &'_ $prim) {
                let mut value = self.0;
                value.$fn(other);
                *self = Self(value);
            }
        }

        impl ::core::ops::$trait<&'_ $name> for $name {
            #[inline]
            fn $fn(&mut self, other: &'_ $name) {
                let mut value = self.0;
                value.$fn(other.0);
                *self = Self(value);
            }
        }
    };
}

macro_rules! impl_clone_and_copy {
    (for $name:ident) => {
        impl Clone for $name {
            #[inline]
            fn clone(&self) -> Self {
                *self
            }
        }

        impl Copy for $name {}
    };
}

macro_rules! impl_fmt {
    ($trait:ident for $name:ident) => {
        impl ::core::fmt::$trait for $name {
            #[inline]
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                ::core::fmt::$trait::fmt(&self.0, f)
            }
        }
    };
}

macro_rules! impl_default {
    (for $name:ident : $prim:ty) => {
        impl Default for $name {
            #[inline]
            fn default() -> Self {
                Self(<$prim>::default())
            }
        }
    };
}

macro_rules! impl_from {
    (for $name:ident : $prim:ty) => {
        impl From<$prim> for $name {
            fn from(value: $prim) -> Self {
                Self(value)
            }
        }

        impl<'a> From<&'a $prim> for $name {
            fn from(value: &'a $prim) -> Self {
                Self(*value)
            }
        }

        impl From<$name> for $prim {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl<'a> From<&'a $name> for $prim {
            fn from(value: &'a $name) -> Self {
                value.0
            }
        }
    };
}

macro_rules! impl_try_from_ptr_size {
    ($size:ident for $name:ident: $prim:ident) => {
        impl TryFrom<$size> for $name {
            type Error = <$prim as TryFrom<$size>>::Error;

            #[inline]
            fn try_from(value: $size) -> Result<Self, Self::Error> {
                Ok(Self(<$prim>::try_from(value)?))
            }
        }

        impl_try_into_ptr_size!($size for $name: $prim);
    };
}

macro_rules! impl_try_into_ptr_size {
    (isize for $name:ident: i16) => {
        impl_into_ptr_size!(isize for $name);
    };

    (usize for $name:ident: u16) => {
        impl_into_ptr_size!(usize for $name);
    };

    ($size:ident for $name:ident: $prim:ident) => {
        impl TryFrom<$name> for $size {
            type Error = <$size as TryFrom<$prim>>::Error;

            #[inline]
            fn try_from(value: $name) -> Result<Self, Self::Error> {
                <$size>::try_from(value.0)
            }
        }
    };
}

macro_rules! impl_into_ptr_size {
    ($size:ident for $name:ident) => {
        impl From<$name> for $size {
            #[inline]
            fn from(value: $name) -> Self {
                <$size>::from(value.0)
            }
        }
    };
}

macro_rules! impl_hash {
    (for $name:ident) => {
        impl core::hash::Hash for $name {
            fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
                self.0.hash(state);
            }
        }
    };
}

macro_rules! impl_partial_ord_and_ord {
    (for $name:ident : $prim:ty) => {
        impl PartialOrd for $name {
            #[inline]
            fn partial_cmp(&self, other: &Self) -> Option<::core::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl PartialOrd<$prim> for $name {
            #[inline]
            fn partial_cmp(&self, other: &$prim) -> Option<::core::cmp::Ordering> {
                self.0.partial_cmp(other)
            }
        }

        impl Ord for $name {
            #[inline]
            fn cmp(&self, other: &Self) -> ::core::cmp::Ordering {
                self.0.cmp(&other.0)
            }
        }
    };
}

macro_rules! impl_partial_eq_and_eq {
    (for $name:ident : $prim:ty) => {
        impl PartialEq for $name {
            #[inline]
            fn eq(&self, other: &Self) -> bool {
                let lhs = self.0;
                let rhs = other.0;
                lhs.eq(&rhs)
            }
        }

        impl PartialEq<$prim> for $name {
            #[inline]
            fn eq(&self, other: &$prim) -> bool {
                self.0.eq(other)
            }
        }

        impl PartialEq<$name> for $prim {
            #[inline]
            fn eq(&self, other: &$name) -> bool {
                self.eq(&other.0)
            }
        }

        impl Eq for $name {}
    };
}

macro_rules! impl_partial_ord {
    (for $name:ident : $prim:ty) => {
        impl PartialOrd for $name {
            #[inline]
            fn partial_cmp(&self, other: &Self) -> Option<::core::cmp::Ordering> {
                self.0.partial_cmp(&other.0)
            }
        }

        impl PartialOrd<$prim> for $name {
            #[inline]
            fn partial_cmp(&self, other: &$prim) -> Option<::core::cmp::Ordering> {
                self.0.partial_cmp(other)
            }
        }
    };
}

macro_rules! impl_product_and_sum {
    (for $name:ident) => {
        impl ::core::iter::Product for $name {
            #[inline]
            fn product<I: Iterator<Item = Self>>(iter: I) -> Self {
                Self(iter.map(|x| x.0).product())
            }
        }

        impl ::core::iter::Sum for $name {
            #[inline]
            fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
                Self(iter.map(|x| x.0).sum())
            }
        }
    };
}

macro_rules! impl_signed_integer_traits {
    ($name:ident: $prim:ident) => {
        impl_binop!(Add::add for $name: $prim);
        impl_binassign!(AddAssign::add_assign for $name: $prim);
        impl_clone_and_copy!(for $name);
        impl_fmt!(Binary for $name);
        impl_binop!(BitAnd::bitand for $name: $prim);
        impl_binassign!(BitAndAssign::bitand_assign for $name: $prim);
        impl_binop!(BitOr::bitor for $name: $prim);
        impl_binassign!(BitOrAssign::bitor_assign for $name: $prim);
        impl_binop!(BitXor::bitxor for $name: $prim);
        impl_binassign!(BitXorAssign::bitxor_assign for $name: $prim);
        impl_fmt!(Debug for $name);
        impl_default!(for $name: $prim);
        impl_fmt!(Display for $name);
        impl_binop!(Div::div for $name: $prim);
        impl_binassign!(DivAssign::div_assign for $name: $prim);
        impl_from!(for $name: $prim);
        impl_try_from_ptr_size!(isize for $name: $prim);
        impl_hash!(for $name);
        impl_fmt!(LowerExp for $name);
        impl_fmt!(LowerHex for $name);
        impl_binop!(Mul::mul for $name: $prim);
        impl_binassign!(MulAssign::mul_assign for $name: $prim);
        impl_unop!(Neg::neg for $name: $prim);
        impl_unop!(Not::not for $name: $prim);
        impl_fmt!(Octal for $name);
        impl_partial_eq_and_eq!(for $name: $prim);
        impl_partial_ord_and_ord!(for $name: $prim);
        impl_product_and_sum!(for $name);
        impl_binop!(Rem::rem for $name: $prim);
        impl_binassign!(RemAssign::rem_assign for $name: $prim);
        impl_binop!(Shl::shl for $name: $prim);
        impl_binassign!(ShlAssign::shl_assign for $name: $prim);
        impl_binop!(Shr::shr for $name: $prim);
        impl_binassign!(ShrAssign::shr_assign for $name: $prim);
        impl_binop!(Sub::sub for $name: $prim);
        impl_binassign!(SubAssign::sub_assign for $name: $prim);
        impl_fmt!(UpperExp for $name);
        impl_fmt!(UpperHex for $name);
    };
}

macro_rules! impl_unsigned_integer_traits {
    ($name:ident: $prim:ident) => {
        impl_binop!(Add::add for $name: $prim);
        impl_binassign!(AddAssign::add_assign for $name: $prim);
        impl_clone_and_copy!(for $name);
        impl_fmt!(Binary for $name);
        impl_binop!(BitAnd::bitand for $name: $prim);
        impl_binassign!(BitAndAssign::bitand_assign for $name: $prim);
        impl_binop!(BitOr::bitor for $name: $prim);
        impl_binassign!(BitOrAssign::bitor_assign for $name: $prim);
        impl_binop!(BitXor::bitxor for $name: $prim);
        impl_binassign!(BitXorAssign::bitxor_assign for $name: $prim);
        impl_fmt!(Debug for $name);
        impl_default!(for $name: $prim);
        impl_fmt!(Display for $name);
        impl_binop!(Div::div for $name: $prim);
        impl_binassign!(DivAssign::div_assign for $name: $prim);
        impl_from!(for $name: $prim);
        impl_try_from_ptr_size!(usize for $name: $prim);
        impl_hash!(for $name);
        impl_fmt!(LowerExp for $name);
        impl_fmt!(LowerHex for $name);
        impl_binop!(Mul::mul for $name: $prim);
        impl_binassign!(MulAssign::mul_assign for $name: $prim);
        impl_unop!(Not::not for $name: $prim);
        impl_fmt!(Octal for $name);
        impl_partial_eq_and_eq!(for $name: $prim);
        impl_partial_ord_and_ord!(for $name: $prim);
        impl_product_and_sum!(for $name);
        impl_binop!(Rem::rem for $name: $prim);
        impl_binassign!(RemAssign::rem_assign for $name: $prim);
        impl_binop!(Shl::shl for $name: $prim);
        impl_binassign!(ShlAssign::shl_assign for $name: $prim);
        impl_binop!(Shr::shr for $name: $prim);
        impl_binassign!(ShrAssign::shr_assign for $name: $prim);
        impl_binop!(Sub::sub for $name: $prim);
        impl_binassign!(SubAssign::sub_assign for $name: $prim);
        impl_fmt!(UpperExp for $name);
        impl_fmt!(UpperHex for $name);
    };
}

macro_rules! impl_float_traits {
    ($name:ident: $prim:ty) => {
        impl_binop!(Add::add for $name: $prim);
        impl_binassign!(AddAssign::add_assign for $name: $prim);
        impl_clone_and_copy!(for $name);
        impl_fmt!(Debug for $name);
        impl_default!(for $name: $prim);
        impl_fmt!(Display for $name);
        impl_binop!(Div::div for $name: $prim);
        impl_binassign!(DivAssign::div_assign for $name: $prim);
        impl_from!(for $name: $prim);
        impl_fmt!(LowerExp for $name);
        impl_binop!(Mul::mul for $name: $prim);
        impl_binassign!(MulAssign::mul_assign for $name: $prim);
        impl_unop!(Neg::neg for $name: $prim);
        impl_partial_eq_and_eq!(for $name: $prim);
        impl_partial_ord!(for $name: $prim);
        impl_product_and_sum!(for $name);
        impl_binop!(Rem::rem for $name: $prim);
        impl_binassign!(RemAssign::rem_assign for $name: $prim);
        impl_binop!(Sub::sub for $name: $prim);
        impl_binassign!(SubAssign::sub_assign for $name: $prim);
        impl_fmt!(UpperExp for $name);
    };
}

macro_rules! define_newtype {
    ($name:ident: $prim:ty, $align:expr) => {
        #[doc = concat!("A wire-encoded `", stringify!($prim), "`")]
        #[repr(C, align($align))]
        #[derive(zerocopy::FromBytes, zerocopy::IntoBytes)]
        pub struct $name(pub $prim);

        impl ::core::ops::Deref for $name {
            type Target = $prim;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl ::core::ops::DerefMut for $name {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }
    };
}

macro_rules! define_signed_integer {
    ($name:ident: $prim:ident, $align:expr) => {
        define_newtype!($name: $prim, $align);
        impl_signed_integer_traits!($name: $prim);
    }
}

define_signed_integer!(WireI16: i16, 2);
define_signed_integer!(WireI32: i32, 4);
define_signed_integer!(WireI64: i64, 8);

macro_rules! define_unsigned_integer {
    ($name:ident: $prim:ident, $align:expr) => {
        define_newtype!($name: $prim, $align);
        impl_unsigned_integer_traits!($name: $prim);
    }
}

define_unsigned_integer!(WireU16: u16, 2);
define_unsigned_integer!(WireU32: u32, 4);
define_unsigned_integer!(WireU64: u64, 8);

macro_rules! define_float {
    ($name:ident: $prim:ident, $align:expr) => {
        define_newtype!($name: $prim, $align);
        impl_float_traits!($name: $prim);
    }
}

define_float!(WireF32: f32, 4);
define_float!(WireF64: f64, 8);
