// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_diagnostics::Selector;
use fuchsia_criterion::criterion;
use fuchsia_inspect::hierarchy::{filter_hierarchy, DiagnosticsHierarchyGetter, HierarchyMatcher};
use selectors::VerboseError;
use std::sync::{Arc, LazyLock};

static SELECTOR_TILL_LEVEL_30: LazyLock<Vec<String>> =
    LazyLock::new(|| generate_selectors_till_level(30));

const HIERARCHY_GENERATOR_SEED: u64 = 0;

/// Generate selectors which selects the nodes in the tree
/// and all the properties on the nodes till a given depth.
fn generate_selectors_till_level(depth: usize) -> Vec<String> {
    // TODO(https://fxbug.dev/42055229): Create a good combination of wildcards and exact matches
    let mut selector: String = String::from("*:root");
    (0..depth)
        .map(|_| {
            let current_selector = format!("{selector}:*");
            selector.push_str("/*");
            current_selector
        })
        .collect()
}

/// Parse selectors and returns an HierarchyMatcher
fn parse_selectors(selectors: &[String]) -> HierarchyMatcher {
    selectors
        .iter()
        .map(|selector| {
            Arc::new(
                selectors::parse_selector::<VerboseError>(selector)
                    .expect("Selectors must be valid and parseable."),
            )
        })
        .collect::<Vec<Arc<Selector>>>()
        .try_into()
        .expect("Unable to construct hierarchy matcher from selectors.")
}

fn snapshot_and_select_bench(b: &mut criterion::Bencher, size: usize) {
    let hierarchy_generator =
        fuchsia_inspect_bench_utils::filled_hierarchy_generator(HIERARCHY_GENERATOR_SEED, size);
    let hierarchy_matcher = parse_selectors(&SELECTOR_TILL_LEVEL_30);

    b.iter_with_large_drop(|| {
        let hierarchy = hierarchy_generator.get_diagnostics_hierarchy().into_owned();
        filter_hierarchy(hierarchy, &hierarchy_matcher)
    });
}

fn main() {
    let mut c = fuchsia_inspect_bench_utils::configured_criterion(
        fuchsia_inspect_bench_utils::CriterionConfig::default(),
    );

    let mut bench = criterion::Benchmark::new("SnapshotAndSelect/10", move |b| {
        snapshot_and_select_bench(b, 10usize);
    });
    for exponent in 2..=5 {
        // This benchmark takes a snapshot of a seedable randomly generated
        // inspect hierarchy in a vmo and then applies the given selectors
        // to the snapshot to filter it down.
        let size = 10i32.pow(exponent);
        bench = bench.with_function(format!("SnapshotAndSelect/{size}"), move |b| {
            snapshot_and_select_bench(b, size as usize);
        });
    }

    c.bench("fuchsia.rust_inspect.selectors", bench);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn generated_selectors_test() {
        let selectors = generate_selectors_till_level(3);
        assert_eq!(selectors, vec!["*:root:*", "*:root/*:*", "*:root/*/*:*"]);
    }
}
