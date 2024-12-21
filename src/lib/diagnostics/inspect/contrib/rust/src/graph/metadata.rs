// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::events::{GraphObjectEventTracker, MetaEventNode};
use super::types::{EdgeMarker, GraphObject, VertexMarker};
use super::VertexId;
use fuchsia_inspect::{self as inspect, ArrayProperty, Property};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Deref;

/// An enum encoding all the possible metadata value types and contents.
#[derive(Debug)]
pub enum MetadataValue<'a> {
    Int(i64),
    Uint(u64),
    Double(f64),
    Str(Cow<'a, str>),
    Bool(bool),
    IntVec(Vec<i64>),
    UintVec(Vec<u64>),
    DoubleVec(Vec<f64>),
}

impl MetadataValue<'_> {
    pub(crate) fn record_inspect(&self, node: &inspect::Node, key: &str) {
        match self {
            Self::Int(value) => node.record_int(key, *value),
            Self::Uint(value) => node.record_uint(key, *value),
            Self::Double(value) => node.record_double(key, *value),
            Self::Str(value) => node.record_string(key, value.as_ref()),
            Self::Bool(value) => node.record_bool(key, *value),
            Self::IntVec(value) => {
                node.atomic_update(|node| {
                    let prop = node.create_int_array(key, value.len());
                    for (idx, v) in value.iter().enumerate() {
                        prop.set(idx, *v);
                    }
                    node.record(prop);
                });
            }
            Self::UintVec(value) => {
                node.atomic_update(|node| {
                    let prop = node.create_uint_array(key, value.len());
                    for (idx, v) in value.iter().enumerate() {
                        prop.set(idx, *v);
                    }
                    node.record(prop);
                });
            }
            Self::DoubleVec(value) => {
                node.atomic_update(|node| {
                    let prop = node.create_double_array(key, value.len());
                    for (idx, v) in value.iter().enumerate() {
                        prop.set(idx, *v);
                    }
                    node.record(prop);
                });
            }
        }
    }
}

impl<'a, T> From<&'a T> for MetadataValue<'a>
where
    T: VertexId,
{
    fn from(value: &'a T) -> MetadataValue<'a> {
        Self::Str(value.get_id())
    }
}

macro_rules! impl_from_for_metadata_value {
    ($([($($type:ty),*), $name:ident, $cast_type:ty]),*) => {
        $($(
            impl From<$type> for MetadataValue<'_> {
                fn from(value: $type) -> MetadataValue<'static> {
                    MetadataValue::$name(value as $cast_type)
                }
            }
        )*)*
    };
}

impl_from_for_metadata_value!(
    [(i8, i16, i32, i64), Int, i64],
    [(u8, u16, u32, u64), Uint, u64],
    [(f32, f64), Double, f64]
);

impl From<bool> for MetadataValue<'_> {
    fn from(value: bool) -> MetadataValue<'static> {
        MetadataValue::Bool(value)
    }
}

impl<'a> From<&'a str> for MetadataValue<'a> {
    fn from(value: &'a str) -> MetadataValue<'a> {
        MetadataValue::Str(Cow::Borrowed(value))
    }
}

impl From<String> for MetadataValue<'_> {
    fn from(value: String) -> MetadataValue<'static> {
        MetadataValue::Str(Cow::Owned(value))
    }
}

impl<'a> From<Cow<'a, str>> for MetadataValue<'a> {
    fn from(value: Cow<'a, str>) -> MetadataValue<'a> {
        MetadataValue::Str(value)
    }
}

impl<'a> From<Vec<i64>> for MetadataValue<'a> {
    fn from(value: Vec<i64>) -> MetadataValue<'a> {
        MetadataValue::IntVec(value)
    }
}

impl<'a> From<Vec<u8>> for MetadataValue<'a> {
    fn from(value: Vec<u8>) -> MetadataValue<'a> {
        MetadataValue::UintVec(value.into_iter().map(|v| v as u64).collect())
    }
}

impl<'a> From<Vec<u64>> for MetadataValue<'a> {
    fn from(value: Vec<u64>) -> MetadataValue<'a> {
        MetadataValue::UintVec(value)
    }
}

impl<'a> From<Vec<f64>> for MetadataValue<'a> {
    fn from(value: Vec<f64>) -> MetadataValue<'a> {
        MetadataValue::DoubleVec(value)
    }
}

/// A metadata item used to initialize metadata key value pairs of nodes and edges.
pub struct Metadata<'a> {
    /// The key of the metadata field.
    pub(crate) key: Cow<'a, str>,
    /// The value of the metadata field.
    inner: InnerMetadata<'a>,
}

