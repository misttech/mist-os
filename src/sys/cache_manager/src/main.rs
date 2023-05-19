// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Context, Error},
    cache_manager_config_lib::Config,
    fidl, fidl_fuchsia_sys2 as fsys, fuchsia_async as fasync,
    fuchsia_component::client as fclient,
    std::process,
    tracing::*,
};

#[fuchsia::main(logging_tags=["cache_manager"])]
async fn main() -> Result<(), Error> {
    info!("cache manager started");
    let config = Config::take_from_startup_handle();
    if config.cache_clearing_threshold > 100 {
        error!(
            "cache clearing threshold is too high, must be <= 100 but it is {}",
            config.cache_clearing_threshold
        );
        process::exit(1);
    }
    info!(
        "cache will be cleared when storage passes {}% capacity",
        config.cache_clearing_threshold
    );
    info!("checking storage every {} milliseconds", config.storage_checking_frequency);

    let storage_admin = fclient::connect_to_protocol::<fsys::StorageAdminMarker>()
        .context("failed opening storage admin")?;

    monitor_storage(&storage_admin, config).await;
    Ok(())
}

#[derive(PartialEq, Debug)]
struct StorageState {
    total_bytes: u64,
    used_bytes: u64,
}

impl StorageState {
    fn percent_used(&self) -> u64 {
        if self.total_bytes > 0 {
            self.used_bytes * 100 / self.total_bytes
        } else {
            0
        }
    }
}

async fn monitor_storage(storage_admin: &fsys::StorageAdminProxy, config: Config) {
    // Sleep for the check interval, then see if we're over the clearing threshold.
    // If we are over the threshold, clear the cache. This panics if we lose the
    // connect to the StorageAdminProtocol.

    error!(
        "cache will be cleared when storage passes {}% capacity",
        config.cache_clearing_threshold
    );
    error!("checking storage every {} milliseconds", config.storage_checking_frequency);

    loop {
        fasync::Timer::new(std::time::Duration::from_millis(config.storage_checking_frequency))
            .await;
        let storage_state = {
            match get_storage_utilization(&storage_admin).await {
                Ok(utilization) => {
                    if utilization.percent_used() > 100 {
                        warn!("storage utlization is above 100%, clearing storage.");
                    }
                    utilization
                }
                Err(e) => match e.downcast_ref::<fidl::Error>() {
                    Some(fidl::Error::ClientChannelClosed { .. }) => {
                        panic!(
                            "cache manager's storage admin channel closed unexpectedly, \
                            is component manager dead?"
                        );
                    }
                    _ => {
                        error!("failed getting cache utilization, will try again later: {:?}", e);
                        continue;
                    }
                },
            }
        };

        // Not enough storage is used, sleep and wait for changes
        if storage_state.percent_used() < config.cache_clearing_threshold {
            continue;
        }

        // Clear the cache
        info!("storage utilization is at {}%, which is above our threshold of {}%, beginning to clear cache storage", storage_state.percent_used(), config.cache_clearing_threshold);

        match clear_cache_storage(&storage_admin).await {
            Err(e) => match e.downcast_ref::<fidl::Error>() {
                Some(fidl::Error::ClientChannelClosed { .. }) => {
                    panic!(
                        "cache manager's storage admin channel closed while clearing storage \
                        is component manager dead?"
                    );
                }
                _ => {
                    error!("non-fatal error while clearing cache: {:?}", e);
                    continue;
                }
            },
            _ => {}
        }

        let storage_state_after = match get_storage_utilization(&storage_admin).await {
            Err(e) => match e.downcast_ref::<fidl::Error>() {
                Some(fidl::Error::ClientChannelClosed { .. }) => {
                    panic!(
                        "cache manager's storage admin channel closed while checking utlization \
                            after cache clearing, is component manager dead?"
                    );
                }
                _ => {
                    error!("non-fatal getting storage utlization {:?}", e);
                    continue;
                }
            },
            Ok(u) => u,
        };

        if storage_state.percent_used() > config.cache_clearing_threshold {
            warn!("storage usage still exceeds threshold after cache clearing, used_bytes={} total_bytes={}", storage_state.used_bytes, storage_state.total_bytes);
        }

        if storage_state_after.percent_used() >= storage_state.percent_used() {
            warn!("cache manager did not reduce storage pressure");
        }
    }
}

