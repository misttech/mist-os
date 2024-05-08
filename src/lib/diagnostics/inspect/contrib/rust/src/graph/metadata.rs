// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::{
    events::GraphObjectEventTracker,
    types::{EdgeMarker, VertexMarker},
    GraphObject, VertexId,
};
use fuchsia_inspect::{self as inspect, ArrayProperty, Property};
use std::{borrow::Cow, collections::BTreeMap};

/// An enum encoding all the possible metadata value types and contents.
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
                    for (idx, v) in value.into_iter().enumerate() {
                        prop.set(idx, *v);
                    }
                    node.record(prop);
                });
            }
            Self::UintVec(value) => {
                node.atomic_update(|node| {
                    let prop = node.create_uint_array(key, value.len());
                    for (idx, v) in value.into_iter().enumerate() {
                        prop.set(idx, *v);
                    }
                    node.record(prop);
                });
            }
            Self::DoubleVec(value) => {
                node.atomic_update(|node| {
                    let prop = node.create_double_array(key, value.len());
                    for (idx, v) in value.into_iter().enumerate() {
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

impl<'a> From<String> for MetadataValue<'_> {
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
    pub(crate) value: MetadataValue<'a>,
    /// Whether or not changes to this metadata field should be tracked.
    pub(crate) track_events: bool,
}

impl<'a> Metadata<'a> {
    /// Create a new metadata item with the given `key` and `value`.
    pub fn new(key: impl Into<Cow<'a, str>>, value: impl Into<MetadataValue<'a>>) -> Self {
        Self { key: key.into(), value: value.into(), track_events: false }
    }

    /// Require that changes to this metadata node must be tracked. To support tracking events
    /// about changes the graph must be configured to support tracking events.
    pub fn track_events(mut self) -> Self {
        self.track_events = true;
        self
    }
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
        self.inner.set(key, value);
    }

    /// Remove the metadata field at `key`.
    pub fn remove(&mut self, key: &str) {
        self.inner.remove(key);
    }

    pub(crate) fn new<'a>(
        parent: &inspect::Node,
        id: I,
        initial_metadata: impl Iterator<Item = &'a Metadata<'a>>,
        events_tracker: Option<GraphObjectEventTracker<VertexMarker<I>>>,
    ) -> Self {
        Self { inner: GraphMetadata::new(parent, id, initial_metadata, events_tracker) }
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
        self.inner.set(key, value);
    }

    /// Remove the metadata field at `key`.
    pub fn remove(&mut self, key: &str) {
        self.inner.remove(key);
    }

    pub(crate) fn new<'a>(
        parent: &inspect::Node,
        id: u64,
        initial_metadata: impl Iterator<Item = &'a Metadata<'a>>,
        events_tracker: Option<GraphObjectEventTracker<EdgeMarker>>,
    ) -> Self {
        Self { inner: GraphMetadata::new(parent, id, initial_metadata, events_tracker) }
    }

    pub(crate) fn events_tracker(&self) -> Option<&GraphObjectEventTracker<EdgeMarker>> {
        self.inner.events_tracker.as_ref()
    }

    pub(crate) fn id(&self) -> u64 {
        self.inner.id
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
        initial_metadata: impl Iterator<Item = &'a Metadata<'a>>,
        events_tracker: Option<GraphObjectEventTracker<T>>,
    ) -> Self {
        let node = parent.create_child("meta");
        let mut map = BTreeMap::default();
        node.atomic_update(|node| {
            for Metadata { key, value, track_events } in initial_metadata.into_iter() {
                Self::insert_to_map(&node, &mut map, key.to_string(), value, *track_events);
            }
        });
        Self { id, node, map, events_tracker }
    }

    fn set<'a, 'b>(&mut self, key: impl Into<Cow<'a, str>>, value: impl Into<MetadataValue<'b>>) {
        let key: Cow<'a, str> = key.into();
        let value = value.into();
        match (self.map.get(key.as_ref()), &value) {
            (Some((MetadataProperty::Int(property), track_events)), MetadataValue::Int(x)) => {
                property.set(*x);
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
            }
            (Some((MetadataProperty::Uint(property), track_events)), MetadataValue::Uint(x)) => {
                property.set(*x);
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
            }
            (
                Some((MetadataProperty::Double(property), track_events)),
                MetadataValue::Double(x),
            ) => {
                property.set(*x);
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
            }
            (Some((MetadataProperty::Bool(property), track_events)), MetadataValue::Bool(x)) => {
                property.set(*x);
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
            }
            (Some((MetadataProperty::Str(property), track_events)), MetadataValue::Str(x)) => {
                property.set(x.as_ref());
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
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
                if *track_events {
                    self.log_update_key(key.as_ref(), &value)
                }
                let track_events = *track_events;
                Self::insert_to_map(
                    &self.node,
                    &mut self.map,
                    key.into_owned(),
                    &value,
                    track_events,
                );
            }
            (None, value) => {
                Self::insert_to_map(&self.node, &mut self.map, key.into_owned(), &value, false);
            }
        }
    }

    fn remove(&mut self, key: &str) {
        self.map.remove(key);
    }

    fn log_update_key(&self, key: &str, value: &MetadataValue<'_>) {
        if let Some(events_tracker) = &self.events_tracker {
            events_tracker.metadata_updated(&self.id, key, value);
        }
    }

    fn insert_to_map(
        node: &inspect::Node,
        map: &mut BTreeMap<String, (MetadataProperty, bool)>,
        key: String,
        value: &MetadataValue<'_>,
        track_events: bool,
    ) {
        match value {
            MetadataValue::Int(value) => {
                let property = node.create_int(&key, *value);
                map.insert(key, (MetadataProperty::Int(property), track_events));
            }
            MetadataValue::Uint(value) => {
                let property = node.create_uint(&key, *value);
                map.insert(key, (MetadataProperty::Uint(property), track_events));
            }
            MetadataValue::Double(value) => {
                let property = node.create_double(&key, *value);
                map.insert(key, (MetadataProperty::Double(property), track_events));
            }
            MetadataValue::Str(value) => {
                let property = node.create_string(&key, value);
                map.insert(key, (MetadataProperty::Str(property), track_events));
            }
            MetadataValue::Bool(value) => {
                let property = node.create_bool(&key, *value);
                map.insert(key, (MetadataProperty::Bool(property), track_events));
            }
            MetadataValue::IntVec(value) => {
                let property = node.atomic_update(|node| {
                    let property = node.create_int_array(&key, value.len());
                    for (idx, v) in value.into_iter().enumerate() {
                        property.set(idx, *v);
                    }
                    property
                });
                map.insert(key, (MetadataProperty::IntVec(property), track_events));
            }
            MetadataValue::UintVec(value) => {
                let property = node.atomic_update(|node| {
                    let property = node.create_uint_array(&key, value.len());
                    for (idx, v) in value.into_iter().enumerate() {
                        property.set(idx, *v);
                    }
                    property
                });
                map.insert(key, (MetadataProperty::UintVec(property), track_events));
            }
            MetadataValue::DoubleVec(value) => {
                let property = node.atomic_update(|node| {
                    let property = node.create_double_array(&key, value.len());
                    for (idx, v) in value.into_iter().enumerate() {
                        property.set(idx, *v);
                    }
                    property
                });
                map.insert(key, (MetadataProperty::DoubleVec(property), track_events));
            }
        }
    }
}