impl<'a> Metadata<'a> {
    /// Create a new metadata item with the given `key` and `value`.
    pub fn new(key: impl Into<Cow<'a, str>>, value: impl Into<MetadataValue<'a>>) -> Self {
        Self {
            key: key.into(),
            inner: InnerMetadata::Value { value: value.into(), track_events: false },
        }
    }

    pub fn nested(
        key: impl Into<Cow<'a, str>>,
        value: impl IntoIterator<Item = Metadata<'a>> + 'a,
    ) -> Self {
        Self { key: key.into(), inner: InnerMetadata::Nested(Box::new(value.into_iter())) }
    }

    /// Require that changes to this metadata node must be tracked. To support tracking events
    /// about changes the graph must be configured to support tracking events.
    /// This is a no-op if called on a nested node. It only works on leaf properties.
    pub fn track_events(mut self) -> Self {
        match &mut self.inner {
            InnerMetadata::Value { ref mut track_events, .. } => *track_events = true,
            InnerMetadata::Nested(_) => {}
        }
        self
    }
}

enum InnerMetadata<'a> {
    Value {
        value: MetadataValue<'a>,
        // Whether or not changes to this metadata field should be tracked.
        track_events: bool,
    },
    Nested(Box<dyn Iterator<Item = Metadata<'a>> + 'a>),
}

#[derive(Debug)]
pub struct VertexGraphMetadata<I>
where
    I: VertexId,
{
    inner: GraphMetadata<VertexMarker<I>>,
}

impl<I: VertexId> VertexGraphMetadata<I> {
    /// Set the value of the metadata field at `key` or insert a new one if not
    /// present.
    pub fn set<'a, 'b>(
        &mut self,
        key: impl Into<Cow<'a, str>>,
        value: impl Into<MetadataValue<'b>>,
    ) {
        self.inner.set(key, value, None);
    }

    pub fn set_and_track<'a, 'b>(
        &mut self,
        key: impl Into<Cow<'a, str>>,
        value: impl Into<MetadataValue<'b>>,
    ) {
        self.inner.set(key, value, Some(true));
    }

    /// Remove the metadata field at `key`.
    pub fn remove(&mut self, key: &str) {
        self.inner.remove(key);
    }

    pub fn remove_and_track(&mut self, key: &str) {
        self.inner.remove_and_track(key);
    }

    pub(crate) fn new<'a>(
        parent: &inspect::Node,
        id: I,
        initial_metadata: impl Iterator<Item = Metadata<'a>>,
        events_tracker: Option<GraphObjectEventTracker<VertexMarker<I>>>,
    ) -> Self {
        let (inner, meta_event_node) =
            GraphMetadata::new(parent, id, initial_metadata, events_tracker);
        if let Some(ref events_tracker) = inner.events_tracker {
            events_tracker.record_added(&inner.id, meta_event_node);
        }
        Self { inner }
    }

    pub(crate) fn events_tracker(&self) -> Option<&GraphObjectEventTracker<VertexMarker<I>>> {
        self.inner.events_tracker.as_ref()
    }

    pub(crate) fn id(&self) -> &I {
        &self.inner.id
    }
}

#[derive(Debug)]
pub struct EdgeGraphMetadata {
    inner: GraphMetadata<EdgeMarker>,
}

impl EdgeGraphMetadata {
    /// Set the value of the metadata field at `key` or insert a new one if not
    /// present.
    pub fn set<'a, 'b>(
        &mut self,
        key: impl Into<Cow<'a, str>>,
        value: impl Into<MetadataValue<'b>>,
    ) {
        self.inner.set(key, value, None);
    }

    /// Remove the metadata field at `key`.
    pub fn remove(&mut self, key: &str) {
        self.inner.remove(key);
    }

