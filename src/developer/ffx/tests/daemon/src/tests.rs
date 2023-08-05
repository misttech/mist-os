// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_testing::{base_fixture, TestContext};
use fixture::fixture;
use futures::{channel::oneshot, Future};
use std::time::{Duration, Instant};

/// Benchmark the time from daemon process start -> daemon socket created -> daemon socket
/// connection accepted. Expect that it is below reasonable thresholds.
#[fixture(base_fixture)]
#[fuchsia::test]
async fn test_daemon(ctx: TestContext) {
    // Timing guarantees:
    const SPAWN_TO_SOCKET_BIND: Duration = {
        // b/294442636:
        // rustls on macOS parses all TLS certificates from the system certificate store to use
        // as root certs. This takes a very long time and happens for each ffx invocation even if
        // analytics are off.
        #[cfg(target_os = "macos")]
        {
            Duration::from_millis(1200)
        }

        #[cfg(not(target_os = "macos"))]
        {
            Duration::from_millis(100)
        }
    };

    // b/294445393:
    // rustls again does heavy work on the main thread when creating the Hyper HTTP client.
    // This timeout is long to accomodate this.
    const SOCKET_BIND_TO_ACCEPT: Duration = Duration::from_millis(1500);

    let isolate = ctx.isolate();

    // Ensure the watcher has been created before spawning the daemon.
    let on_created = wait_for_daemon_socket(&ctx);

    // Run the daemon manually to avoid inheriting any logic from `isolate.start_daemon`.
    let process_spawned_at = Instant::now();
    let _daemon =
        ffx_daemon::run_daemon(isolate.env_context()).await.expect("Could not start daemon");

    on_created.await;

    let socket_bound_at = Instant::now();

    let socket_bound_delay = socket_bound_at.duration_since(process_spawned_at);

    eprintln!("Spawn -> Socket Bound: {:?} (< {:?})", socket_bound_delay, SPAWN_TO_SOCKET_BIND);

    let socket_connect_at = Instant::now();

    let output = isolate.ffx(&["daemon", "echo"]).await.unwrap();
    eprintln!("{}", output.stderr);
    assert!(output.status.success());

    let socket_accept_at = Instant::now();

    let socket_accept_delay = socket_accept_at.duration_since(socket_connect_at);

    eprintln!(
        "Socket Connect -> Socket Accept: {:?} (< {:?})",
        socket_accept_delay, SOCKET_BIND_TO_ACCEPT
    );

    assert!(socket_bound_delay < SPAWN_TO_SOCKET_BIND);
    assert!(socket_accept_delay < SOCKET_BIND_TO_ACCEPT);
}

fn wait_for_daemon_socket(ctx: &TestContext) -> impl Future<Output = ()> {
    use notify::{event::Event, Watcher};

    let isolate = ctx.isolate();

    let (sender, receiver) = oneshot::channel();

    let mut watcher = notify::RecommendedWatcher::new(
        {
            let mut sender = Some(sender);
            let socket_path = isolate.ascendd_path();
            move |res: Result<Event, notify::Error>| {
                let event = res.expect("File watcher encountered an error");

                eprintln!("{:?}", event);

                if socket_path.exists() {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(());
                    }
                }
            }
        },
        notify::Config::default().with_poll_interval(Duration::from_millis(100)),
    )
    .expect("Could not create daemon socket watcher");

    watcher
        .watch(isolate.dir(), notify::RecursiveMode::NonRecursive)
        .expect("Could not watch isolate directory");

    async move {
        receiver.await.expect("Failed to wait for daemon socket creation");

        // Make sure to keep the watcher alive while we wait.
        drop(watcher);
    }
}
