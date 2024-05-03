// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common context abstractions.

/// A pair of core and bindings contexts.
///
/// This trait exists so implementers can be agnostic on the storage of the
/// contexts, since all we need from a context pair is mutable references from
/// both contexts.
pub trait ContextPair {
    /// The core context type held by this pair.
    type CoreContext;
    /// The bindings context type held by this pair.
    type BindingsContext;

    /// Gets a mutable reference to both contexts.
    fn contexts(&mut self) -> (&mut Self::CoreContext, &mut Self::BindingsContext);

    /// Gets a mutable reference to the core context.
    fn core_ctx(&mut self) -> &mut Self::CoreContext {
        let (core_ctx, _) = self.contexts();
        core_ctx
    }

    /// Gets a mutable reference to the bindings context.
    fn bindings_ctx(&mut self) -> &mut Self::BindingsContext {
        let (_, bindings_ctx) = self.contexts();
        bindings_ctx
    }
}

impl<'a, C> ContextPair for &'a mut C
where
    C: ContextPair,
{
    type CoreContext = C::CoreContext;
    type BindingsContext = C::BindingsContext;
    fn contexts(&mut self) -> (&mut Self::CoreContext, &mut Self::BindingsContext) {
        C::contexts(self)
    }
}

/// A marker trait indicating that the implementor is not a test context.
///
/// This trait allows us to sidestep some blanket `Handler` implementations that
/// are provided by core types and overridden in specific test contexts.
///
/// See [this issue] for details on why this is needed in some other cases.
///
/// [this issue]: https://github.com/rust-lang/rust/issues/97811
pub trait NonTestCtxMarker {}
