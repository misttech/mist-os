// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_criterion::criterion::{self, Criterion};
use fuchsia_criterion::FuchsiaCriterion;
use selectors::FastError;
use std::time::Duration;
use std::{fmt, mem};

struct Case {
    name: &'static str,
    val: String,
}

impl fmt::Display for Case {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.name, self.val.len())
    }
}

fn make_repeated_cases(name: &'static str, base: &'static str, repeats: Vec<usize>) -> Vec<Case> {
    let mut ret = vec![];
    for r in repeats {
        ret.push(Case { name, val: base.repeat(r) });
    }
    ret
}

fn make_selector_cases(
    case_name: &'static str,
    base: &'static str,
    tree_name: Option<&'static str>,
    repeats: Vec<usize>,
) -> Vec<Case> {
    let mut ret = vec![];
    for r in repeats {
        let segment_count = if tree_name.is_some() { 3 } else { r };
        let mut segment = [base]
            .iter()
            .cycle()
            .take(segment_count)
            .map(|s| s.to_string())
            .collect::<Vec<String>>()
            .join("/");

        let component_selector = segment.clone();
        if let Some(n) = tree_name {
            let name_set = [n]
                .iter()
                .cycle()
                .enumerate()
                .map(|(i, s)| format!(r#"name="{s}{i}""#))
                .take(r - 1)
                .collect::<Vec<String>>()
                .join(",");

            segment = format!(r#"[{name_set}, name="{n}"]{segment}"#);
        }

        let val = [component_selector, segment, base.to_string()].join(":");
        ret.push(Case { name: case_name, val });
    }
    ret
}

fn bench_sanitize_string_for_selectors() -> criterion::Benchmark {
    let mut bench = criterion::Benchmark::new("sanitize_string_for_selectors/empty", move |b| {
        b.iter(|| criterion::black_box(selectors::sanitize_string_for_selectors("")));
    });

    // Measure the time taken by sanitize_string_for_selectors() on
    // strings where different amounts of string escaping is required. This
    // function is called frequently during selector parsing.
    let cases: Vec<Case> = vec![]
        .into_iter()
        .chain(make_repeated_cases("no_replace", "abcd", vec![2, 64]))
        .chain(make_repeated_cases("replace_half", "a:b*", vec![2, 64]))
        .chain(make_repeated_cases("replace_all", ":*\\:", vec![2, 64]))
        .collect();

    for case in cases.into_iter() {
        bench = bench.with_function(
            format!("sanitize_string_for_selectors/{}", case.to_string()),
            move |b| {
                b.iter(|| criterion::black_box(selectors::sanitize_string_for_selectors(&case.val)))
            },
        );
    }

    bench
}

fn bench_parse_selector() -> criterion::Benchmark {
    let cases: Vec<Case> = vec![]
        .into_iter()
        .chain(make_selector_cases("no_wildcard", "abcd", None, vec![2, 64]))
        .chain(make_selector_cases("with_wildcard", "*ab*", None, vec![2, 64]))
        .chain(make_selector_cases("with_escaped", "ab\\:", None, vec![2, 64]))
        .chain(make_selector_cases("with_tree_name", "abcd", Some("foo"), vec![2, 64]))
        .chain(make_selector_cases(
            "with_tree_name_and_odd_chars",
            "abcd",
            Some(r#"foo:bar,baz\"qux\*_-"#),
            vec![2, 64],
        ))
        .collect();

    let mut bench = criterion::Benchmark::new("parse_selector/empty", move |b| {
        b.iter(|| criterion::black_box(selectors::parse_selector::<FastError>("").unwrap_err()));
    });

    for case in cases.into_iter() {
        bench = bench.with_function(format!("parse_selector/{}", case.to_string()), move |b| {
            b.iter(|| {
                criterion::black_box(selectors::parse_selector::<FastError>(&case.val).unwrap())
            })
        });
    }

    bench
}

fn main() {
    let mut c = FuchsiaCriterion::default();
    let internal_c: &mut Criterion = &mut c;
    *internal_c = mem::take(internal_c)
        .warm_up_time(Duration::from_millis(150))
        .measurement_time(Duration::from_millis(300))
        .sample_size(20);

    c.bench("fuchsia.diagnostics.lib.selectors", bench_sanitize_string_for_selectors());
    c.bench("fuchsia.diagnostics.lib.selectors", bench_parse_selector());
}