/// Check the current cache storage utilization. If no components are using cache, the utilization
/// reported by the filesystem is not checked and utlization is reported as zero.
async fn get_storage_utilization(
    storage_admin: &fsys::StorageAdminProxy,
) -> Result<StorageState, Error> {
    let utilization = match storage_admin.get_status().await? {
        Ok(u) => u,
        Err(e) => {
            return Err(format_err!(
                "RPC to get storage status succeeded, but server returned an error: {:?}",
                e
            ));
        }
    };

    Ok(StorageState {
        total_bytes: utilization.total_size.unwrap(),
        used_bytes: utilization.used_size.unwrap(),
    })
}

/// Tries to delete the cache for all components. Failures for individual components are ignored,
/// but if `storage_admin` is closed that is reported as the `fidl::Error::ClientChannelClosed`
/// that it is.
async fn clear_cache_storage(storage_admin: &fsys::StorageAdminProxy) -> Result<(), Error> {
    storage_admin
        .delete_all_storage_contents()
        .await?
        .map_err(|err| format_err!("protocol error clearing cache: {:?}", err))
}

#[cfg(test)]
mod tests {
    use {
        crate::monitor_storage,
        cache_manager_config_lib::Config,
        fidl::endpoints::{ClientEnd, ServerEnd},
        fidl_fuchsia_sys2 as fsys,
        fuchsia_async::{self as fasync, Duration, TestExecutor},
        futures::{
            channel::mpsc::{self as mpsc, UnboundedReceiver},
            TryStreamExt,
        },
        std::{boxed::Box, future::Future, pin::Pin},
    };

    struct FakeStorageServer {
        storage_statuses: Vec<fsys::StorageStatus>,
        chan: ServerEnd<fsys::StorageAdminMarker>,
    }

    #[derive(Debug, PartialEq)]
    enum CallType {
        Status,
        Delete,
    }

    impl FakeStorageServer {
        /// Run the fake server. The server sends monikers it receives it storage deletion requests
        /// over the `moniker_channel`.
        pub fn run_server(
            mut self,
            call_chan: mpsc::UnboundedSender<CallType>,
        ) -> Pin<Box<impl Future<Output = ()>>> {
            let server = async move {
                let mut req_stream = self.chan.into_stream().unwrap();
                while let Ok(request) = req_stream.try_next().await {
                    match request {
                        Some(fsys::StorageAdminRequest::DeleteAllStorageContents { responder }) => {
                            call_chan.unbounded_send(CallType::Delete).unwrap();
                            let _ = responder.send(Ok(()));
                        }
                        Some(fsys::StorageAdminRequest::GetStatus { responder }) => {
                            call_chan.unbounded_send(CallType::Status).unwrap();
                            // note that we can panic here, but that is okay because if more
                            // status values are requested than expect that is also an error
                            let status = self.storage_statuses.remove(0);
                            let _ = responder.send(Ok(&status));
                        }
                        None => return,
                        _ => panic!("unexpected call not supported by fake server"),
                    }
                }
            };
            Box::pin(server)
        }
    }

    fn common_setup(
        server: Option<FakeStorageServer>,
        client: ClientEnd<fsys::StorageAdminMarker>,
    ) -> (UnboundedReceiver<CallType>, TestExecutor, Duration, fsys::StorageAdminProxy, Config)
    {
        let (calls_tx, calls_rx) = futures::channel::mpsc::unbounded::<CallType>();
        let exec = TestExecutor::new_with_fake_time();
        let time_step = Duration::from_millis(5000);
        let config = Config {
            cache_clearing_threshold: 20,
            storage_checking_frequency: time_step.clone().into_millis().try_into().unwrap(),
        };
        if let Some(server) = server {
            fasync::Task::spawn(server.run_server(calls_tx)).detach();
        }
        let client = client.into_proxy().unwrap();
        (calls_rx, exec, time_step, client, config)
    }

    /// Advance the TestExecutor by |time_step| and wake expired timers.
    fn advance_time_and_wake(exec: &mut TestExecutor, time_step: &Duration) {
        let new_time =
            fasync::Time::from_nanos(exec.now().into_nanos() + time_step.clone().into_nanos());
        exec.set_fake_time(new_time);
        exec.wake_expired_timers();
    }

    #[test]
    fn test_typical_case() {
        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fsys::StorageAdminMarker>();

        // Create a take storage server with the canned call responses for
        // GetStatus that we want.
        let server = FakeStorageServer {
            storage_statuses: vec![
                fsys::StorageStatus {
                    total_size: Some(100),
                    used_size: Some(10),
                    ..Default::default()
                },
                fsys::StorageStatus {
                    total_size: Some(100),
                    used_size: Some(50),
                    ..Default::default()
                },
                fsys::StorageStatus {
                    total_size: Some(100),
                    used_size: Some(0),
                    ..Default::default()
                },
                fsys::StorageStatus {
                    total_size: Some(100),
                    used_size: Some(0),
                    ..Default::default()
                },
            ],
            chan: server_end,
        };

        let (mut calls_rx, mut exec, time_step, client, config) =
            common_setup(Some(server), client_end);
        let mut monitor = Box::pin(monitor_storage(&client, config));
        let _ = exec.run_until_stalled(&mut monitor);

        // We expect no query sent to the capability provider since it sleeps first
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Err(()));

