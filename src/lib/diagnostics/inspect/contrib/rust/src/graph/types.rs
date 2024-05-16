// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect as inspect;
use std::{borrow::Cow, marker::PhantomData};

pub trait GraphObject {
    type Id;
    fn write_to_node(node: &inspect::Node, id: &Self::Id);
}

#[derive(Debug)]
pub struct VertexMarker<T>(PhantomData<T>);
#[derive(Debug)]
pub struct EdgeMarker;

impl GraphObject for EdgeMarker {
    type Id = u64;

    fn write_to_node(node: &inspect::Node, id: &Self::Id) {
        node.record_uint("edge_id", *id);
    }
}

impl<T: VertexId> GraphObject for VertexMarker<T> {
    type Id = T;

    fn write_to_node(node: &inspect::Node, id: &Self::Id) {
        node.record_string("vertex_id", id.get_id().as_ref());
    }
}

/// The ID of a vertex.
pub trait VertexId {
    /// Fetches the ID of a vertex, which must have a string representation.
    fn get_id(&self) -> Cow<'_, str>;
}

impl<T: std::fmt::Display> VertexId for T {
    fn get_id(&self) -> Cow<'_, str> {
        Cow::Owned(format!("{self}"))
    }
}

impl VertexId for str {
    fn get_id(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
}
