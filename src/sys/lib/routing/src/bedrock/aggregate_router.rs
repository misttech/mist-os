// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::capability_source::AggregateInstance;
use cm_types::Name;
use sandbox::{DirEntry, Router};

/// An `AggregateSource` describes the source of one (or more) service capabilities whose instances
/// will be added to an aggregated service.
#[derive(Debug, Clone)]
pub enum AggregateSource {
    /// A router to a single service capability provider, whose published instances will be part of
    /// an aggregate.
    DirectoryRouter {
        /// Where the router comes from, be it a parent, child, etc.
        source_instance: AggregateInstance,
        /// The router that will back this source to the aggregate.
        router: Router<DirEntry>,
    },
    /// A collection whose dynamically created components may contribute to an aggregate.
    Collection { collection_name: Name },
}
