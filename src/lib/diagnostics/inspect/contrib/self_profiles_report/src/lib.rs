// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Analyze output from fuchsia_inspect_contrib's self profiling feature.

use diagnostics_data::{DiagnosticsHierarchy, ExtendedMoniker, InspectData, Property};
use std::collections::BTreeMap;

/// If a duration used less than 0.5% of its parent's CPU time, it's probably not that interesting.
const CHILD_PROPORTION_DISPLAY_THRESHOLD: f64 = 0.005;

#[derive(Debug, PartialEq)]
pub struct SelfProfilesReport {
    name: String,
    root_summary: DurationSummary,
    custom_rollups: Vec<CustomRollup>,
}

impl SelfProfilesReport {
    #[allow(clippy::result_large_err, reason = "mass allow for https://fxbug.dev/381896734")]
    pub fn from_snapshot(data: &[InspectData]) -> Result<Vec<Self>, AnalysisError> {
        let mut summaries = vec![];
        for d in data {
            if let Some(s) = Self::from_single_snapshot(d) {
                summaries.push(s?);
            }
        }
        Ok(summaries)
    }

    pub fn from_single_snapshot(data: &InspectData) -> Option<Result<Self, AnalysisError>> {
        if let Some(payload) = data.payload.as_ref() {
            for child_node in &payload.children {
                if child_node.get_property("__profile_durations_root").and_then(|p| p.boolean())
                    == Some(true)
                {
                    return Some(Self::from_node(&data.moniker, child_node));
                }
            }
        }
        None
    }

    #[allow(clippy::result_large_err, reason = "mass allow for https://fxbug.dev/381896734")]
    fn from_node(
        name: &ExtendedMoniker,
        node: &DiagnosticsHierarchy,
    ) -> Result<Self, AnalysisError> {
        let root_summary = DurationSummaryBuilder::from_inspect(node)?.build();
        Ok(Self { name: name.to_string(), root_summary, custom_rollups: vec![] })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn root_summary(&self) -> &DurationSummary {
        &self.root_summary
    }

    pub fn leaf_durations(&self) -> Vec<(String, DurationSummary)> {
        let mut leaves = BTreeMap::new();
        self.root_summary.summarize_leaves(&self.name, &mut leaves);

        let mut leaves = leaves.into_iter().collect::<Vec<_>>();
        leaves.sort_by_key(|d| d.1.runtime.cpu_time);
        leaves.reverse();

        leaves
    }

    pub fn delta_from(&self, baseline: &Self) -> Result<Self, ComparisonError> {
        if self.name != baseline.name {
            return Err(ComparisonError::MismatchedNames {
                lhs: self.name.clone(),
                rhs: baseline.name.clone(),
            });
        }
        Ok(Self {
            name: self.name.clone(),
            root_summary: self.root_summary.delta_from(&baseline.root_summary)?,
            custom_rollups: vec![],
        })
    }

    pub fn add_rollup(
        &mut self,
        title: impl Into<String>,
        prefixes: impl IntoIterator<Item = impl Into<String>>,
    ) {
        self.custom_rollups.push(CustomRollup {
            display_title: title.into(),
            match_prefixes: prefixes.into_iter().map(|s| s.into()).collect(),
        });
    }
}

impl std::fmt::Display for SelfProfilesReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Profile duration summary for `{}`:\n\n{}\n", self.name, self.root_summary)?;

        let leaf_durations = self.leaf_durations();
        let root_runtime = self.root_summary.runtime;

        writeln!(f, "Rolled up leaf durations:\n")?;
        for (name, duration) in &leaf_durations {
            let proportion_of_total =
                duration.runtime.cpu_time as f64 / root_runtime.cpu_time as f64;
            if proportion_of_total >= CHILD_PROPORTION_DISPLAY_THRESHOLD {
                write!(f, "{}", duration.display_tree(name, root_runtime))?;
            }
        }