        // Move forward to the first check
        advance_time_and_wake(&mut exec, &time_step);
        let _ = exec.run_until_stalled(&mut monitor);

        // We expect a status check
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Status)));

        // Since the reported usage is below the threshold, the monitor should do nothing.
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Err(()));

        // Move forward to the next check
        advance_time_and_wake(&mut exec, &time_step);
        let _ = exec.run_until_stalled(&mut monitor);

        // Expect a check, where we'll report we're above the cache clearing threshold
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Status)));

        // Expect that the monitor tries to clear storage
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Delete)));

        // Monitor checks after clearing storage
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Status)));

        // No call is expected until the timer goes off
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Err(()));

        // advance time
        advance_time_and_wake(&mut exec, &time_step);
        let _ = exec.run_until_stalled(&mut monitor);

        // Monitor checks again and we'll respond that usage is below the threshold
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Status)));

        // There should be no call until the next check
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Err(()));
    }

    #[test]
    /// Test the scenario where clearing storage doesn't lower utilization below the clearing
    /// threshold. In this case we expect the call behavior to be the same as "normal" operrations.
    fn test_utilization_stays_above_threshold() {
        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fsys::StorageAdminMarker>();

        // Create a take storage server with the canned call responses for
        // GetStatus that we want.
        let server = FakeStorageServer {
            storage_statuses: vec![
                fsys::StorageStatus {
                    total_size: Some(100),
                    used_size: Some(10),
                    ..Default::default()
                },
                fsys::StorageStatus {
                    total_size: Some(100),
                    used_size: Some(50),
                    ..Default::default()
                },
                fsys::StorageStatus {
                    total_size: Some(100),
                    used_size: Some(50),
                    ..Default::default()
                },
                fsys::StorageStatus {
                    total_size: Some(100),
                    used_size: Some(50),
                    ..Default::default()
                },
                fsys::StorageStatus {
                    total_size: Some(100),
                    used_size: Some(50),
                    ..Default::default()
                },
            ],
            chan: server_end,
        };

        let (mut calls_rx, mut exec, time_step, client, config) =
            common_setup(Some(server), client_end);
        let mut monitor = Box::pin(monitor_storage(&client, config));
        let _ = exec.run_until_stalled(&mut monitor);

        // We expect no query sent to the capability provider since it sleeps first
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Err(()));

        // Move forward to the first check
        advance_time_and_wake(&mut exec, &time_step);
        let _ = exec.run_until_stalled(&mut monitor);

        // We expect a status check
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Status)));

        // Since the reported usage is below the threshold, the monitor should do nothing.
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Err(()));

        // Move forward to the next check
        advance_time_and_wake(&mut exec, &time_step);
        let _ = exec.run_until_stalled(&mut monitor);

        // Expect a check, where we'll report we're above the cache clearing threshold
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Status)));

        // Expect that the monitor tries to clear storage
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Delete)));

        // Monitor checks after clearing storage
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Status)));

        // No call is expected until the timer goes off
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Err(()));

        // advance time
        advance_time_and_wake(&mut exec, &time_step);
        let _ = exec.run_until_stalled(&mut monitor);

        // Monitor checks again and we'll respond that usage is below the threshold
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Status)));

        // Expect that the monitor tries to clear storage again on the next run
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Delete)));

        // Monitor checks after clearing storage
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Ok(Some(CallType::Status)));

        // There should be no call until the next check
        assert_eq!(calls_rx.try_next().map_err(|_| ()), Err(()));
    }

    #[test]
    #[should_panic]
    fn test_channel_closure_panics() {
        let (client_end, server_end) =
            fidl::endpoints::create_endpoints::<fsys::StorageAdminMarker>();

        let (mut _calls_rx, mut exec, time_step, client, config) = common_setup(None, client_end);
        let mut monitor = Box::pin(monitor_storage(&client, config));
        let _ = exec.run_until_stalled(&mut monitor);

        drop(server_end);

        // Move forward to the first check
        advance_time_and_wake(&mut exec, &time_step);
        let _ = exec.run_until_stalled(&mut monitor);
    }
}
