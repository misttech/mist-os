// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::driver::Driver;
use anyhow::{Context, Result};
use fidl::endpoints::{ClientEnd, ServerEnd};
use fidl::HandleBased;
use fuchsia_component::client;
use futures::channel::oneshot;
use futures::{StreamExt, TryStreamExt};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::{Arc, Weak};
use zx::{AsHandleRef, Status};
use {
    fidl_fuchsia_driver_framework as fidl_fdf, fidl_fuchsia_driver_host as fdh,
    fidl_fuchsia_ldsvc as fldsvc, fidl_fuchsia_system_state as fss,
};

/// We use Weak<Driver> to avoid accidentally extending the lifetime of the Driver. Driver must be
/// droped and have it's destroy hook called in the driver runtime's shutdown observer callback in
/// order to comply with the guarantees the driver framework provides for drivers. The driver host
/// itself doesn't actually ever access the drivers, it strictly uses it for debugging and keeping
/// track of when to shut doesn the driver host.
struct WeakDriver(Weak<Driver>);

impl Ord for WeakDriver {
    fn cmp(&self, other: &WeakDriver) -> std::cmp::Ordering {
        (self.0.as_ptr() as usize).cmp(&(other.0.as_ptr() as usize))
    }
}

impl PartialOrd for WeakDriver {
    fn partial_cmp(&self, other: &WeakDriver) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for WeakDriver {
    fn eq(&self, other: &Self) -> bool {
        (self.0.as_ptr() as usize).eq(&(other.0.as_ptr() as usize))
    }
}

impl Eq for WeakDriver {}

pub(crate) struct DriverHost {
    env: fdf_env::Environment,
    drivers: RefCell<BTreeSet<WeakDriver>>,
    no_more_drivers_signaler: RefCell<Option<oneshot::Sender<()>>>,
    scope: fuchsia_async::Scope,
}

impl DriverHost {
    pub fn new(
        env: fdf_env::Environment,
        no_more_drivers_signaler: oneshot::Sender<()>,
    ) -> DriverHost {
        DriverHost {
            env,
            drivers: RefCell::new(BTreeSet::new()),
            no_more_drivers_signaler: RefCell::new(Some(no_more_drivers_signaler)),
            scope: fuchsia_async::Scope::new(),
        }
    }

    pub async fn run_driver_host_server(self: Rc<Self>, stream: fdh::DriverHostRequestStream) {
        stream
            .map(|result| result.context("failed request"))
            .try_for_each_concurrent(None, |request| {
                let this = self.clone();
                async move {
                    match request {
                        fdh::DriverHostRequest::Start { start_args, driver, responder } => {
                            responder
                                .send(this.start_driver(start_args, driver).await)
                                .or_else(ignore_peer_closed)?;
                        }
                        fdh::DriverHostRequest::StartLoadedDriver {
                            start_args,
                            dynamic_linking_abi,
                            driver,
                            responder,
                        } => {
                            responder
                                .send(
                                    this.start_loaded_driver(
                                        start_args,
                                        dynamic_linking_abi,
                                        driver,
                                    )
                                    .await,
                                )
                                .or_else(ignore_peer_closed)?;
                        }
                        fdh::DriverHostRequest::GetProcessInfo { responder } => {
                            responder.send(get_process_info()).or_else(ignore_peer_closed)?;
                        }
                        fdh::DriverHostRequest::InstallLoader { loader, .. } => {
                            install_loader(loader);
                        }
                    }
                    Ok(())
                }
            })
            .await
            .expect("Failed to handle request")
    }

