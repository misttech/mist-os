// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! IP routing rules.

use alloc::vec::Vec;
use core::fmt::Debug;

use net_types::ip::Ip;

use crate::RoutingTableId;

/// Table that contains routing rules.
pub struct RulesTable<I: Ip, D> {
    rules: Vec<Rule<I, D>>,
}

impl<I: Ip, D> RulesTable<I, D> {
    pub(crate) fn new(main_table_id: RoutingTableId<I, D>) -> Self {
        // TODO(https://fxbug.dev/355059790): If bindings is installing the main table, we should
        // also let the bindings install this default rule.
        Self { rules: alloc::vec![Rule { action: RuleAction::Lookup(main_table_id) }] }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &'_ Rule<I, D>> {
        self.rules.iter()
    }

    #[cfg(test)]
    pub(crate) fn rules_mut(&mut self) -> &mut Vec<Rule<I, D>> {
        &mut self.rules
    }
}

/// The action part of the routing rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuleAction<Lookup> {
    /// Will resolve to unreachable.
    // TODO(https://fxbug.dev/357858471): Install Bindings rules in Core.
    #[allow(unused)]
    Unreachable,
    /// Lookup in a routing table.
    Lookup(Lookup),
}

/// A routing rule.
pub(crate) struct Rule<I: Ip, D> {
    // TODO(https://fxbug.dev/354724171): Rules need selectors.
    pub(crate) action: RuleAction<RoutingTableId<I, D>>,
}
