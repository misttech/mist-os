// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::reboot_reasons::RebootReasons;

use either::Either;
use fidl::endpoints::Proxy;
use fidl_fuchsia_hardware_power_statecontrol as fpower;
use fuchsia_async::{self as fasync, DurationExt, TimeoutExt};
use futures::lock::Mutex;
use futures::prelude::*;
use futures::TryStreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use zx::AsHandleRef;

/// Summary: Provides an implementation of the
/// fuchsia.hardware.power.statecontrol.RebootMethodsWatcherRegister protocol that allows other
/// components in the system to register to be notified of pending system shutdown requests and the
/// associated shutdown reason.
///
/// FIDL dependencies:
///     - fuchsia.hardware.power.statecontrol.RebootMethodsWatcherRegister: the serverprovides a
///       Register API that other components in the system can use
///       to receive notifications about system shutdown events and reasons
///     - fuchsia.hardware.power.statecontrol.RebootMethodsWatcher: the server receives an instance
///       of this protocol over the RebootMethodsWatcherRegister channel, and uses this channel to
///       send shutdown notifications
pub struct ShutdownWatcher {
    /// Contains all the registered RebootMethodsWatcher channels to be notified when a reboot
    /// request is received.
    reboot_watchers: Arc<
        Mutex<
            // TODO(https://fxbug.dev/385742868): The value is the proxy (either
            // deprecated or not). Clean this up once the deprecated version is
            // removed from the API.
            HashMap<u32, Either<fpower::RebootMethodsWatcherProxy, fpower::RebootWatcherProxy>>,
        >,
    >,
}

impl ShutdownWatcher {
    const NOTIFY_RESPONSE_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(
        fpower::MAX_REBOOT_WATCHER_RESPONSE_TIME_SECONDS as i64,
    );

    pub fn new() -> Arc<Self> {
        Arc::new(Self { reboot_watchers: Arc::new(Mutex::new(HashMap::new())) })
    }

    /// Handles a new client connection to the RebootMethodsWatcherRegister service.
    pub async fn handle_reboot_register_request(
        self: Arc<Self>,
        mut stream: fpower::RebootMethodsWatcherRegisterRequestStream,
    ) {
        while let Ok(Some(req)) = stream.try_next().await {
            match req {
                // TODO(https://fxbug.dev/385742868): Delete this method
                // once it's removed from the API.
                fpower::RebootMethodsWatcherRegisterRequest::Register {
                    watcher,
                    control_handle: _,
                } => {
                    self.add_deprecated_reboot_watcher(watcher.into_proxy()).await;
                }
                // TODO(https://fxbug.dev/385742868): Delete this method
                // once it's removed from the API.
                fpower::RebootMethodsWatcherRegisterRequest::RegisterWithAck {
                    watcher,
                    responder,
                } => {
                    self.add_deprecated_reboot_watcher(watcher.into_proxy()).await;
                    let _ = responder.send();
                }
                fpower::RebootMethodsWatcherRegisterRequest::RegisterWatcher {
                    watcher,
                    responder,
                } => {
                    self.add_reboot_watcher(watcher.into_proxy()).await;
                    let _ = responder.send();
                }
            }
        }
    }

    /// Adds a new RebootMethodsWatcher channel to the list of registered watchers.
    async fn add_deprecated_reboot_watcher(&self, watcher: fpower::RebootMethodsWatcherProxy) {
        fuchsia_trace::duration!(
            c"shutdown-shim",
            c"ShutdownWatcher::add_deprecated_reboot_watcher",
            "watcher" => watcher.as_channel().raw_handle()
        );

        println!("[shutdown-shim] Adding a deprecated reboot watcher");
        // If the client closes the watcher channel, remove it from our `reboot_watchers` map and
        // notify all clients
        let key = watcher.as_channel().raw_handle();
        let proxy = watcher.clone();
        let reboot_watchers = self.reboot_watchers.clone();
        fasync::Task::spawn(async move {
            let _ = proxy.on_closed().await;
            let mut watchers_mut = reboot_watchers.lock().await;
            watchers_mut.remove(&key);
        })
        .detach();

        let mut watchers_mut = self.reboot_watchers.lock().await;
        watchers_mut.insert(key, Either::Left(watcher));
    }

