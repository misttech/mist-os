// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(https://fxbug.dev/357825314): NodeExt and TimeProperty would benefit from being generic over
// the timeline, so that callers can choose whether to use Monotonic, Boot or something else.

//! Utilities and wrappers providing higher level functionality for Inspect Nodes and properties.

mod list;
mod lru_cache;

pub use list::BoundedListNode;
pub use lru_cache::LruCacheNode;

use fuchsia_inspect::{InspectType, IntProperty, Node, Property, StringReference};
use fuchsia_zircon as zx;

/// Extension trait that allows to manage timestamp properties.
pub trait NodeExt {
    /// Creates a new property holding the current monotonic timestamp.
    fn create_time(&self, name: impl Into<StringReference>) -> TimeProperty;

    /// Creates a new property holding the given timestamp.
    fn create_time_at<'b>(
        &self,
        name: impl Into<StringReference>,
        timestamp: zx::MonotonicTime,
    ) -> TimeProperty;

    /// Records a new property holding the current monotonic timestamp.
    fn record_time(&self, name: impl Into<StringReference>);
}

impl NodeExt for Node {
    fn create_time(&self, name: impl Into<StringReference>) -> TimeProperty {
        self.create_time_at(name, zx::MonotonicTime::get())
    }

    fn create_time_at<'b>(
        &self,
        name: impl Into<StringReference>,
        timestamp: zx::MonotonicTime,
    ) -> TimeProperty {
        TimeProperty { inner: self.create_int(name, timestamp.into_nanos()) }
    }

    fn record_time(&self, name: impl Into<StringReference>) {
        self.record_int(name, zx::MonotonicTime::get().into_nanos());
    }
}

/// Wrapper around an int property that stores a monotonic timestamp.
#[derive(Debug)]
pub struct TimeProperty {
    pub(crate) inner: IntProperty,
}

impl TimeProperty {
    /// Updates the underlying property with the current monotonic timestamp.
    pub fn update(&self) {
        self.set_at(zx::MonotonicTime::get());
    }

    /// Updates the underlying property with the given timestamp.
    pub fn set_at(&self, timestamp: zx::MonotonicTime) {
        Property::set(&self.inner, timestamp.into_nanos());
    }
}

impl InspectType for TimeProperty {}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyProperty, PropertyAssertion};
    use fuchsia_inspect::{DiagnosticsHierarchyGetter, Inspector};
    use test_util::assert_lt;

    #[fuchsia::test]
    fn test_time_metadata_format() {
        let inspector = Inspector::default();

        let time_property =
            inspector.root().create_time_at("time", zx::MonotonicTime::from_nanos(123_456_700_000));
        let t1 = validate_inspector_get_time(&inspector, 123_456_700_000i64);

        time_property.set_at(zx::MonotonicTime::from_nanos(333_005_000_000));
        let t2 = validate_inspector_get_time(&inspector, 333_005_000_000i64);

        time_property.set_at(zx::MonotonicTime::from_nanos(333_444_000_000));
        let t3 = validate_inspector_get_time(&inspector, 333_444_000_000i64);

        assert_lt!(t1, t2);
        assert_lt!(t2, t3);
    }

    #[fuchsia::test]
    fn test_create_time_and_update() {
        let inspector = Inspector::default();
        let time_property = inspector.root().create_time("time");
        let t1 = validate_inspector_get_time(&inspector, AnyProperty);

        time_property.update();
        let t2 = validate_inspector_get_time(&inspector, AnyProperty);

        time_property.update();
        let t3 = validate_inspector_get_time(&inspector, AnyProperty);

        assert_lt!(t1, t2);
        assert_lt!(t2, t3);
    }

    #[fuchsia::test]
    fn test_record_time() {
        let before_time = zx::MonotonicTime::get().into_nanos();
        let inspector = Inspector::default();
        inspector.root().record_time("time");
        let after_time = validate_inspector_get_time(&inspector, AnyProperty);
        assert_lt!(before_time, after_time);
    }

    #[fuchsia::test]
    fn test_create_time_no_executor() {
        let inspector = Inspector::default();
        inspector.root().create_time("time");
    }

    #[fuchsia::test]
    fn test_record_time_no_executor() {
        let inspector = Inspector::default();
        inspector.root().record_time("time");
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
