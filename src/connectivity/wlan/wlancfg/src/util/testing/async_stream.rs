// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#![cfg(test)]

use {
    fuchsia_async as fasync,
    futures::{future::Either, prelude::*, stream::StreamFuture, task::Poll},
    pin_utils::pin_mut,
};

/// Run a background task while waiting for a future that should occur.
/// This is useful for running a task which you expect to produce side effects that
/// mean the task is operating correctly. i.e. reacting to a peer action by producing a
/// response on a client's hanging get.
/// `background_fut` is expected not to finish. If it finishes, this function will panic.
/// Cribbed from bluetooth at src/connectivity/bluetooth/lib/async-helpers/src/test/lib.rs
#[track_caller]
pub fn run_while<BackgroundFut, ResultFut, Out>(
    exec: &mut fasync::TestExecutor,
    background_fut: &mut BackgroundFut,
    result_fut: ResultFut,
) -> Out
where
    BackgroundFut: Future + Unpin,
    ResultFut: Future<Output = Out>,
{
    pin_mut!(result_fut);
    let mut select_fut = futures::future::select(background_fut, result_fut);

    // Set a maximum loop count. After then, panic if the result_fut is still stalled (pending).
    const MAX_LOOP_COUNT: i32 = 2000;
    for _loop_count in 1..MAX_LOOP_COUNT {
        match exec.run_until_stalled(&mut select_fut) {
            Poll::Ready(Either::Left(_)) => panic!("Background future finished"),
            Poll::Ready(Either::Right((out, _background_fut))) => return out,
            Poll::Pending => {}
        }
    }
    panic!("Future failed to produce a result in {} iterations", MAX_LOOP_COUNT);
}

#[track_caller]
pub fn poll_sme_req(
    exec: &mut fasync::TestExecutor,
    next_sme_req: &mut StreamFuture<fidl_fuchsia_wlan_sme::ClientSmeRequestStream>,
) -> Poll<fidl_fuchsia_wlan_sme::ClientSmeRequest> {
    exec.run_until_stalled(next_sme_req).map(|(req, stream)| {
        *next_sme_req = stream.into_future();
        req.expect("did not expect the SME request stream to end")
            .expect("error polling SME request stream")
    })
}

mod tests {
    use {super::*, fuchsia_async as fasync, futures::future};

    #[fuchsia::test]
    fn test_run_while() {
        let mut exec = fasync::TestExecutor::new();
        let neverending_background_fut: future::Pending<bool> = future::pending();
        pin_mut!(neverending_background_fut);

        // You can directly pass in the future
        let result_fut = future::ready(1);
        let result = run_while(&mut exec, &mut neverending_background_fut, result_fut);
        assert_eq!(result, 1);

        // You can pass in a reference to the future
        let result_fut = future::ready(1);
        pin_mut!(result_fut);
        let result = run_while(&mut exec, &mut neverending_background_fut, result_fut);
        assert_eq!(result, 1);
    }
}
