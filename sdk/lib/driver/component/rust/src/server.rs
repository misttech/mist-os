// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::ffi::c_void;
use core::ptr::NonNull;
use std::num::NonZero;
use std::ops::ControlFlow;
use std::sync::OnceLock;

use log::{debug, warn};
use zx::Status;

use fdf::{Channel, DispatcherBuilder, DispatcherRef};
use fidl_fuchsia_driver_framework::DriverRequest;

use fdf::{fdf_handle_t, DriverHandle, Message};

use crate::{Driver, DriverContext};
use fdf_sys::fdf_dispatcher_get_current_dispatcher;
use fidl_fuchsia_driver_framework::DriverStartArgs;

/// Implements the lifecycle management of a rust driver, including starting and stopping it
/// and setting up the rust async dispatcher and logging for the driver to use, and running a
/// message loop for the driver start and stop messages.
pub struct DriverServer<T> {
    server_handle: OnceLock<Channel<[u8]>>,
    root_dispatcher: DispatcherRef<'static>,
    driver: OnceLock<T>,
}

impl<T: Driver> DriverServer<T> {
    /// Called by the driver host to start the driver.
    ///
    /// # Safety
    ///
    /// The caller must provide a valid non-zero driver transport channel handle for
    /// `server_handle`.
    pub unsafe extern "C" fn initialize(server_handle: fdf_handle_t) -> *mut c_void {
        // SAFETY: We verify that the pointer returned is non-null, ensuring that this was
        // called from within a driver context.
        let root_dispatcher = NonNull::new(unsafe { fdf_dispatcher_get_current_dispatcher() })
            .expect("Non-null current dispatcher");
        // SAFETY: We use NonZero::new to verify that we've been given a non-zero
        // driver handle, and expect that the caller (which is the driver runtime) has given us
        // a valid driver transport fidl channel.
        let server_handle = OnceLock::from(unsafe {
            Channel::from_driver_handle(DriverHandle::new_unchecked(
                NonZero::new(server_handle).expect("valid driver handle"),
            ))
        });

        // SAFETY: the root dispatcher is expected to live as long as this driver is loaded.
        let root_dispatcher = unsafe { DispatcherRef::from_raw(root_dispatcher) };
        // We leak the box holding the server so that the driver runtime can take control over the
        // lifetime of the server object.
        let server_ptr = Box::into_raw(Box::new(Self {
            server_handle,
            root_dispatcher: root_dispatcher.clone(),
            driver: OnceLock::default(),
        }));

        // Reconstitute the pointer to the `DriverServer` as a mut reference to use it in the main
        // loop.
        // SAFETY: We are the exclusive owner of the object until we drop the server handle,
        // triggering the driver host to call `destroy`.
        let server = unsafe { &mut *server_ptr };

        // Build a new dispatcher that we can have spin on a fuchsia-async executor main loop
        // to act as a reactor for non-driver events.
        let rust_async_dispatcher = DispatcherBuilder::new()
            .name("fuchsia-async")
            .allow_thread_blocking()
            .create_released()
            .expect("failure creating blocking dispatcher for rust async");
        // Post the task to the dispatcher that will run the fuchsia-async loop, and have it run
        // the server's message loop waiting for start and stop messages from the driver host.
        rust_async_dispatcher
            .post_task_sync(move |status| {
                // bail immediately if we were somehow cancelled before we started
                let Status::OK = status else { return };
                // create and run a fuchsia-async executor, giving it the "root" dispatcher to actually
                // execute driver tasks on, as this thread will be effectively blocked by the reactor
                // loop.
                let mut executor = fuchsia_async::LocalExecutor::new();
                executor.run_singlethreaded(async move {
                    server.message_loop(root_dispatcher.clone()).await;
                    // take the server handle so it can drop after the async block is done,
                    // which will signal to the driver host that the driver has finished shutdown,
                    // so that we are can guarantee that when `destroy` is called, we are not still
                    // using `server`.
                    server.server_handle.take()
                });
            })
            .expect("failure spawning main event loop for rust async dispatch");

        // Take the pointer of the server object to use as the identifier for the server to the
        // driver runtime. It uses this as an opaque identifier and expects no particular layout of
        // the object pointed to, and we use it to free the box at unload in `Self::destroy`.
        server_ptr.cast()
    }

    /// Called by the driver host after shutting down a driver and once the handle passed to
    /// [`Self::initialize`] is dropped.
    ///
    /// # Safety
    ///
    /// This must only be called after the handle provided to [`Self::initialize`] has been
    /// dropped, which indicates that the main event loop of the driver lifecycle has ended.
    pub unsafe extern "C" fn destroy(obj: *mut c_void) {
        let obj: *mut Self = obj.cast();
        // SAFETY: We built this object in `initialize` and gave ownership of its
        // lifetime to the driver framework, which is now giving it to us to free.
        unsafe { drop(Box::from_raw(obj)) }
    }