    async fn start_driver(
        self: Rc<Self>,
        start_args: fidl_fdf::DriverStartArgs,
        request: ServerEnd<fdh::DriverMarker>,
    ) -> Result<(), i32> {
        let (driver, start_args) =
            Driver::load(&self.env, start_args).await.map_err(Status::into_raw)?;
        let (shutdown_signaler, shutdown_event) = oneshot::channel();
        driver
            .start(start_args, request, shutdown_signaler, &self.scope)
            .await
            .map_err(Status::into_raw)?;
        self.drivers.borrow_mut().insert(WeakDriver(Arc::downgrade(&driver)));

        // We carry a weak reference to avoid accidentally extending the lifetime of the
        // driver_host.
        let this = Rc::downgrade(&self);
        self.scope.spawn_local(async move {
            let driver = shutdown_event.await.unwrap();
            if let Some(this) = this.upgrade() {
                let mut drivers = this.drivers.borrow_mut();
                drivers.remove(&WeakDriver(driver));

                // If this is the last driver instance running, we should exit.
                if drivers.is_empty() {
                    // We only exit if we're not shutting down in order to match DFv1 behavior.
                    // TODO(https://fxbug.dev/42075187): We should always exit driver hosts when we
                    // get down to 0 drivers.
                    let client = client::connect_to_protocol::<fss::SystemStateTransitionMarker>()
                        .expect("Could not connect to SystemStateTransition protocol.");
                    match client.get_termination_system_state().await {
                        Err(_) | Ok(fss::SystemPowerState::FullyOn) => (),
                        _ => return,
                    };

                    if let Some(signaler) = this.no_more_drivers_signaler.borrow_mut().take() {
                        signaler.send(()).unwrap();
                    }
                }
            }
        });
        Ok(())
    }

    async fn start_loaded_driver(
        self: Rc<Self>,
        start_args: fidl_fdf::DriverStartArgs,
        dynamic_linking_abi: u64,
        request: ServerEnd<fdh::DriverMarker>,
    ) -> Result<(), i32> {
        let (driver, start_args) = Driver::initialize(&self.env, start_args, dynamic_linking_abi)
            .await
            .map_err(Status::into_raw)?;
        let (shutdown_signaler, shutdown_event) = oneshot::channel();
        driver
            .start(start_args, request, shutdown_signaler, &self.scope)
            .await
            .map_err(Status::into_raw)?;
        self.drivers.borrow_mut().insert(WeakDriver(Arc::downgrade(&driver)));

        // We carry a weak reference to avoid accidentally extending the lifetime of the
        // driver_host.
        let this = Rc::downgrade(&self);
        self.scope.spawn_local(async move {
            let driver = shutdown_event.await.unwrap();
            if let Some(this) = this.upgrade() {
                let mut drivers = this.drivers.borrow_mut();
                drivers.remove(&WeakDriver(driver));

                // If this is the last driver instance running, we should exit.
                if drivers.is_empty() {
                    if let Some(signaler) = this.no_more_drivers_signaler.borrow_mut().take() {
                        signaler.send(()).unwrap();
                    }
                }
            }
        });
        Ok(())
    }
}

impl Drop for DriverHost {
    fn drop(&mut self) {
        // All drivers should now be shutdown and stopped.
        // Destroy all dispatchers in case any weren't freed correctly.
        // This will block until all dispatcher callbacks complete.
        self.env.destroy_all_dispatchers();
    }
}

fn get_process_info(
) -> Result<(u64, u64, &'static [fdh::ThreadInfo], &'static [fdh::DispatcherInfo]), i32> {
    let job_koid =
        fuchsia_runtime::job_default().get_koid().map_err(zx::Status::into_raw)?.raw_koid();
    let process_koid =
        fuchsia_runtime::process_self().get_koid().map_err(Status::into_raw)?.raw_koid();
    static THREAD_INFO: [fdh::ThreadInfo; 0] = [];
    static DISPATCHER_INFO: [fdh::DispatcherInfo; 0] = [];
    Ok((job_koid, process_koid, &THREAD_INFO, &DISPATCHER_INFO))
}

extern "C" {
    fn dl_set_loader_service(handle: zx::sys::zx_handle_t) -> zx::sys::zx_handle_t;
}

fn install_loader(loader: ClientEnd<fldsvc::LoaderMarker>) {
    let loader_handle = loader.into_channel().into_raw();
    // SAFETY: The old loader implementation should be a valid channel which should be closed after
    // it is swapped out.
    let _old_loader = unsafe { zx::Handle::from_raw(dl_set_loader_service(loader_handle)) };
}

fn ignore_peer_closed(err: fidl::Error) -> Result<(), fidl::Error> {
    if err.is_closed() {
        Ok(())
    } else {
        Err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    async fn get_process_info_test() {
        assert!(get_process_info().is_ok());
    }
}
