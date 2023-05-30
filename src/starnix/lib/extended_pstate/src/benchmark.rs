// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Measures the amount of time used to save and restore extended processor state such as floating
// point and vector registers and floating point control state. There are two types of operations
// measured:
//
//   - Reset             Measures the time taken to reset the in-memory copy of the state
//   - SaveAndRestore/*  Measure the time taken to save and immediately restore state using a
//                       particular strategy.
//
// x86_64 processors may support multiple instructions to save and restore state. The instructions
// tested in this benchmark are:
//   - XSaveOpt XSAVEOPT + XRSTOR
//   - XSave    XSAVE + XRSTOR
//   - FXSave   FXSAVE + FXRSTOR
//
// The benchmark exercises all instructions available on the currently running hardware and so may
// not test all instructions on a particular device.
//
// aarch64 processors provide only one mechanism for saving and restoring state.

use fuchsia as _;
use {
    criterion::Criterion,
    extended_pstate::ExtendedPstateState,
    fuchsia_criterion::FuchsiaCriterion,
    std::{mem, time::Duration},
};

#[cfg(target_arch = "x86_64")]
use extended_pstate::x86_64::{Strategy, PREFERRED_STRATEGY};

fn main() {
    let mut fc = FuchsiaCriterion::default();
    let c: &mut Criterion = &mut fc;
    *c = mem::take(c)
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(10))
        .sample_size(100);

    #[cfg(target_arch = "x86_64")]
    let bench_strategy = |bench: criterion::Benchmark, strategy| {
        if *PREFERRED_STRATEGY <= strategy {
            bench.with_function(&format!("SaveAndRestore/{:?}", strategy), move |b| {
                let mut state = ExtendedPstateState::with_strategy(strategy);
                b.iter(|| unsafe {
                    state.run_with_saved_state(|| {});
                });
            })
        } else {
            bench
        }
    };

    let mut bench = criterion::Benchmark::new("Reset", |b| {
        let mut state = ExtendedPstateState::default();
        b.iter(|| {
            state.reset();
        })
    });

    #[cfg(target_arch = "x86_64")]
    {
        bench = bench_strategy(bench, Strategy::XSaveOpt);
        bench = bench_strategy(bench, Strategy::XSave);
        bench = bench_strategy(bench, Strategy::FXSave);
    }
    #[cfg(target_arch = "aarch64")]
    {
        bench = bench.with_function("SaveAndRestore/Aarch64", |b| {
            let mut state = ExtendedPstateState::default();
            b.iter(|| unsafe {
                state.run_with_saved_state(|| {});
            });
        });
    }

    c.bench("fuchsia.extended_pstate", bench);
}
