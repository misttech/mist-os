// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities and wrappers providing higher level functionality for Inspect Nodes and properties.

mod list;

pub use list::BoundedListNode;

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
        timestamp: zx::Time,
    ) -> TimeProperty;

    /// Records a new property holding the current monotonic timestamp.
    fn record_time(&self, name: impl Into<StringReference>);
}

impl NodeExt for Node {
    fn create_time(&self, name: impl Into<StringReference>) -> TimeProperty {
        self.create_time_at(name, zx::Time::get_monotonic())
    }

    fn create_time_at<'b>(
        &self,
        name: impl Into<StringReference>,
        timestamp: zx::Time,
    ) -> TimeProperty {
        TimeProperty { inner: self.create_int(name, timestamp.into_nanos()) }
    }

    fn record_time(&self, name: impl Into<StringReference>) {
        self.record_int(name, zx::Time::get_monotonic().into_nanos());
    }
}

/// Wrapper around an int property that stores a monotonic timestamp.
pub struct TimeProperty {
    pub(crate) inner: IntProperty,
}

impl TimeProperty {
    /// Updates the underlying property with the current monotonic timestamp.
    pub fn update(&self) {
        self.set_at(zx::Time::get_monotonic());
    }

    /// Updates the underlying property with the given timestamp.
    pub fn set_at(&self, timestamp: zx::Time) {
        Property::set(&self.inner, timestamp.into_nanos());
    }
}

impl InspectType for TimeProperty {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::FakeClock;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_inspect::Inspector;

    #[fuchsia::test]
    fn test_time_metadata_format() {
        let fake_clock = FakeClock::new();
        let current = fake_clock.get();
        let current_nanos = current.into_nanos();
        let inspector = Inspector::default();

        let time_property = inspector.root().create_time_at("time", current);
        assert_data_tree!(inspector, root: { time: current_nanos });

        let duration = zx::Duration::from_nanos(123);
        fake_clock.advance(duration);
        time_property.set_at(current + duration);
        assert_data_tree!(inspector, root: { time: current_nanos + 123 });

        fake_clock.advance(duration);
        time_property.set_at(current + zx::Duration::from_nanos(123 * 2));
        assert_data_tree!(inspector, root: { time: current_nanos + 123 * 2  });
    }

    #[fuchsia::test]
    fn test_create_time_and_update() {
        let fake_clock = FakeClock::new();
        let current = fake_clock.get().into_nanos();
        let inspector = Inspector::default();

        let time_property = inspector.root().create_time("time");
        assert_data_tree!(inspector, root: { time: current });

        fake_clock.advance(zx::Duration::from_nanos(5));
        time_property.update();
        assert_data_tree!(inspector, root: { time: current + 5 });

        fake_clock.advance(zx::Duration::from_nanos(5));
        time_property.update();
        assert_data_tree!(inspector, root: { time: current + 10 });
    }

    #[fuchsia::test]
    fn test_record_time() {
        let fake_clock = FakeClock::new();
        let current = fake_clock.get().into_nanos();
        fake_clock.advance(zx::Duration::from_nanos(55));
        let inspector = Inspector::default();
        inspector.root().record_time("time");
        assert_data_tree!(inspector, root: { time: current + 55 });
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
}