    pub(crate) fn new<'a>(
        parent: &inspect::Node,
        id: u64,
        initial_metadata: impl Iterator<Item = Metadata<'a>>,
        events_tracker: Option<GraphObjectEventTracker<EdgeMarker>>,
        from_id: &str,
        to_id: &str,
    ) -> Self {
        let (inner, meta_event_node) =
            GraphMetadata::new(parent, id, initial_metadata, events_tracker);
        if let Some(ref events_tracker) = inner.events_tracker {
            events_tracker.record_added(from_id, to_id, id, meta_event_node);
        }
        Self { inner }
    }

    pub(crate) fn events_tracker(&self) -> Option<&GraphObjectEventTracker<EdgeMarker>> {
        self.inner.events_tracker.as_ref()
    }

    pub(crate) fn id(&self) -> u64 {
        self.inner.id
    }

    pub(crate) fn noop(id: <EdgeMarker as GraphObject>::Id) -> EdgeGraphMetadata {
        Self {
            inner: GraphMetadata {
                id,
                map: BTreeMap::default(),
                events_tracker: None,
                node: inspect::Node::default(),
            },
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // TODO(https://fxbug.dev/338660036)
enum MetadataProperty {
    Int(inspect::IntProperty),
    Uint(inspect::UintProperty),
    Double(inspect::DoubleProperty),
    Str(inspect::StringProperty),
    Bool(inspect::BoolProperty),
    IntVec(inspect::IntArrayProperty),
    UintVec(inspect::UintArrayProperty),
    DoubleVec(inspect::DoubleArrayProperty),
}

/// The metadata of a vertex or edge in the graph.
#[derive(Debug)]
struct GraphMetadata<T: GraphObject> {
    id: T::Id,
    map: BTreeMap<String, (MetadataProperty, bool)>,
    events_tracker: Option<GraphObjectEventTracker<T>>,
    node: inspect::Node,
}

impl<T> GraphMetadata<T>
where
    T: GraphObject,
{
    fn new<'a>(
        parent: &inspect::Node,
        id: T::Id,
        initial_metadata: impl Iterator<Item = Metadata<'a>>,
        events_tracker: Option<GraphObjectEventTracker<T>>,
    ) -> (Self, MetaEventNode) {
        let node = parent.create_child("meta");
        let meta_event_node = MetaEventNode::new(parent);

        let mut map = BTreeMap::default();
        node.atomic_update(|node| {
            for Metadata { key, inner } in initial_metadata.into_iter() {
                match inner {
                    InnerMetadata::Nested(_) => {
                        Self::insert_nested_to_map(
                            node,
                            meta_event_node.deref(),
                            &mut vec![],
                            &mut map,
                            Metadata { key, inner },
                        );
                    }
                    InnerMetadata::Value { value, track_events } => {
                        let key = key.to_string();
                        if events_tracker.is_some() && track_events {
                            value.record_inspect(&meta_event_node, &key);
                        }
                        Self::insert_to_map(node, &mut map, &[key.into()], value, track_events)
                    }
                }
            }
        });
        (Self { id, node, map, events_tracker }, meta_event_node)
    }

    fn set<'a, 'b>(
        &mut self,
        key: impl Into<Cow<'a, str>>,
        value: impl Into<MetadataValue<'b>>,
        maybe_track: Option<bool>,
    ) {
        let key: Cow<'a, str> = key.into();
        // Sometimes, dynamically-created properties need tracking.
        if let Some(choice) = maybe_track {
            if let Some((_, ref mut track_events)) = self.map.get_mut(key.as_ref()) {
                if *track_events != choice {
                    *track_events = choice;
                }
            }
        }
        let value = value.into();

        // Turn this log line on to understand if setting a property had issues.
        log::debug!("trying to set key={:?} to value={:?}", &key, &value);
        match (self.map.get(key.as_ref()), value) {
            (
                Some((MetadataProperty::Int(property), track_events)),
                value @ MetadataValue::Int(x),
            ) => {
                property.set(x);
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
            }
            (
                Some((MetadataProperty::Uint(property), track_events)),
                value @ MetadataValue::Uint(x),
            ) => {
                property.set(x);
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
            }
            (
                Some((MetadataProperty::Double(property), track_events)),
                value @ MetadataValue::Double(x),
            ) => {
                property.set(x);
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
            }
            (
                Some((MetadataProperty::Bool(property), track_events)),
                value @ MetadataValue::Bool(x),
            ) => {
                property.set(x);
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
            }
            (
                Some((MetadataProperty::Str(property), track_events)),
                ref value @ MetadataValue::Str(ref x),
            ) => {
                property.set(x);
                if *track_events {
                    self.log_update_key(key.as_ref(), value)
                }
            }
            (Some((MetadataProperty::IntVec(_), _)), MetadataValue::IntVec(_)) => {
                // TODO(https://fxbug.dev/338660036): implement later.
            }
            (Some((MetadataProperty::UintVec(_), _)), MetadataValue::UintVec(_)) => {
                // TODO(https://fxbug.dev/338660036): implement later.
            }
            (Some((MetadataProperty::DoubleVec(_), _)), MetadataValue::DoubleVec(_)) => {
                // TODO(https://fxbug.dev/338660036): implement later.
            }
            (Some((_, track_events)), value) => {
                let track_events = *track_events; // drops ownership
                if track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
                Self::insert_to_map(&self.node, &mut self.map, &[key], value, track_events);
            }
            (None, value) => {
                let track_events = maybe_track.unwrap_or(false);
                if track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
                Self::insert_to_map(&self.node, &mut self.map, &[key], value, track_events);
            }
        }
    }

    fn remove(&mut self, key: &str) {
        self.map.remove(key);
    }

    fn remove_and_track(&mut self, key: &str) {
        if self.map.remove(key).is_some() {
            if let Some(events_tracker) = &self.events_tracker {
                events_tracker.metadata_dropped(&self.id, key);
            }
        }
    }

    fn log_update_key(&self, key: &str, value: &MetadataValue<'_>) {
        if let Some(events_tracker) = &self.events_tracker {
            events_tracker.metadata_updated(&self.id, key, value);
        }
    }

    fn insert_to_map(
        node: &inspect::Node,
        map: &mut BTreeMap<String, (MetadataProperty, bool)>,
        key_path: &[Cow<'_, str>],
        value: MetadataValue<'_>,
        track_events: bool,
    ) {
        let node_key = key_path.last().unwrap().as_ref();
        let map_key = key_path.join("/");
        match value {
            MetadataValue::Int(value) => {
                let property = node.create_int(node_key, value);
                map.insert(map_key, (MetadataProperty::Int(property), track_events));
            }
            MetadataValue::Uint(value) => {
                let property = node.create_uint(node_key, value);
                map.insert(map_key, (MetadataProperty::Uint(property), track_events));
            }
            MetadataValue::Double(value) => {
                let property = node.create_double(node_key, value);
                map.insert(map_key, (MetadataProperty::Double(property), track_events));
            }
            MetadataValue::Str(value) => {
                let property = node.create_string(node_key, value);
                map.insert(map_key, (MetadataProperty::Str(property), track_events));
            }
            MetadataValue::Bool(value) => {
                let property = node.create_bool(node_key, value);
                map.insert(map_key, (MetadataProperty::Bool(property), track_events));
            }
            MetadataValue::IntVec(value) => {
                let property = node.atomic_update(|node| {
                    let property = node.create_int_array(node_key, value.len());
                    for (idx, v) in value.into_iter().enumerate() {
                        property.set(idx, v);
                    }
                    property
                });
                map.insert(map_key, (MetadataProperty::IntVec(property), track_events));
            }
            MetadataValue::UintVec(value) => {
                let property = node.atomic_update(|node| {
                    let property = node.create_uint_array(node_key, value.len());
                    for (idx, v) in value.into_iter().enumerate() {
                        property.set(idx, v);
                    }
                    property
                });
                map.insert(map_key, (MetadataProperty::UintVec(property), track_events));
            }
            MetadataValue::DoubleVec(value) => {
                let property = node.atomic_update(|node| {
                    let property = node.create_double_array(node_key, value.len());
                    for (idx, v) in value.into_iter().enumerate() {
                        property.set(idx, v);
                    }
                    property
                });
                map.insert(map_key, (MetadataProperty::DoubleVec(property), track_events));
            }
        }
    }

    fn insert_nested_to_map<'a>(
        parent: &inspect::Node,
        parent_event_node: &inspect::Node,
        key_path: &mut Vec<Cow<'a, str>>,
        map: &mut BTreeMap<String, (MetadataProperty, bool)>,
        metadata: Metadata<'a>,
    ) -> bool {
        let Metadata { key, inner: InnerMetadata::Nested(children) } = metadata else {
            unreachable!("We can only reach this function with nested nodes.");
        };
        let meta_node = parent.create_child(key.as_ref());
        let event_node = parent_event_node.create_child(key.as_ref());

        key_path.push(key);

        let mut recorded_events = false;
        for Metadata { inner: child_inner, key: child_key } in children {
            match child_inner {
                InnerMetadata::Nested(_) => {
                    recorded_events |= Self::insert_nested_to_map(
                        &meta_node,
                        &event_node,
                        key_path,
                        map,
                        Metadata { inner: child_inner, key: child_key },
                    );
                }
                InnerMetadata::Value { value, track_events } => {
                    if track_events {
                        recorded_events = true;
                        value.record_inspect(&event_node, &child_key);
                    }
                    key_path.push(child_key);
                    Self::insert_to_map(&meta_node, map, key_path, value, track_events);
                    key_path.pop();
                }
            }
        }

        key_path.pop();

        // Tie the lifetime of the nodes to that of the parent. If there were no events recorded on
        // this node, we can safely drop the event_node.
        parent.record(meta_node);
        if recorded_events {
            parent_event_node.record(event_node);
        }

        recorded_events
    }
}
