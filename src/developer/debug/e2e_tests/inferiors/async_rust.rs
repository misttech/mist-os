// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {fuchsia_async as fasync, futures::FutureExt};

#[fasync::run_singlethreaded]
async fn main() {
    let _task = fasync::Task::spawn(async {});
    let block = async {
        fasync::Task::spawn(baz(20)).detach();
        let task = fasync::Task::spawn(baz(21));
        task.await;
    }
    .fuse();
    futures::pin_mut!(block);
    futures::select! {
        _ = foo().fuse() => (),
        _ = bar().fuse() => (),
        _ = block => (),
    };
}

async fn foo() {
    futures::join!(baz(10).boxed(), baz(11).boxed_local());
}

async fn bar() {
    baz(30).fuse().await;
}

async fn baz(i: i64) {
    if i == 21 {
        panic!();
    }
    fasync::Timer::new(fasync::Duration::from_seconds(i)).await;
}
