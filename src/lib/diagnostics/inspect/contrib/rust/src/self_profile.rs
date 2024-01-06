// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utility for profiling nested/hierarchical durations and reporting their accumulated runtime
//! in inspect. See caveats below before using.
//!
//! # Caveats
//!
//! Profiling data collected by this module has some runtime overhead depending on the level of
//! nesting and frequency of entrances and exits. Do not call `start_self_profiling` in production.
//!
//! The method this tool uses to collect nested durations' runtimes prevents them from perfectly
//! summing to 100% of the parent duration's runtime, even when the code is completely covered by
//! adjoining durations. As a rule of thumb, the error should be limited to a few percent of the
//! parent's runtime so if you see >10% unaccounted runtime you may need to add more durations.
//!
//! Runtimes are accumulated with atomics rather than with a single lock per duration to reduce
//! error accumulation from lock contention, but this means that a given inspect snapshot may
//! observe partially written results. This means that the tool is best suited for measuring
//! workloads where durations occur at a high frequency (and as a result may be a poor fit for
//! tracing) or where the timing of the inspect snapshot can be run after the profiled code has
//! exited all durations of interest.
//!
//! This module does not yet integrate well with `async`/`.await` workloads.
//!
//! # Setup
//!
//! There are three steps required to collect profiling data with this module:
//!
//! 1. register a lazy node at the top of your component's inspect hierarchies using
//!    `ProfileDuration::lazy_node_callback()`
//! 2. call `start_self_profiling()`
//! 3. invoke `profile_duration!("...");` at the beginning of scopes of interest
//!
//! # Analysis
//!
//! To see a human-readable summary of the captured profiles, take a snapshot of your component's
//! inspect and pass the results to `self_profiles_report::SelfProfilesReport`.

use fuchsia_inspect::{Inspector, Node};
use fuchsia_sync::Mutex;
use fuchsia_zircon::{self as zx, Task as _};
use once_cell::sync::Lazy;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    future::Future,
    panic::Location,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        Arc,
    },
};

static PROFILING_ENABLED: AtomicBool = AtomicBool::new(false);

static ROOT_PROFILE_DURATION: Lazy<Arc<ProfileDurationTree>> =
    Lazy::new(|| Arc::new(ProfileDurationTree::new(Location::caller())));

thread_local! {
    static CURRENT_PROFILE_DURATION: RefCell<Arc<ProfileDurationTree>> =
        RefCell::new(ROOT_PROFILE_DURATION.clone());
}

/// Profile the remainder of the invoking lexical scope under the provided name.
///
/// If you need to split a single lexical scope into multiple durations, consider using
/// `ProfileDuration::enter` and `ProfileDuration::pivot`.
#[macro_export]
macro_rules! profile_duration {
    ($name:expr) => {
        let _guard = $crate::ProfileDuration::enter($name);
    };
}

/// Start collecting profiling information for any durations entered after this call is made.
pub fn start_self_profiling() {
    PROFILING_ENABLED.store(true, Ordering::Relaxed);
}

/// Stop collecting profiling information for any durations entered after this call is made.
pub fn stop_self_profiling() {
    PROFILING_ENABLED.store(false, Ordering::Relaxed);
}

#[inline]
fn profiling_enabled() -> bool {
    PROFILING_ENABLED.load(Ordering::Relaxed)
}

/// A guard value to manually control entry/exit of profile durations.
pub struct ProfileDuration {
    inner: Option<InnerGuard>,
}

impl ProfileDuration {
    /// Function to be passed to `fuchsia_inspect::Node::record_lazy_child` to record profiling data.
    pub fn lazy_node_callback(
    ) -> Pin<Box<dyn Future<Output = Result<Inspector, anyhow::Error>> + Send + 'static>> {
        Box::pin(async move {
            let inspector = Inspector::default();
            ROOT_PROFILE_DURATION.report(inspector.root());

            // Make sure the analysis tooling can find this node regardless of the user-defined name.
            inspector.root().record_bool("__profile_durations_root", true);
            Ok(inspector)
        })
    }

    /// Manually enter a profile duration. The duration will be exited when the returned value is
    /// dropped.
    ///
    /// If using a single variable binding to hold guards for multiple durations, call
    /// `ProfileDuration::pivot` instead of re-assigning to the binding.
    #[track_caller]
    pub fn enter(name: &'static str) -> Self {
        Self { inner: if profiling_enabled() { Some(InnerGuard::enter(name)) } else { None } }
    }

    /// End the current duration and enter a new one, re-using the same guard value. Allows
    /// splitting a single lexical scope into multiple durations.
    #[track_caller]
    pub fn pivot(&mut self, name: &'static str) {
        if profiling_enabled() {
            drop(self.inner.take());
            self.inner = Some(InnerGuard::enter(name));
        }
    }
}

