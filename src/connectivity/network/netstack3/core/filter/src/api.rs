// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use net_types::ip::{Ipv4, Ipv6};
use netstack3_base::ContextPair;

use crate::{FilterBindingsTypes, FilterContext, State, ValidState, ValidationError};

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
    /// # Panics
    ///
    /// Panics if the provided state includes cyclic routine graphs.
    pub fn set_filter_state<RuleInfo: Clone>(
        &mut self,
        v4: State<Ipv4, <C::BindingsContext as FilterBindingsTypes>::DeviceClass, RuleInfo>,
        v6: State<Ipv6, <C::BindingsContext as FilterBindingsTypes>::DeviceClass, RuleInfo>,
    ) -> Result<(), ValidationError<RuleInfo>> {
        let v4 = ValidState::new(v4)?;
        let v6 = ValidState::new(v6)?;

        self.core_ctx().with_all_filter_state_mut(|state_v4, state_v6| {
            *state_v4 = v4;
            *state_v6 = v6;
        });

        Ok(())
    }
}