    /// Adds a new RebootMethodsWatcher channel to the list of registered watchers.
    async fn add_reboot_watcher(&self, watcher: fpower::RebootWatcherProxy) {
        fuchsia_trace::duration!(
            c"shutdown-shim",
            c"ShutdownWatcher::add_reboot_watcher",
            "watcher" => watcher.as_channel().raw_handle()
        );

        // If the client closes the watcher channel, remove it from our `reboot_watchers` map
        println!("[shutdown-shim] Adding a reboot watcher");
        let key = watcher.as_channel().raw_handle();
        let proxy = watcher.clone();
        let reboot_watchers = self.reboot_watchers.clone();
        fasync::Task::spawn(async move {
            let _ = proxy.on_closed().await;
            let mut watchers_mut = reboot_watchers.lock().await;
            watchers_mut.remove(&key);
        })
        .detach();

        let mut watchers_mut = self.reboot_watchers.lock().await;
        watchers_mut.insert(key, Either::Right(watcher));
    }

    /// Handles the SystemShutdown message by notifying the appropriate registered watchers.
    pub async fn handle_system_shutdown_message(&self, reasons: RebootReasons) {
        self.notify_reboot_watchers(reasons, Self::NOTIFY_RESPONSE_TIMEOUT).await
    }

    async fn notify_reboot_watchers(&self, reasons: RebootReasons, timeout: zx::MonotonicDuration) {
        // TODO(https://fxbug.dev/42120903): This string must live for the duration of the function because the
        // trace macro uses it when the function goes out of scope. Therefore, it must be bound here
        // and not used anonymously at the macro callsite.
        let reasons_str = format!("{:?}", reasons);
        fuchsia_trace::duration!(
            c"shutdown-shim",
            c"ShutdownWatcher::notify_reboot_watchers",
            "reasons" => reasons_str.as_str()
        );

        // Create a future for each watcher that calls the watcher's `on_reboot` method and returns
        // the watcher proxy if the response was received within the timeout, or None otherwise. We
        // take this approach so that watchers that timed out have their channel dropped
        // (https://fxbug.dev/42131208).
        let watcher_futures = {
            // Take the current watchers out of the RefCell because we'll be modifying the vector
            let watchers = self.reboot_watchers.lock().await;
            println!(
                "[shutdown-shim] Notifying {:?} watchers of reboot notification",
                watchers.len()
            );
            watchers.clone().into_iter().map(|(key, watcher_proxy)| {
                let reasons = reasons.clone();
                async move {
                    let deadline = timeout.after_now();
                    let result = match &watcher_proxy {
                        Either::Left(proxy) => {
                            futures::future::Either::Left(proxy.on_reboot(reasons.to_deprecated()))
                        }
                        Either::Right(proxy) => {
                            futures::future::Either::Right(proxy.on_reboot(&reasons.into()))
                        }
                    }
                    .map_err(|_| ())
                    .on_timeout(deadline, || Err(()))
                    .await;

                    match result {
                        Ok(()) => Some((key, watcher_proxy)),
                        Err(()) => None,
                    }
                }
            })
        };

        // Run all of the futures, collecting the successful watcher proxies into a vector
        let new_watchers = futures::future::join_all(watcher_futures)
            .await
            .into_iter()
            .filter_map(|watcher_opt| watcher_opt) // Unwrap the Options while filtering out None
            .collect();

        // Repopulate the successful watcher proxies back into the `reboot_watchers` RefCell
        *self.reboot_watchers.lock().await = new_watchers;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl::endpoints::{ControlHandle, RequestStream};

    // In this test, convert float to integer for simpilification
    fn seconds(seconds: f64) -> zx::MonotonicDuration {
        zx::MonotonicDuration::from_seconds(seconds as i64)
    }

    /// Tests that a client can successfully register a reboot watcher, and the registered watcher
    /// receives the expected reboot notification.
    #[fasync::run_singlethreaded(test)]
    async fn test_add_reboot_watcher() {
        let registrar = ShutdownWatcher::new();

        // Create the proxy/stream to register the watcher
        let (register_proxy, register_stream) = fidl::endpoints::create_proxy_and_stream::<
            fpower::RebootMethodsWatcherRegisterMarker,
        >();

        // Start the RebootMethodsWatcherRegister server that will handle Register calls from
        // register_proxy
        let registrar_clone = registrar.clone();
        fasync::Task::local(async move {
            registrar_clone.handle_reboot_register_request(register_stream).await;
        })
        .detach();

        // Create the watcher proxy/stream to receive reboot notifications
        let (watcher_client, mut watcher_stream) =
            fidl::endpoints::create_request_stream::<fpower::RebootWatcherMarker>();

        // Call the Register API, passing in the watcher_client end
        assert_matches!(register_proxy.register_watcher(watcher_client).await, Ok(()));
        // Signal the watchers
        registrar
            .notify_reboot_watchers(
                RebootReasons::new(fpower::RebootReason2::UserRequest),
                seconds(0.0),
            )
            .await;

        // Verify the watcher_stream gets the correct reboot notification
        let reasons = assert_matches!(
            watcher_stream.try_next().await.unwrap().unwrap(),
            fpower::RebootWatcherRequest::OnReboot {
                options: fpower::RebootOptions{reasons: Some(reasons), ..},
                ..
            } => reasons
        );
        assert_eq!(&reasons[..], [fpower::RebootReason2::UserRequest]);
    }

    /// Tests that a reboot watcher is delivered the correct reboot reason
    #[fasync::run_singlethreaded(test)]
    async fn test_reboot_watcher_reason() {
        let registrar = ShutdownWatcher::new();
        let (watcher_proxy, mut watcher_stream) =
            fidl::endpoints::create_proxy_and_stream::<fpower::RebootWatcherMarker>();
        registrar.add_reboot_watcher(watcher_proxy).await;
        registrar
            .notify_reboot_watchers(
                RebootReasons::new(fpower::RebootReason2::HighTemperature),
                seconds(0.0),
            )
            .await;

        let reasons = match watcher_stream.try_next().await {
            Ok(Some(fpower::RebootWatcherRequest::OnReboot {
                options: fpower::RebootOptions { reasons: Some(reasons), .. },
                ..
            })) => reasons,
            e => panic!("Unexpected watcher_stream result: {:?}", e),
        };

        assert_eq!(&reasons[..], [fpower::RebootReason2::HighTemperature]);
    }

    /// Tests that if there are multiple registered reboot watchers, each one will receive the
    /// expected reboot notification.
    #[fasync::run_singlethreaded(test)]
    async fn test_multiple_reboot_watchers() {
        let registrar = ShutdownWatcher::new();

        // Create three separate reboot watchers
        let (watcher_proxy1, mut watcher_stream1) =
            fidl::endpoints::create_proxy_and_stream::<fpower::RebootWatcherMarker>();
        registrar.add_reboot_watcher(watcher_proxy1).await;

        let (watcher_proxy2, mut watcher_stream2) =
            fidl::endpoints::create_proxy_and_stream::<fpower::RebootWatcherMarker>();
        registrar.add_reboot_watcher(watcher_proxy2).await;

        let (watcher_proxy3, mut watcher_stream3) =
            fidl::endpoints::create_proxy_and_stream::<fpower::RebootWatcherMarker>();
        registrar.add_reboot_watcher(watcher_proxy3).await;

        // Close the channel of the first watcher to verify the registrar still correctly notifies the
        // second and third watchers
        watcher_stream1.control_handle().shutdown();

        registrar
            .notify_reboot_watchers(
                RebootReasons::new(fpower::RebootReason2::HighTemperature),
                seconds(0.0),
            )
            .await;

        // The first watcher should get None because its channel was closed
        match watcher_stream1.try_next().await {
            Ok(None) => {}
            e => panic!("Unexpected watcher_stream1 result: {:?}", e),
        };

        // Verify the watcher received the correct OnReboot request
        match watcher_stream2.try_next().await {
            Ok(Some(fpower::RebootWatcherRequest::OnReboot {
                options: fpower::RebootOptions { reasons: Some(reasons), .. },
                ..
            })) => {
                assert_eq!(&reasons[..], [fpower::RebootReason2::HighTemperature])
            }
            e => panic!("Unexpected watcher_stream2 result: {:?}", e),
        };

        // Verify the watcher received the correct OnReboot request
        match watcher_stream3.try_next().await {
            Ok(Some(fpower::RebootWatcherRequest::OnReboot {
                options: fpower::RebootOptions { reasons: Some(reasons), .. },
                ..
            })) => assert_eq!(&reasons[..], [fpower::RebootReason2::HighTemperature]),
            e => panic!("Unexpected watcher_stream3 result: {:?}", e),
        };
    }

    #[test]
    fn test_watcher_response_delay() {
        let mut exec = fasync::TestExecutor::new();
        let registrar = ShutdownWatcher::new();

        // Register the reboot watcher
        let (watcher_proxy, mut watcher_stream) =
            fidl::endpoints::create_proxy_and_stream::<fpower::RebootWatcherMarker>();
        let fut = async {
            registrar.add_reboot_watcher(watcher_proxy).await;
            assert_eq!(registrar.reboot_watchers.lock().await.len(), 1);
        };
        exec.run_singlethreaded(fut);

        // Set up the notify future
        let notify_future = registrar.notify_reboot_watchers(
            RebootReasons::new(fpower::RebootReason2::HighTemperature),
            seconds(1.0),
        );
        futures::pin_mut!(notify_future);

        // Verify that the notify future can't complete on the first attempt (because the watcher
        // will not have responded)
        assert!(exec.run_until_stalled(&mut notify_future).is_pending());

        // Ack the reboot notification, allowing the shutdown flow to continue
        let fpower::RebootWatcherRequest::OnReboot { responder, .. } =
            exec.run_singlethreaded(&mut watcher_stream.try_next()).unwrap().unwrap();
        assert_matches!(responder.send(), Ok(()));

        // Verify the notify future can now complete
        assert!(exec.run_until_stalled(&mut notify_future).is_ready());
    }

    /// Tests that a reboot watcher is able to delay the shutdown but will time out after the
    /// expected duration. The test also verifies that when a watcher times out, it is removed
    /// from the list of registered reboot watchers.
    #[test]
    fn test_watcher_response_timeout() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        let registrar = ShutdownWatcher::new();
        exec.set_fake_time(fasync::MonotonicInstant::from_nanos(0));

        // Register the reboot watcher
        let (watcher_proxy, _watcher_stream) =
            fidl::endpoints::create_proxy_and_stream::<fpower::RebootWatcherMarker>();
        let fut = async {
            registrar.add_reboot_watcher(watcher_proxy).await;
            assert_eq!(registrar.reboot_watchers.lock().await.len(), 1);
        };
        futures::pin_mut!(fut);
        exec.run_until_stalled(&mut fut).is_ready();

        // Set up the notify future
        let notify_future = registrar.notify_reboot_watchers(
            RebootReasons::new(fpower::RebootReason2::HighTemperature),
            seconds(1.0),
        );
        futures::pin_mut!(notify_future);

        // Verify that the notify future can't complete on the first attempt (because the watcher
        // will not have responded)
        assert!(exec.run_until_stalled(&mut notify_future).is_pending());

        // Wake the timer that causes the watcher timeout to fire
        assert_eq!(exec.wake_next_timer(), Some(fasync::MonotonicInstant::from_nanos(1e9 as i64)));

        // Verify the notify future can now complete
        assert!(exec.run_until_stalled(&mut notify_future).is_ready());

        // Since the watcher timed out, verify it is removed from `reboot_watchers`
        let fut = async {
            assert_eq!(registrar.reboot_watchers.lock().await.len(), 0);
        };
        futures::pin_mut!(fut);
        exec.run_until_stalled(&mut fut).is_ready();
    }

    /// Tests that an unsuccessful RebootWatcher registration results in the
    /// RebootMethodsWatcherRegister channel being closed.
    #[fasync::run_singlethreaded(test)]
    async fn test_watcher_register_fail() {
        let registrar = ShutdownWatcher::new();

        // Create the registration proxy/stream
        let (register_proxy, register_stream) = fidl::endpoints::create_proxy_and_stream::<
            fpower::RebootMethodsWatcherRegisterMarker,
        >();

        // Start the RebootMethodsWatcherRegister server that will handle Register requests from
        // `register_proxy`
        fasync::Task::local(async move {
            registrar.handle_reboot_register_request(register_stream).await;
        })
        .detach();

        // Send an invalid request to the server to force a failure
        assert_matches!(register_proxy.as_channel().write(&[], &mut []), Ok(()));

        // Verify the RebootMethodsWatcherRegister channel is closed
        assert_matches!(register_proxy.on_closed().await, Ok(zx::Signals::CHANNEL_PEER_CLOSED));
    }
}
