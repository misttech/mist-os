// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! General utilities for converting types.

use core::{convert::Infallible as Never, marker::PhantomData};

use explicit::UnreachableExt as _;

/// Produces an owned value `T` from Self.
///
/// This trait is useful for implementing functions that take a value that may
/// or may not end up needing to be consumed, but where the caller doesn't
/// necessarily want or need to keep the value either.
///
/// This trait is blanket-implemented for `T` as a pass-through and for `&T`
/// where `T: Clone` by cloning `self`.
pub(crate) trait OwnedOrCloned<T> {
    fn into_owned(self) -> T;
}

impl<T> OwnedOrCloned<T> for T {
    fn into_owned(self) -> T {
        self
    }
}

impl<'s, T: Clone> OwnedOrCloned<T> for &'s T {
    fn into_owned(self) -> T {
        self.clone()
    }
}

/// Provides functions for converting infallibly between types.
///
/// This trait can be implemented on types that allow converting between two
/// related types. It has two blanket implementations: `()` for identity
/// conversions, i.e. `Input=Output`, and [`UninstantiableConverter`] as an
/// uninstantiable type that implements the trait for any input and output.
pub(crate) trait BidirectionalConverter<Input> {
    /// The output type for the given `Input` type.
    ///
    /// An instance of this type is produced by `convert` and consumed by
    /// `convert_back`.
    type Output;

    /// Converts an instance of `Input` into an instance of `Output`.
    fn convert(&self, a: Input) -> Self::Output;

    /// Converts an instance of `Output` into an instance of `Input`.
    fn convert_back(&self, b: Self::Output) -> Input;
}

impl<I> BidirectionalConverter<I> for () {
    type Output = I;
    fn convert_back(&self, value: I) -> Self::Output {
        value
    }
    fn convert(&self, value: I) -> Self::Output {
        value
    }
}

pub(crate) struct UninstantiableConverter<Output>(Never, PhantomData<Output>);

impl<Output> AsRef<Never> for UninstantiableConverter<Output> {
    fn as_ref(&self) -> &Never {
        &self.0
    }
}

impl<I, O> BidirectionalConverter<I> for UninstantiableConverter<O> {
    type Output = O;
    fn convert_back(&self, _: O) -> I {
        self.uninstantiable_unreachable()
    }
    fn convert(&self, _: I) -> O {
        self.uninstantiable_unreachable()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    struct Uncloneable(u8);

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct Cloneable(u16);

    #[test]
    fn owned_as_owned_or_cloned() {
        assert_eq!(Uncloneable(45).into_owned(), Uncloneable(45));
    }

    #[test]
    fn reference_as_owned_or_cloned() {
        let cloneable = Cloneable(32);
        let reference = &cloneable;
        let cloned: Cloneable = reference.into_owned();
        assert_eq!(cloned, cloneable);
    }

    #[test]
    fn bidirectional_converter_identity() {
        let a = ();
        assert_eq!(a.convert(123), 123);
        assert_eq!(a.convert_back(123), 123);
    }
}