struct InnerGuard {
    start_runtime: zx::TaskRuntimeInfo,
    start_monotonic_ns: zx::Time,
    parent_duration: Arc<ProfileDurationTree>,
}

impl InnerGuard {
    #[track_caller]
    fn enter(name: &'static str) -> Self {
        let start_monotonic_ns = zx::Time::get_monotonic();
        let start_runtime = current_thread_runtime();

        // Get the location outside the below closure since it can't be track_caller on stable.
        let location = Location::caller();
        let parent_duration = CURRENT_PROFILE_DURATION.with(|current_duration| {
            let mut current_duration = current_duration.borrow_mut();
            let child_duration = current_duration.child(name, location);
            std::mem::replace(&mut *current_duration, child_duration)
        });

        Self { start_runtime, start_monotonic_ns, parent_duration }
    }
}

impl Drop for InnerGuard {
    fn drop(&mut self) {
        CURRENT_PROFILE_DURATION.with(|current_duration| {
            let mut current_duration = current_duration.borrow_mut();
            let completed_duration =
                std::mem::replace(&mut *current_duration, self.parent_duration.clone());

            let runtime_delta = current_thread_runtime() - self.start_runtime;
            let wall_time_delta = zx::Time::get_monotonic() - self.start_monotonic_ns;

            completed_duration.count.fetch_add(1, Ordering::Relaxed);
            completed_duration.wall_time.fetch_add(wall_time_delta.into_nanos(), Ordering::Relaxed);
            completed_duration.cpu_time.fetch_add(runtime_delta.cpu_time, Ordering::Relaxed);
            completed_duration.queue_time.fetch_add(runtime_delta.queue_time, Ordering::Relaxed);
            completed_duration
                .page_fault_time
                .fetch_add(runtime_delta.page_fault_time, Ordering::Relaxed);
            completed_duration
                .lock_contention_time
                .fetch_add(runtime_delta.lock_contention_time, Ordering::Relaxed);
        });
    }
}

#[derive(Debug)]
struct ProfileDurationTree {
    location: &'static Location<'static>,
    count: AtomicU64,
    wall_time: AtomicI64,
    cpu_time: AtomicI64,
    queue_time: AtomicI64,
    page_fault_time: AtomicI64,
    lock_contention_time: AtomicI64,
    children: Mutex<BTreeMap<&'static str, Arc<Self>>>,
}

impl ProfileDurationTree {
    fn new(location: &'static Location<'static>) -> Self {
        Self {
            location,
            count: Default::default(),
            wall_time: Default::default(),
            cpu_time: Default::default(),
            queue_time: Default::default(),
            page_fault_time: Default::default(),
            lock_contention_time: Default::default(),
            children: Default::default(),
        }
    }

    fn child(&self, name: &'static str, location: &'static Location<'static>) -> Arc<Self> {
        self.children.lock().entry(name).or_insert_with(|| Arc::new(Self::new(location))).clone()
    }

    fn report(&self, node: &Node) {
        let children = self.children.lock();
        node.record_string("location", self.location.to_string());
        node.record_uint("count", self.count.load(Ordering::Relaxed));
        node.record_int("wall_time", self.wall_time.load(Ordering::Relaxed));
        node.record_int("cpu_time", self.cpu_time.load(Ordering::Relaxed));
        node.record_int("queue_time", self.queue_time.load(Ordering::Relaxed));
        node.record_int("page_fault_time", self.page_fault_time.load(Ordering::Relaxed));
        node.record_int("lock_contention_time", self.lock_contention_time.load(Ordering::Relaxed));
        for (name, child) in children.iter() {
            node.record_child(*name, |n| child.report(n));
        }
    }
}

fn current_thread_runtime() -> zx::TaskRuntimeInfo {
    fuchsia_runtime::thread_self()
        .get_runtime_info()
        .expect("should always be able to read own thread's runtime info")
}
