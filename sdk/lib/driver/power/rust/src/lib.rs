// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::future::Future;
use fdf_component::{Driver, DriverContext};
use futures::TryStreamExt;
use log::{error, warn};
use std::sync::{Arc, Weak};
use zx::Status;
use {fidl_fuchsia_power_system as fpower, fuchsia_async as fasync};

/// Implement this trait if you'd like to get notifications when the system is about to go into
/// suspend and come out of resume.
pub trait SuspendableDriver: Driver {
    /// Called prior the system entering suspend. The system is not guaranteed to enter suspend,
    /// but `resume` will be called regardless before a subsequent suspension attempt occurs.
    fn suspend(&self) -> impl Future<Output = ()> + Send;

    /// Called after `suspend` to indicate the system is no longer in suspend. The system may not
    /// have actually entered suspension in between `suspend` and `resume` invocations.
    fn resume(&self) -> impl Future<Output = ()> + Send;

    /// Returns whether or not suspend is enabled. If false is returned, suspend and resume methods
    /// will never be called.
    fn suspend_enabled(&self) -> bool;
}

/// Wrapper trait to indicate the driver supports power operations.
pub struct Suspendable<T: Driver> {
    #[allow(unused)]
    scope: fasync::Scope,
    driver: Arc<T>,
}

async fn run_suspend_blocker<T: SuspendableDriver>(
    driver: Weak<T>,
    mut service: fpower::SuspendBlockerRequestStream,
) {
    use fpower::SuspendBlockerRequest::*;
    while let Some(req) = service.try_next().await.unwrap() {
        match req {
            BeforeSuspend { responder, .. } => {
                if let Some(driver) = driver.upgrade() {
                    driver.suspend().await;
                } else {
                    return;
                }
                responder.send()
            }
            AfterResume { responder, .. } => {
                if let Some(driver) = driver.upgrade() {
                    driver.resume().await;
                } else {
                    return;
                }
                responder.send()
            }
            // Ignore unknown requests.
            _ => {
                warn!("Received unknown sag listener request");
                Ok(())
            }
        }
        .unwrap()
    }
}

impl<T: SuspendableDriver + Send + Sync> Driver for Suspendable<T> {
    const NAME: &str = T::NAME;

    async fn start(context: DriverContext) -> Result<Self, Status> {
        let sag: fpower::ActivityGovernorProxy =
            context.incoming.connect_protocol().map_err(|err| {
                error!("Error connecting to sag: {err}");
                Status::INTERNAL
            })?;

        let driver = Arc::new(T::start(context).await?);

        let scope = fasync::Scope::new_with_name("suspend");
        if driver.suspend_enabled() {
            let (client, server) = fidl::endpoints::create_endpoints();

            let _ = sag
                .register_suspend_blocker(fpower::ActivityGovernorRegisterSuspendBlockerRequest {
                    suspend_blocker: Some(client),
                    name: Some(Self::NAME.into()),
                    ..Default::default()
                })
                .await
                .map_err(|err| {
                    error!("Error connecting to sag: {err}");
                    Status::INTERNAL
                })?
                .map_err(|err| {
                    error!("Error connecting to sag: {err:?}");
                    Status::INTERNAL
                })?;

            let weak_driver = Arc::downgrade(&driver);
            scope
                .spawn(async move { run_suspend_blocker(weak_driver, server.into_stream()).await });
        }

        Ok(Self { driver, scope })
    }

    async fn stop(&self) {
        self.driver.stop().await;
    }
}
