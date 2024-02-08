// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// A pair of core and bindings contexts.
///
/// This trait exists so implementers can be agnostic on the storage of the
/// contexts, since all we need from a context pair is mutable references from
/// both contexts.
pub trait ContextPair {
    type CoreContext;
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