        for rollup in &self.custom_rollups {
            writeln!(f, "Custom rollup: {}\n", rollup.display_title)?;

            for (name, duration) in &leaf_durations {
                #[allow(
                    clippy::search_is_some,
                    reason = "mass allow for https://fxbug.dev/381896734"
                )]
                if rollup
                    .match_prefixes
                    .iter()
                    .find(|prefix| name.starts_with(prefix.as_str()))
                    .is_some()
                {
                    write!(f, "{}", duration.display_tree(name, root_runtime))?;
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Default)]
struct DurationSummaryBuilder {
    count: u64,
    runtime: TaskRuntimeInfo,
    location: String,
    children: BTreeMap<String, Self>,
}

impl DurationSummaryBuilder {
    #[allow(clippy::result_large_err, reason = "mass allow for https://fxbug.dev/381896734")]
    fn from_inspect(node: &DiagnosticsHierarchy) -> Result<Self, AnalysisError> {
        let mut children = BTreeMap::new();
        for child_node in &node.children {
            let (name, child) = Self::from_inspect_recursive(child_node)?;
            children.insert(name, child);
        }
        let location = node.get_property("location").unwrap().string().unwrap().to_owned();

        // The top-level node doesn't get any metrics recorded, it's just a container for children.
        let runtime = children.values().map(|c| c.runtime).sum();

        Ok(Self { runtime, count: 0, children, location })
    }

    #[allow(clippy::result_large_err, reason = "mass allow for https://fxbug.dev/381896734")]
    fn from_inspect_recursive(
        node: &DiagnosticsHierarchy,
    ) -> Result<(String, Self), AnalysisError> {
        let count = node.get_property("count").unwrap().uint().unwrap();
        let runtime = TaskRuntimeInfo {
            cpu_time: get_time_property(node, "cpu_time")?,
            queue_time: get_time_property(node, "queue_time")?,
            page_fault_time: get_time_property(node, "page_fault_time")?,
            lock_contention_time: get_time_property(node, "lock_contention_time")?,
            wall_time: get_time_property(node, "wall_time")?,
        };
        let location = node.get_property("location").unwrap().string().unwrap().to_owned();

        let mut children = BTreeMap::new();
        for child_node in &node.children {
            let (name, child) = Self::from_inspect_recursive(child_node)?;
            children.insert(name, child);
        }

        Ok((node.name.clone(), Self { count, runtime, children, location }))
    }

