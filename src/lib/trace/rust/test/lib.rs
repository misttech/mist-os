// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::future::poll_fn;
use std::sync::{Barrier, Condvar, Mutex};
use std::task::Poll;
use {fuchsia_async as fasync, fuchsia_trace as trace, fuchsia_trace_observer as trace_observer};

#[no_mangle]
pub extern "C" fn rs_test_trace_enabled() -> bool {
    return trace::is_enabled();
}

#[no_mangle]
pub extern "C" fn rs_test_category_disabled() -> bool {
    return trace::category_enabled(c"-disabled");
}

#[no_mangle]
pub extern "C" fn rs_test_category_enabled() -> bool {
    return trace::category_enabled(c"+enabled");
}

#[no_mangle]
pub extern "C" fn rs_test_counter_macro() {
    trace::counter!(c"+enabled", c"name", 42, "arg" => 10);
}

#[no_mangle]
pub extern "C" fn rs_test_instant_macro() {
    trace::instant!(c"+enabled", c"name", trace::Scope::Process, "arg" => 10);
}

#[no_mangle]
pub extern "C" fn rs_test_duration_macro() {
    trace::duration!(c"+enabled", c"name", "x" => 5, "y" => 10);
}

#[no_mangle]
pub extern "C" fn rs_test_duration_macro_with_scope() {
    // N.B. The ordering here is intentional. The duration! macro emits a trace
    // event when the scoped object is dropped. From an output perspective,
    // that means we are looking to see that the instant event occurs first.
    trace::duration!(c"+enabled", c"name", "x" => 5, "y" => 10);
    trace::instant!(c"+enabled", c"name", trace::Scope::Process, "arg" => 10);
}

#[no_mangle]
pub extern "C" fn rs_test_duration_begin_end_macros() {
    trace::duration_begin!(c"+enabled", c"name", "x" => 5);
    trace::instant!(c"+enabled", c"name", trace::Scope::Process, "arg" => 10);
    trace::duration_end!(c"+enabled", c"name", "y" => "foo");
}

#[no_mangle]
pub extern "C" fn rs_test_blob_macro() {
    trace::blob!(c"+enabled", c"name", "blob contents".as_bytes().to_vec().as_slice(), "x" => 5);
}

#[no_mangle]
pub extern "C" fn rs_test_flow_begin_step_end_macros() {
    trace::flow_begin!(c"+enabled", c"name", 123.into(), "x" => 5);
    trace::flow_step!(c"+enabled", c"step", 123.into(), "z" => 42);
    trace::flow_end!(c"+enabled", c"name", 123.into(), "y" => "foo");
}

#[no_mangle]
pub extern "C" fn rs_test_arglimit() {
    trace::duration!(c"+enabled", c"name",
        "1" => 1,
        "2" => 2,
        "3" => 3,
        "4" => 4,
        "5" => 5,
        "6" => 6,
        "7" => 7,
        "8" => 8,
        "9" => 9,
        "10" => 10,
        "11" => 11,
        "12" => 12,
        "13" => 13,
        "14" => 14,
        "15" => 15
    );
}

#[no_mangle]
pub extern "C" fn rs_test_async_event_with_scope() {
    // N.B. The ordering here is intentional. The async_enter! macro emits a trace event when the
    // scoped object is instantiated and when it is dropped. From an output perspective, that means
    // we are looking to see that the instant event occurs sandwiched between the two.
    let _guard = trace::async_enter!(1.into(), c"+enabled", c"name", "x" => 5, "y" => 10);
    trace::instant!(c"+enabled", c"name", trace::Scope::Process, "arg" => 10);
}

#[no_mangle]
pub extern "C" fn rs_test_alert() {
    trace::alert!(c"+enabled", c"alert_name");
}

fn trace_future_test(args: trace::TraceFutureArgs<'_>) {
    let mut executor = fuchsia_async::TestExecutor::new();
    let mut polled = false;
    executor.run_singlethreaded(trace::TraceFuture::new(
        args,
        poll_fn(move |cx| {
            if !polled {
                polled = true;
                cx.waker().clone().wake();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }),
    ))
}

#[no_mangle]
pub extern "C" fn rs_test_trace_future_enabled() {
    trace_future_test(trace::trace_future_args!(c"+enabled", c"name", 3.into()));
}

#[no_mangle]
pub extern "C" fn rs_test_trace_future_enabled_with_arg() {
    trace_future_test(trace::trace_future_args!(c"+enabled", c"name", 3.into(), "arg" => 10));
}

#[no_mangle]
pub extern "C" fn rs_test_trace_future_disabled() {
    trace_future_test(trace::trace_future_args!(c"-disabled", c"name", 3.into()));
}

#[no_mangle]
pub extern "C" fn rs_test_trace_future_disabled_with_arg() {
    #[allow(unreachable_code)]
    trace_future_test(trace::trace_future_args!(
        c"-disabled",
        c"name",
        3.into(),
        "arg" => {
            panic!("arg should not be evaluated");
            ()
        }
    ));
}

static STATE_CV: Condvar = Condvar::new();
static TRACE_STATE: Mutex<fuchsia_trace::TraceState> =
    Mutex::new(fuchsia_trace::TraceState::Stopped);
#[no_mangle]
pub extern "C" fn rs_check_trace_state() -> u32 {
    *TRACE_STATE.lock().unwrap() as u32
}
#[no_mangle]
pub extern "C" fn rs_wait_trace_state_is(expected: u32) {
    let mut state = TRACE_STATE.lock().unwrap();
    while *state as u32 != expected {
        state = STATE_CV.wait(state).unwrap();
    }
}

#[no_mangle]
pub extern "C" fn rs_setup_trace_observer() {
    static BARRIER: Barrier = Barrier::new(2);
    std::thread::spawn(|| {
        let mut executor = fasync::LocalExecutor::new();
        executor.run_singlethreaded(async move {
            let observer = trace_observer::TraceObserver::new();
            BARRIER.wait();
            while let Ok(new_state) = observer.on_state_changed().await {
                let mut state = TRACE_STATE.lock().unwrap();
                *state = new_state;
                STATE_CV.notify_all();
            }
        });
    });
    // Ensure that we've registered our trace_observer in the spawned thread before continuing on
    // and potentially starting the trace session.
    BARRIER.wait();
}
