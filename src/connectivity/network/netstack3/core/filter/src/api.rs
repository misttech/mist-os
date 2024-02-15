// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use net_types::ip::{Ipv4, Ipv6};
use netstack3_base::ContextPair;

use crate::{
    FilterBindingsTypes, FilterContext, State, ValidState, ValidationError, ValidationInfo,
};

/// The filtering API.
pub struct FilterApi<C>(C);

impl<C> FilterApi<C> {
    /// Creates a new `FilterApi` from a context pair.
    pub fn new(ctx: C) -> Self {
        Self(ctx)
    }
}

impl<C> FilterApi<C>
where
    C: ContextPair,
    C::CoreContext: FilterContext<C::BindingsContext>,
    C::BindingsContext: FilterBindingsTypes,
{
    fn core_ctx(&mut self) -> &mut C::CoreContext {
        let Self(pair) = self;
        pair.core_ctx()
    }

    /// Sets the filtering state for the provided IP version.
    ///
    /// The provided state must not contain any cyclical routine graphs (formed by
    /// rules with jump actions). The behavior in this case is unspecified but could
    /// be a deadlock or a panic, for example.
    ///
    /// TODO(https://fxbug.dev/325492760): replace usage of
    /// [`once_cell::sync::OnceCell`] with `std::sync::OnceLock`, which always
    /// panics when called reentrantly.
    pub fn set_filter_state<RuleInfo: Clone>(
        &mut self,
        v4: State<
            Ipv4,
            <C::BindingsContext as FilterBindingsTypes>::DeviceClass,
            ValidationInfo<RuleInfo>,
        >,
        v6: State<
            Ipv6,
            <C::BindingsContext as FilterBindingsTypes>::DeviceClass,
            ValidationInfo<RuleInfo>,
        >,
    ) -> Result<(), ValidationError<RuleInfo>>
    where
        <C::BindingsContext as FilterBindingsTypes>::DeviceClass: Clone,
    {
        let v4 = ValidState::new(v4)?;
        let v6 = ValidState::new(v6)?;

        self.core_ctx().with_all_filter_state_mut(|state_v4, state_v6| {
            *state_v4 = v4;
            *state_v6 = v6;
        });

        Ok(())
    }
}