    fn build(&self) -> DurationSummary {
        let mut children = vec![];
        for (name, child) in &self.children {
            children.push((name.clone(), child.build()));
        }

        // Sort children by how much time they occupied.
        children.sort_by_key(|(_, analysis)| analysis.runtime.cpu_time);
        children.reverse();

        DurationSummary {
            count: self.count,
            runtime: self.runtime,
            location: self.location.clone(),
            children,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DurationSummary {
    count: u64,
    runtime: TaskRuntimeInfo,
    location: String,
    children: Vec<(String, Self)>,
}

impl DurationSummary {
    /// The source location where this duration was entered.
    pub fn location(&self) -> &str {
        self.location.as_str()
    }

    /// Number of times this duration was exited.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Sum of time this duration was scheduled on-CPU, in nanoseconds.
    pub fn cpu_time(&self) -> i64 {
        self.runtime.cpu_time
    }

    /// Sum of time this duration was ready to execute and was pending in the scheduler queue,
    /// in nanoseconds.
    pub fn queue_time(&self) -> i64 {
        self.runtime.queue_time
    }

    /// Sum of time this duration was blocked handling page faults, in nanoseconds.
    pub fn page_fault_time(&self) -> i64 {
        self.runtime.page_fault_time
    }

    /// Sum of time this duration was blocked on contended kernel locks, in nanoseconds.
    pub fn lock_contention_time(&self) -> i64 {
        self.runtime.lock_contention_time
    }

    /// Sum of time this duration was logically executing on the monotonic clock, in nanoseconds.
    pub fn wall_time(&self) -> i64 {
        self.runtime.wall_time
    }

    /// Child durations observed while this was executing.
    pub fn children(&self) -> impl Iterator<Item = (&str, &Self)> {
        self.children.iter().map(|(name, summary)| (name.as_str(), summary))
    }

    fn summarize_leaves(&self, own_name: &str, leaves: &mut BTreeMap<String, Self>) {
        if self.children.is_empty() {
            match leaves.entry(own_name.to_string()) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(DurationSummary {
                        count: self.count,
                        runtime: self.runtime,
                        location: self.location.clone(),
                        children: vec![],
                    });
                }
                std::collections::btree_map::Entry::Occupied(mut o) => {
                    let leaf = o.get_mut();
                    leaf.runtime += self.runtime;
                    leaf.count += self.count;
                }
            }
        } else {
            for (name, child) in &self.children {
                child.summarize_leaves(name, leaves);
            }
        }
    }

    fn display_leaf_no_location(
        &self,
        name: &str,
        count: u64,
        runtime: TaskRuntimeInfo,
    ) -> termtree::Tree<DurationRuntimeWithPercentage> {
        let mut leaf = termtree::Tree::new(DurationRuntimeWithPercentage {
            name: name.to_owned(),
            count,
            runtime,
            location: None,
            portion_of_parent_cpu_time: runtime.cpu_time as f64 / self.runtime.cpu_time as f64,
        });
        leaf.set_multiline(true);
        leaf
    }

    fn display_tree(
        &self,
        name: &str,
        parent_total_runtime: TaskRuntimeInfo,
    ) -> termtree::Tree<DurationRuntimeWithPercentage> {
        let mut tree = termtree::Tree::new(DurationRuntimeWithPercentage {
            name: name.to_owned(),
            count: self.count,
            runtime: self.runtime,
            location: Some(self.location.clone()),
            portion_of_parent_cpu_time: self.runtime.cpu_time as f64
                / parent_total_runtime.cpu_time as f64,
        });
        tree.set_multiline(true);

        let mut etc_time = TaskRuntimeInfo::default();
        let mut etc_count = 0;
        for (name, child) in &self.children {
            let portion_of_self = child.runtime.cpu_time as f64 / self.runtime.cpu_time as f64;
            if portion_of_self > CHILD_PROPORTION_DISPLAY_THRESHOLD {
                tree.push(child.display_tree(name, self.runtime));
            } else {
                etc_count += child.count;
                etc_time += child.runtime;
            }
        }
        if !self.children.is_empty() {
            // Add a bucket for time that was not accounted by the child durations (if any).
            let unaccounted_runtime =
                self.runtime - self.children.iter().map(|(_, c)| c.runtime).sum();
            let portion_unaccounted =
                unaccounted_runtime.cpu_time as f64 / self.runtime.cpu_time as f64;
            if portion_unaccounted > CHILD_PROPORTION_DISPLAY_THRESHOLD {
                tree.push(self.display_leaf_no_location("UNACCOUNTED", 0, unaccounted_runtime));
            } else {
                etc_time += unaccounted_runtime;
            }
        }
        if etc_time.cpu_time > 0 {
            tree.push(self.display_leaf_no_location("BELOW_THRESHOLD", etc_count, etc_time));
        }

        tree
    }

    fn delta_from(&self, baseline: &Self) -> Result<Self, ComparisonError> {
        let count = self.count - baseline.count;
        let runtime = self.runtime - baseline.runtime;
        if self.location != baseline.location {
            return Err(ComparisonError::MismatchedLocations {
                lhs: self.location.clone(),
                rhs: baseline.location.clone(),
            });
        }

        let mut delta_children = BTreeMap::<String, Self>::new();
        let baseline_children = baseline
            .children
            .iter()
            .map(/*invoke auto ref */ |(n, s)| (n, s))
            .collect::<BTreeMap<_, _>>();
        for (name, summary) in &self.children {
            if let Some(baseline_summary) = baseline_children.get(name) {
                delta_children.insert(name.clone(), summary.delta_from(baseline_summary)?);
            } else {
                delta_children.insert(name.clone(), summary.clone());
            }
        }

        // Sort children by how much time they occupied.
        let mut children = delta_children.into_iter().collect::<Vec<_>>();
        children.sort_by_key(|(_, analysis)| analysis.runtime.cpu_time);
        children.reverse();

        Ok(Self { count, runtime, children, location: self.location.clone() })
    }
}

impl std::fmt::Display for DurationSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total_child_runtime = self.children().map(|(_, c)| c.runtime).sum();
        write!(f, "{}", self.display_tree("overall", total_child_runtime))
    }
}