    /// Implements the main message loop for handling start and stop messages from rust
    /// driver host and passing them on to the implementation of [`Driver`] we contain.
    async fn message_loop(&mut self, dispatcher: DispatcherRef<'_>) {
        loop {
            let server_handle_lock = self.server_handle.get();
            let Some(server_handle) = server_handle_lock else {
                panic!("driver already shut down while message loop was running")
            };
            match server_handle.read_bytes(dispatcher.clone()).await {
                Ok(Some(message)) => {
                    if let ControlFlow::Break(_) = self.handle_message(message).await {
                        // driver shut down or failed to start, exit message loop
                        return;
                    }
                }
                Ok(None) => panic!("unexpected empty message on server channel"),
                Err(status @ Status::PEER_CLOSED) | Err(status @ Status::UNAVAILABLE) => {
                    warn!("Driver server channel closed before a stop message with status {status}, exiting main loop early but stop() will not be called.");
                    return;
                }
                Err(e) => panic!("unexpected error on server channel {e}"),
            }
        }
    }

    /// Handles the start message by initializing logging and calling the [`Driver::start`] with
    /// a constructed [`DriverContext`].
    ///
    /// # Panics
    ///
    /// This method panics if the start message has already been received.
    async fn handle_start(&self, start_args: DriverStartArgs) -> Result<(), Status> {
        let context = DriverContext::new(self.root_dispatcher.clone(), start_args)?;
        context.start_logging(T::NAME)?;

        log::debug!("driver starting");

        let driver = T::start(context).await?;
        self.driver.set(driver).map_err(|_| ()).expect("Driver received start message twice");
        Ok(())
    }

    async fn handle_stop(&mut self) {
        log::debug!("driver stopping");
        self.driver
            .take()
            .expect("received stop message more than once or without successfully starting")
            .stop()
            .await;
    }

    /// Dispatches messages from the driver host to the appropriate implementation.
    ///
    /// # Panics
    ///
    /// This method panics if the messages are received out of order somehow (two start messages,
    /// stop before start, etc).
    async fn handle_message(&mut self, message: Message<[u8]>) -> ControlFlow<()> {
        let (_, request) = DriverRequest::read_from_message(message).unwrap();
        match request {
            DriverRequest::Start { start_args, responder } => {
                let res = self.handle_start(start_args).await.map_err(Status::into_raw);
                let Some(server_handle) = self.server_handle.get() else {
                    panic!("driver shutting down before it was finished starting")
                };
                responder.send_response(server_handle, res).unwrap();
                if res.is_ok() {
                    ControlFlow::Continue(())
                } else {
                    debug!("driver failed to start, exiting main loop");
                    ControlFlow::Break(())
                }
            }
            DriverRequest::Stop {} => {
                self.handle_stop().await;
                ControlFlow::Break(())
            }
            _ => panic!("Unknown message on server channel"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    use fdf::test::with_raw_dispatcher;
    use fdf::Arena;
    use zx::Status;

    #[derive(Default)]
    struct TestDriver {
        _not_empty: bool,
    }

    impl Driver for TestDriver {
        const NAME: &str = "test_driver";

        async fn start(context: DriverContext) -> Result<Self, Status> {
            let DriverContext { root_dispatcher, start_args, .. } = context;
            println!("created new test driver on dispatcher: {root_dispatcher:?}");
            println!("driver start message: {start_args:?}");
            Ok(Self::default())
        }
        async fn stop(&self) {
            println!("driver stop message");
        }
    }

    crate::driver_register!(TestDriver);

    #[test]
    fn register_driver() {
        assert_eq!(__fuchsia_driver_registration__.version, 1);
        let initialize_func = __fuchsia_driver_registration__.v1.initialize.expect("initializer");
        let destroy_func = __fuchsia_driver_registration__.v1.destroy.expect("destroy function");

        let (fin_tx, fin_rx) = mpsc::channel();
        let (server_chan, client_chan) = fdf::Channel::<[u8]>::create();
        with_raw_dispatcher("driver registration", move |dispatcher| {
            dispatcher
                .spawn_task(async move {
                    let channel_handle = server_chan.into_driver_handle().into_raw().get();
                    let driver_server = unsafe { initialize_func(channel_handle) } as usize;
                    assert_ne!(driver_server, 0);

                    let start_msg = DriverRequest::start_as_message(
                        Arena::new(),
                        DriverStartArgs::default(),
                        1,
                    )
                    .unwrap();
                    client_chan.write(start_msg).unwrap();
                    let _ = client_chan.read_bytes(dispatcher.as_ref()).await.unwrap();

                    let stop_msg = DriverRequest::stop_as_message(Arena::new()).unwrap();
                    client_chan.write(stop_msg).unwrap();
                    let Err(Status::PEER_CLOSED) =
                        client_chan.read_bytes(dispatcher.as_ref()).await
                    else {
                        panic!("expected peer closed from driver server after end message");
                    };

                    unsafe {
                        destroy_func(driver_server as *mut c_void);
                    }

                    fin_tx.send(()).unwrap();
                })
                .unwrap();
            fin_rx.recv().unwrap();
        });
    }
}
