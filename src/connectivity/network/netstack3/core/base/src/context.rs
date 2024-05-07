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

/// A type that provides a context implementation.
///
/// This trait allows for [`CtxPair`] to hold context implementations
/// agnostically of the storage method and how they're implemented. For example,
/// tests usually create API structs with a mutable borrow to contexts, while
/// the core context exposed to bindings is implemented on an owned [`CoreCtx`]
/// type.
///
/// The shape of this trait is equivalent to [`core::ops::DerefMut`] but we
/// decide against using that because of the automatic dereferencing semantics
/// the compiler provides around implementers of `DerefMut`.
pub trait ContextProvider {
    /// The context provided by this `ContextProvider`.
    type Context: Sized;

    /// Gets a mutable borrow to this context.
    fn context(&mut self) -> &mut Self::Context;
}

impl<'a, T: Sized> ContextProvider for &'a mut T {
    type Context = T;

    fn context(&mut self) -> &mut Self::Context {
        &mut *self
    }
}

/// A concrete implementation of [`ContextPair`].
///
///
/// `CtxPair` provides a [`ContextPair`] implementation when `CC` and `BC` are
/// [`ContextProvider`] and using their respective targets as the `CoreContext`
/// and `BindingsContext` associated types.
pub struct CtxPair<CC, BC> {
    /// The core context.
    pub core_ctx: CC,
    /// The bindings context.
    pub bindings_ctx: BC,
}

impl<CC, BC> ContextPair for CtxPair<CC, BC>
where
    CC: ContextProvider,
    BC: ContextProvider,
{
    type CoreContext = CC::Context;
    type BindingsContext = BC::Context;

    fn contexts(&mut self) -> (&mut Self::CoreContext, &mut Self::BindingsContext) {
        let Self { core_ctx, bindings_ctx } = self;
        (core_ctx.context(), bindings_ctx.context())
    }
}
