// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities which allow code to be more robust to changes in dependencies.
//!
//! The utilities in this crate allow code to depend on details of its
//! dependencies which would not normally be captured by code using the
//! canonical Rust style. See [this document][rust-patterns] for a discussion of
//! when and why this might be desirable.
//!
//! [rust-patterns]: https://fuchsia.dev/fuchsia-src/contribute/contributing-to-netstack/rust-patterns

#![no_std]
#![deny(missing_docs)]

use core::convert::Infallible as Never;

/// An extension trait adding functionality to [`Result`].
pub trait ResultExt<T, E> {
    /// Like [`Result::ok`], but the caller must provide the error type being
    /// discarded.
    ///
    /// This allows code to be written which will stop compiling if a result's
    /// error type changes in the future.
    fn ok_checked<EE: sealed::EqType<E>>(self) -> Option<T>;
}

impl<T, E> ResultExt<T, E> for Result<T, E> {
    fn ok_checked<EE: sealed::EqType<E>>(self) -> Option<T> {
        Result::ok(self)
    }
}

/// An extension trait adding functionality to [`Poll`].
pub trait PollExt<T> {
    /// Like [`Poll::is_ready`], but the caller must provide the inner type.
    ///
    /// This allows both the authors and the reviewers to check if information
    /// is being discarded unnoticed.
    fn is_ready_checked<TT: sealed::EqType<T>>(&self) -> bool;
}

impl<T> PollExt<T> for core::task::Poll<T> {
    fn is_ready_checked<TT: sealed::EqType<T>>(&self) -> bool {
        core::task::Poll::is_ready(self)
    }
}

/// A trait providing unreachability assertion enforced by the type system.
pub trait UnreachableExt: sealed::Sealed {
    /// A method that can't be called.
    ///
    /// This method returns an instance of any caller-specified type, which
    /// makes it impossible to implement unless the method receiver is itself
    /// uninstantiable. This method is similar to the `unreachable!` macro, but
    /// should be preferred over the macro since it uses the type system to
    /// enforce unreachability where `unreachable!` indicates a logical
    /// assertion checked at runtime.
    fn uninstantiable_unreachable<T>(&self) -> T;
}

impl<N: AsRef<Never>> UnreachableExt for N {
    fn uninstantiable_unreachable<T>(&self) -> T {
        match *self.as_ref() {}
    }
}

mod sealed {
    use core::convert::Infallible as Never;

    /// `EqType<T>` indicates that the implementer is equal to `T`.
    ///
    /// For all `T`, `T: EqType<T>`. For all distinct `T` and `U`, `T:
    /// !EqType<U>`.
    pub trait EqType<T> {}

    impl<T> EqType<T> for T {}

    /// Trait that can only be implemented within this crate.
    pub trait Sealed {}

    impl<T: AsRef<Never>> Sealed for T {}
}