#[allow(clippy::result_large_err, reason = "mass allow for https://fxbug.dev/381896734")]
fn get_time_property(
    node: &DiagnosticsHierarchy,
    name: &'static str,
) -> Result<i64, AnalysisError> {
    let property = node.get_property(name).ok_or(AnalysisError::MissingTime { name })?;
    property
        .number_as_int()
        .ok_or_else(|| AnalysisError::WrongType { name, property: property.clone() })
}

#[derive(Debug)]
pub struct DurationRuntimeWithPercentage {
    name: String,
    location: Option<String>,
    portion_of_parent_cpu_time: f64,
    count: u64,
    runtime: TaskRuntimeInfo,
}

impl std::fmt::Display for DurationRuntimeWithPercentage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:^5.3}%: {}", self.portion_of_parent_cpu_time * 100.0, self.name)?;
        if self.count > 0 {
            write!(f, ", {}x", self.count)?;
        }
        writeln!(
            f,
            "\n    cpu: {}, queue: {}, faulting: {}, contended: {}, wall: {}",
            ns_to_ms(self.runtime.cpu_time),
            ns_to_ms(self.runtime.queue_time),
            ns_to_ms(self.runtime.page_fault_time),
            ns_to_ms(self.runtime.lock_contention_time),
            ns_to_ms(self.runtime.wall_time),
        )?;
        if let Some(location) = &self.location {
            writeln!(f, "    source: {location}")?;
        }
        Ok(())
    }
}

fn ns_to_ms(ns: i64) -> String {
    format!("{:^5.3}ms", ns as f64 / 1_000_000.0)
}

#[derive(Debug, thiserror::Error)]
pub enum ComparisonError {
    #[error("Can't compare profiles from two different sources `{lhs}` and `{rhs}`.")]
    MismatchedNames { lhs: String, rhs: String },

    #[error("Can't compare profiles which disagree on the source location for a duration: `{lhs}` vs `{rhs}`")]
    MismatchedLocations { lhs: String, rhs: String },
}

/// Failures that can occur when analyzing Starnix traces.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("Profile duration inspect node without `{name}` property.")]
    MissingTime { name: &'static str },

    #[error(
        "Profile duration inspect node's `{name}` property had a non-integer type: {property:?}"
    )]
    WrongType { name: &'static str, property: Property<String> },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TaskRuntimeInfo {
    cpu_time: i64,
    queue_time: i64,
    page_fault_time: i64,
    lock_contention_time: i64,
    wall_time: i64,
}

impl std::ops::Add for TaskRuntimeInfo {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        Self {
            cpu_time: self.cpu_time + rhs.cpu_time,
            queue_time: self.queue_time + rhs.queue_time,
            page_fault_time: self.page_fault_time + rhs.page_fault_time,
            lock_contention_time: self.lock_contention_time + rhs.lock_contention_time,
            wall_time: self.wall_time + rhs.wall_time,
        }
    }
}

impl std::ops::AddAssign for TaskRuntimeInfo {
    fn add_assign(&mut self, rhs: Self) {
        self.cpu_time += rhs.cpu_time;
        self.queue_time += rhs.queue_time;
        self.page_fault_time += rhs.page_fault_time;
        self.lock_contention_time += rhs.lock_contention_time;
        self.wall_time += rhs.wall_time;
    }
}

impl std::ops::Sub for TaskRuntimeInfo {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        Self {
            cpu_time: self.cpu_time - rhs.cpu_time,
            queue_time: self.queue_time - rhs.queue_time,
            page_fault_time: self.page_fault_time - rhs.page_fault_time,
            lock_contention_time: self.lock_contention_time - rhs.lock_contention_time,
            wall_time: self.wall_time - rhs.wall_time,
        }
    }
}

impl std::ops::SubAssign for TaskRuntimeInfo {
    fn sub_assign(&mut self, rhs: Self) {
        self.cpu_time -= rhs.cpu_time;
        self.queue_time -= rhs.queue_time;
        self.page_fault_time -= rhs.page_fault_time;
        self.lock_contention_time -= rhs.lock_contention_time;
        self.wall_time -= rhs.wall_time;
    }
}

impl std::iter::Sum for TaskRuntimeInfo {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |l, r| l + r)
    }
}

#[derive(Debug, PartialEq)]
struct CustomRollup {
    display_title: String,
    match_prefixes: Vec<String>,
}
