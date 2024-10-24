// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities and wrappers providing higher level functionality for Inspect Nodes and properties.

use std::{fmt, marker};

mod list;
mod lru_cache;

pub use list::BoundedListNode;
pub use lru_cache::LruCacheNode;
pub use zx::{BootTimeline, MonotonicTimeline};

use fuchsia_inspect::{InspectType, IntProperty, Node, Property, StringReference};

/// Returned by functions which take the current time and write it to a property.
pub struct CreateTimeResult<T> {
    /// The time written to the property.
    pub timestamp: zx::Instant<T>,
    /// A property to which the timestamp was written.
    pub property: TimeProperty<T>,
}

/// Extension trait that allows to manage timestamp properties.
pub trait NodeTimeExt<T: zx::Timeline> {
    /// Creates a new property holding the current timestamp on the given timeline. Returns the
    /// current timestamp that was used for the returned property too.
    fn create_time(&self, name: impl Into<StringReference>) -> CreateTimeResult<T>;

    /// Creates a new property holding the given timestamp.
    fn create_time_at(
        &self,
        name: impl Into<StringReference>,
        timestamp: zx::Instant<T>,
    ) -> TimeProperty<T>;

    /// Records a new property holding the current timestamp and returns the instant that was
    /// recorded.
    fn record_time(&self, name: impl Into<StringReference>) -> zx::Instant<T>;
}

impl NodeTimeExt<zx::MonotonicTimeline> for Node {
    fn create_time(
        &self,
        name: impl Into<StringReference>,
    ) -> CreateTimeResult<zx::MonotonicTimeline> {
        let timestamp = zx::MonotonicInstant::get();
        CreateTimeResult { timestamp, property: self.create_time_at(name, timestamp) }
    }

    fn create_time_at(
        &self,
        name: impl Into<StringReference>,
        timestamp: zx::MonotonicInstant,
    ) -> TimeProperty<zx::MonotonicTimeline> {
        TimeProperty {
            inner: self.create_int(name, timestamp.into_nanos()),
            _phantom: marker::PhantomData,
        }
    }

    fn record_time(&self, name: impl Into<StringReference>) -> zx::MonotonicInstant {
        let instant = zx::MonotonicInstant::get();
        self.record_int(name, instant.into_nanos());
        instant
    }
}

impl NodeTimeExt<zx::BootTimeline> for Node {
    fn create_time(&self, name: impl Into<StringReference>) -> CreateTimeResult<zx::BootTimeline> {
        let timestamp = zx::BootInstant::get();
        CreateTimeResult { timestamp, property: self.create_time_at(name, timestamp) }
    }

    fn create_time_at(
        &self,
        name: impl Into<StringReference>,
        timestamp: zx::BootInstant,
    ) -> TimeProperty<zx::BootTimeline> {
        TimeProperty {
            inner: self.create_int(name, timestamp.into_nanos()),
            _phantom: marker::PhantomData,
        }
    }

    fn record_time(&self, name: impl Into<StringReference>) -> zx::BootInstant {
        let instant = zx::BootInstant::get();
        self.record_int(name, instant.into_nanos());
        instant
    }
}

/// Wrapper around an int property that stores a monotonic timestamp.
#[derive(Debug)]
pub struct TimeProperty<T> {
    pub(crate) inner: IntProperty,
    _phantom: marker::PhantomData<T>,
}

impl TimeProperty<zx::MonotonicTimeline> {
    /// Updates the underlying property with the current monotonic timestamp.
    pub fn update(&self) {
        self.set_at(zx::MonotonicInstant::get());
    }

    /// Updates the underlying property with the given timestamp.
    pub fn set_at(&self, timestamp: zx::MonotonicInstant) {
        Property::set(&self.inner, timestamp.into_nanos());
    }
}

impl TimeProperty<zx::BootTimeline> {
    /// Updates the underlying property with the current monotonic timestamp.
    pub fn update(&self) {
        self.set_at(zx::BootInstant::get());
    }

    /// Updates the underlying property with the given timestamp.
    pub fn set_at(&self, timestamp: zx::BootInstant) {
        Property::set(&self.inner, timestamp.into_nanos());
    }
}

/// An Inspect Time Property on the boot timeline.
pub type BootTimeProperty = TimeProperty<zx::BootTimeline>;

/// An Inspect Time Property on the monotonictimeline.
pub type MonotonicTimeProperty = TimeProperty<zx::MonotonicTimeline>;

impl<T: fmt::Debug + Send + Sync> InspectType for TimeProperty<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty, PropertyAssertion};
    use fuchsia_inspect::{DiagnosticsHierarchyGetter, Inspector};
    use test_util::assert_lt;

    #[fuchsia::test]
    fn test_time_metadata_format() {
        let inspector = Inspector::default();

        let time_property = inspector
            .root()
            .create_time_at("time", zx::MonotonicInstant::from_nanos(123_456_700_000));
        let t1 = validate_inspector_get_time(&inspector, 123_456_700_000i64);

        time_property.set_at(zx::MonotonicInstant::from_nanos(333_005_000_000));
        let t2 = validate_inspector_get_time(&inspector, 333_005_000_000i64);

        time_property.set_at(zx::MonotonicInstant::from_nanos(333_444_000_000));
        let t3 = validate_inspector_get_time(&inspector, 333_444_000_000i64);

        assert_lt!(t1, t2);
        assert_lt!(t2, t3);
    }

    #[fuchsia::test]
    fn test_create_time_and_update() {
        let inspector = Inspector::default();
        let CreateTimeResult { timestamp: recorded_t1, property: time_property }: CreateTimeResult<
            zx::MonotonicTimeline,
        > = inspector.root().create_time("time");
        let t1 = validate_inspector_get_time(&inspector, AnyProperty);
        assert_eq!(recorded_t1.into_nanos(), t1);

        time_property.update();
        let t2 = validate_inspector_get_time(&inspector, AnyProperty);

        time_property.update();
        let t3 = validate_inspector_get_time(&inspector, AnyProperty);

        assert_lt!(t1, t2);
        assert_lt!(t2, t3);
    }

    #[fuchsia::test]
    fn test_record_time() {
        let before_time = zx::MonotonicInstant::get().into_nanos();
        let inspector = Inspector::default();
        NodeTimeExt::<zx::MonotonicTimeline>::record_time(inspector.root(), "time");
        let after_time = validate_inspector_get_time(&inspector, AnyProperty);
        assert_lt!(before_time, after_time);
    }

    #[fuchsia::test]
    fn test_create_time_no_executor() {
        let inspector = Inspector::default();
        let _: CreateTimeResult<zx::MonotonicTimeline> = inspector.root().create_time("time");
    }

    #[fuchsia::test]
    fn test_record_time_no_executor() {
        let inspector = Inspector::default();
        NodeTimeExt::<zx::MonotonicTimeline>::record_time(inspector.root(), "time");
    }

    fn validate_inspector_get_time<T>(inspector: &Inspector, expected: T) -> i64
    where
        T: PropertyAssertion<String> + 'static,
    {
        let hierarchy = inspector.get_diagnostics_hierarchy();
        assert_data_tree!(hierarchy, root: { time: expected });
        hierarchy.get_property("time").and_then(|t| t.int()).unwrap()
    }
}
