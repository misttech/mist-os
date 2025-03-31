// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::loader::{Library, LoaderService};
use crate::modules::ModulesAndSymbols;
use crate::utils::*;
use fdf::OnDispatcher;
use fdf_component::Incoming;
use fidl::client::decode_transaction_body;
use fidl::encoding::{DefaultFuchsiaResourceDialect, EmptyStruct, ResultType};
use fidl::endpoints::{RequestStream, ServerEnd};
use fidl::AsHandleRef;
use fuchsia_sync::Mutex;
use futures::channel::oneshot;
use futures::{FutureExt, TryStreamExt};
use namespace::Namespace;
use std::ffi::c_char;
use std::ptr::NonNull;
use std::sync::{Arc, Weak};
use zx::{HandleBased, Status};
use {
    fidl_fuchsia_data as fdata, fidl_fuchsia_driver_framework as fidl_fdf,
    fidl_fuchsia_driver_host as fdh,
};

extern "C" {
    fn driver_host_find_symbol(
        passive_abi: u64,
        so_name: *const c_char,
        so_name_len: usize,
        symbol_name: *const c_char,
        symbol_name_len: usize,
    ) -> u64;
}

struct LoadedDriver(u64);

impl LoadedDriver {
    fn get_hooks(&self, module_name: &str) -> Result<Hooks, Status> {
        const SYMBOL: &str = "__fuchsia_driver_registration__";
        let registration_addr = unsafe {
            driver_host_find_symbol(
                self.0,
                module_name.as_ptr() as *const c_char,
                module_name.len(),
                SYMBOL.as_ptr() as *const c_char,
                SYMBOL.len(),
            )
        };
        if registration_addr == 0 {
            return Err(Status::NOT_FOUND);
        }
        Hooks::new(registration_addr as *mut _)
    }

    fn get_symbols(
        &self,
        program: &fdata::Dictionary,
    ) -> Result<Vec<fidl_fdf::NodeSymbol>, Status> {
        let default = Vec::new();
        let modules = get_program_objvec(program, "modules")?.unwrap_or(&default);

        let mut symbols = Vec::new();

        for module in modules {
            let mut module_name = get_program_string(module, "module_name")?;
            // Special case for compat. The syntax could allow more more generic references to other
            // fields, but we don't need that for now, so we hardcode support for one specific field.
            if module_name == "#program.compat" {
                module_name = get_program_string(program, "compat")?;
            }
            let so_name = basename(module_name);

            // Lookup symbols specific to this module.
            let module_symbols = get_program_strvec(module, "symbols")?.ok_or(Status::NOT_FOUND)?;
            for symbol in module_symbols {
                let address = unsafe {
                    driver_host_find_symbol(
                        self.0,
                        so_name.as_ptr() as *const c_char,
                        so_name.len(),
                        symbol.as_ptr() as *const c_char,
                        symbol.len(),
                    )
                };
                if address == 0 {
                    return Err(Status::INVALID_ARGS);
                }
                symbols.push(fidl_fdf::NodeSymbol {
                    name: Some(symbol.to_string()),
                    address: Some(address),
                    module_name: Some(module_name.to_string()),
                    ..Default::default()
                });
            }
        }

        Ok(symbols)
    }
}

#[derive(Debug)]
struct Hooks(NonNull<fdf_sys::DriverRegistration>);
/// SAFETY: These hooks are just static pointers to code and therefore have no thread local state.
unsafe impl Send for Hooks {}
/// SAFETY: These hooks are valid on every thread after they are loaded.
unsafe impl Sync for Hooks {}

impl Hooks {
    fn new(registration: *mut fdf_sys::DriverRegistration) -> Result<Hooks, Status> {
        if registration.is_null() {
            log::error!("__fuchsia_driver_registration__ symbol not available in driver");
        }
        let registration: NonNull<fdf_sys::DriverRegistration> =
            NonNull::new(registration.cast()).ok_or(Status::NOT_FOUND)?;
        // SAFETY: The symbol is valid as long as the shared library is not closed. So its
        // lifetime must track that of |library| from above. We also do a null check to ensure
        // it's a valid pointer.
        let hooks = unsafe { registration.as_ref() };
        let version = hooks.version;
        if version < 1 || version > fdf_sys::DRIVER_REGISTRATION_VERSION_MAX as u64 {
            log::error!("Failed to start driver, unknown driver registration version: {version}");
            return Err(Status::WRONG_TYPE);
        }
        if hooks.v1.initialize.is_none() || hooks.v1.destroy.is_none() {
            log::error!("Failed to start driver, missing methods");
            return Err(Status::WRONG_TYPE);
        }
        Ok(Hooks(registration))
    }

    fn new_from_library(library: &Library) -> Result<Hooks, Status> {
        // SAFETY: The symbol is valid as long as the shared library is not closed. So its
        // lifetime must track that of |library| from above. We also do a null check to ensure
        // it's a valid pointer.
        Hooks::new(unsafe {
            libc::dlsym(library.ptr.as_ptr(), c"__fuchsia_driver_registration__".as_ptr())
        } as *mut _)
    }

    fn initialize(&self, channel_handle: fdf::DriverHandle) -> Token {
        // SAFETY: We know there are 0 other references to this. This is ref-safe because we know
        // the underlying memory is valid for the lifetime of the DriverInner object.
        let hooks = unsafe { self.0.as_ref() };
        let initialize_func = hooks.v1.initialize.unwrap();
        // SAFETY: We know it's safe to call initialize from the initial dispatcher.
        Token(unsafe { initialize_func(channel_handle.into_raw().get()) }.addr())
    }

    /// # Threading
    ///
    /// Must be called in the same sequence as initialize was called.
    fn destroy(&self, token: Token) {
        // SAFETY: We know there are 0 other references to this. This is ref-safe because we know
        // the underlying memory is valid for the lifetime of the DriverInner object.
        let hooks = unsafe { self.0.as_ref() };
        let destroy_func = hooks.v1.destroy.unwrap();
        // SAFETY: We need to call destroy if we've called initialize. This will be
        // synchronized with initialize to occur when the driver is destroyed in the shutdown
        // observer for the dispatcher.
        unsafe { destroy_func(token.0 as *mut _) };
    }
}

#[derive(Debug)]
struct Token(usize);

#[derive(Debug)]
struct LegacyDynamicallyLinkedState {
    #[allow(unused)]
    library: Library,

    // Modules listed in the program's module section.
    #[allow(unused)]
    modules_and_symbols: ModulesAndSymbols,
}

#[derive(Debug)]
struct DriverInner {
    #[allow(unused)]
    legacy_state: Option<LegacyDynamicallyLinkedState>,

    // The hooks to initialize and destroy the driver. Backed by the registration symbol.
    hooks: Hooks,

    // The initial dispatcher of the driver.
    // This is where the initialize hook is called for the driver.
    dispatcher: Option<fdf::DispatcherRef<'static>>,

    // This is the handle we use to represent the driver to the driver runtime.
    runtime_handle: Option<fdf_env::Driver<Driver>>,

    // This is set through the initialize hook and passed into destroy.
    token: Option<Token>,

    // This signals to the driver_host that the driver has been shutdown.
    shutdown_signaler: Option<oneshot::Sender<Weak<Driver>>>,

    // This is the token representing the node of this driver in the driver manager.
    node_token: Option<fidl::Event>,
}

impl Drop for DriverInner {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            self.hooks.destroy(token);
        }
    }
}

#[derive(Debug)]
pub(crate) struct Driver {
    url: String,
    inner: Mutex<DriverInner>,
}
impl Driver {
    pub async fn load(
        env: &fdf_env::Environment,
        mut start_args: fidl_fdf::DriverStartArgs,
    ) -> Result<(Arc<Driver>, fidl_fdf::DriverStartArgs), Status> {
        // Parse out important
        let url = start_args.url.clone().ok_or(Status::INVALID_ARGS)?;
        let program = start_args.program.as_ref().ok_or(Status::INVALID_ARGS)?;
        let binary = get_program_string(program, "binary")?;
        let default_dispatcher_opts = DispatcherOpts::from(program);
        let scheduler_role = get_program_string(program, "default_dispatcher_scheduler_role")
            .unwrap_or("")
            .to_string();
        let allowed_scheduler_roles =
            get_program_strvec(program, "allowed_scheduler_roles")?.map(|roles| roles.clone());

        // Read binary from incoming namespace into vmo.
        let incoming = start_args.incoming.take().ok_or(Status::INVALID_ARGS)?;
        let incoming: Namespace = incoming.try_into().map_err(|_| Status::INVALID_ARGS)?;
        start_args.incoming = Some(incoming.clone().into());
        let incoming: Incoming = incoming.try_into().map_err(|_| Status::INVALID_ARGS)?;
        let vmo = get_file_vmo(&incoming, binary).await?;
        vmo.set_name(&zx::Name::new_lossy(basename(binary)))?;

        if binary != "driver/compat.so" {
            let symbols = driver_symbols::find_restricted_symbols(&vmo, &url)?;
            if symbols.len() > 0 {
                log::error!(
                    "Driver '{binary}' referenced {} restricted libc symbols: ",
                    symbols.len()
                );
                for symbol in symbols {
                    log::error!("{symbol}");
                }
                return Err(Status::NOT_SUPPORTED);
            }
        }

        let library = LoaderService::try_load(vmo).await?;
        let hooks = Hooks::new_from_library(&library)?;
        let modules_and_symbols = ModulesAndSymbols::load(program, &incoming).await?;
        modules_and_symbols.copy_to_start_args(&mut start_args);
        let legacy_state = Some(LegacyDynamicallyLinkedState { library, modules_and_symbols });
        let dispatcher_name = basename(&url);
        let node_token = start_args.node_token.as_ref().map(|t| {
            t.duplicate_handle(fidl::Rights::SAME_RIGHTS)
                .expect("Failed to duplicate node token handle.")
        });
        let driver = Arc::new(Driver {
            url: url.clone(),
            inner: Mutex::new(DriverInner {
                legacy_state,
                hooks,
                dispatcher: None,
                runtime_handle: None,
                token: None,
                shutdown_signaler: None,
                node_token,
            }),
        });
        let driver_runtime_handle = env.new_driver(Arc::into_raw(driver.clone()));

        if !scheduler_role.is_empty() {
            driver_runtime_handle.add_allowed_scheduler_role(&scheduler_role);
        }
        if let Some(roles) = allowed_scheduler_roles {
            for role in roles {
                driver_runtime_handle.add_allowed_scheduler_role(&role);
            }
        }

        // The dispatcher must be shutdown before the dispatcher is destroyed.
        // Usually we will wait for the callback from |fdf_env::DriverShutdown| before destroying
        // the driver object (and hence the dispatcher).
        // In the case where we fail to start the driver, the driver object would be destructed
        // immediately, so here we hold an extra reference to the driver object to ensure the
        // dispatcher will not be destructed until shutdown completes.
        //
        // We do not destroy the dispatcher in the shutdown callback, to prevent crashes that
        // would happen if the driver attempts to access the dispatcher in its Stop hook.
        //
        // Currently we only support synchronized dispatchers for the default dispatcher.
        let driver_clone = driver.clone();
        let dispatcher = fdf::DispatcherBuilder::new()
            .name(&format!("{dispatcher_name}-default-{driver:p}"))
            .scheduler_role(&scheduler_role)
            .shutdown_observer(move |_| {
                let _ = driver_clone;
            });
        let dispatcher = match default_dispatcher_opts {
            DispatcherOpts::AllowSyncCalls => dispatcher.allow_thread_blocking(),
            DispatcherOpts::Default => dispatcher,
        };
        let dispatcher = driver_runtime_handle.new_dispatcher(dispatcher)?;
        driver.inner.lock().dispatcher = Some(dispatcher);
        driver.inner.lock().runtime_handle = Some(driver_runtime_handle);

        Ok((driver, start_args))
    }

    pub async fn initialize(
        env: &fdf_env::Environment,
        mut start_args: fidl_fdf::DriverStartArgs,
        dynamic_linking_abi: u64,
    ) -> Result<(Arc<Driver>, fidl_fdf::DriverStartArgs), Status> {
        // Parse out important fields.
        let url = start_args.url.clone().ok_or(Status::INVALID_ARGS)?;
        let program = start_args.program.as_ref().ok_or(Status::INVALID_ARGS)?;
        let binary = get_program_string(program, "binary")?;
        let default_dispatcher_opts = DispatcherOpts::from(program);
        let scheduler_role =
            get_program_string(program, "default_dispatcher_scheduler_role").unwrap_or("");
        let allowed_scheduler_roles = get_program_strvec(program, "allowed_scheduler_roles")?;

        let loaded_driver = LoadedDriver(dynamic_linking_abi);
        let hooks = loaded_driver.get_hooks(basename(binary))?;
        let mut symbols = loaded_driver.get_symbols(program)?;
        start_args.symbols.get_or_insert_default().append(&mut symbols);
        let dispatcher_name = basename(&url);
        let node_token = start_args.node_token.as_ref().map(|t| {
            t.duplicate_handle(fidl::Rights::SAME_RIGHTS)
                .expect("Failed to duplicate node token handle.")
        });
        let driver = Arc::new(Driver {
            url: url.clone(),
            inner: Mutex::new(DriverInner {
                legacy_state: None,
                hooks,
                dispatcher: None,
                runtime_handle: None,
                token: None,
                shutdown_signaler: None,
                node_token,
            }),
        });
        let driver_runtime_handle = env.new_driver(Arc::into_raw(driver.clone()));

        if !scheduler_role.is_empty() {
            driver_runtime_handle.add_allowed_scheduler_role(&scheduler_role);
        }
        if let Some(roles) = allowed_scheduler_roles {
            for role in roles {
                driver_runtime_handle.add_allowed_scheduler_role(&role);
            }
        }

        // The dispatcher must be shutdown before the dispatcher is destroyed.
        // Usually we will wait for the callback from |fdf_env::DriverShutdown| before destroying
        // the driver object (and hence the dispatcher).
        // In the case where we fail to start the driver, the driver object would be destructed
        // immediately, so here we hold an extra reference to the driver object to ensure the
        // dispatcher will not be destructed until shutdown completes.
        //
        // We do not destroy the dispatcher in the shutdown callback, to prevent crashes that
        // would happen if the driver attempts to access the dispatcher in its Stop hook.
        //
        // Currently we only support synchronized dispatchers for the default dispatcher.
        let driver_clone = driver.clone();
        let dispatcher = fdf::DispatcherBuilder::new()
            .name(&format!("{dispatcher_name}-default-{driver:p}"))
            .scheduler_role(&scheduler_role)
            .shutdown_observer(move |_| {
                let _ = driver_clone;
            });
        let dispatcher = match default_dispatcher_opts {
            DispatcherOpts::AllowSyncCalls => dispatcher.allow_thread_blocking(),
            DispatcherOpts::Default => dispatcher,
        };
        let dispatcher = driver_runtime_handle.new_dispatcher(dispatcher)?;
        driver.inner.lock().dispatcher = Some(dispatcher);
        driver.inner.lock().runtime_handle = Some(driver_runtime_handle);

        Ok((driver, start_args))
    }

    /// Must be called from the driver host main thread.
    pub async fn start(
        self: &Arc<Self>,
        start_args: fidl_fdf::DriverStartArgs,
        driver_request: ServerEnd<fdh::DriverMarker>,
        shutdown_signaler: oneshot::Sender<Weak<Driver>>,
        scope: &fuchsia_async::Scope,
    ) -> Result<(), Status> {
        self.inner.lock().shutdown_signaler = Some(shutdown_signaler);

        let (completer, task_result) = oneshot::channel();
        let (server_chan, client_chan) = fdf::Channel::<[u8]>::create();
        {
            let self_clone = self.clone();
            let inner = self.inner.lock();
            let dispatcher = inner.dispatcher.as_ref().expect("dispatcher should always be valid");
            let dispatcher_ref = dispatcher.clone();
            dispatcher.spawn_task(async move {
                {
                    let mut inner = self_clone.inner.lock();
                    let hooks = &inner.hooks;

                    inner.token = Some(hooks.initialize(server_chan.into_driver_handle()));
                };

                let start_msg =
                    fidl_fdf::DriverRequest::start_as_message(fdf::Arena::new(), start_args, 1)
                        .expect("Failed to create start message");
                if let Err(e) = client_chan.write(start_msg) {
                    completer.send(Err(e)).unwrap();
                    return;
                }

                // It's possible that this may never return if the driver blocks its main thread
                // after replying? In that case we would probably want another dispatcher.
                match client_chan.read_bytes(dispatcher_ref).await {
                    Err(e) => {
                        completer.send(Err(e)).unwrap();
                    }
                    Ok(None) => {
                        // Invalid response.
                        completer.send(Err(Status::INTERNAL)).unwrap();
                    }
                    Ok(Some(msg)) => {
                        let buf = msg.data().unwrap().to_vec();
                        let buf = fidl::MessageBufEtc::new_with(buf, Vec::new());
                        match decode_transaction_body::<
                            ResultType<EmptyStruct, i32>,
                            DefaultFuchsiaResourceDialect,
                            0x27be00ae42aa60c2,
                        >(buf)
                        {
                            Ok(status) => {
                                let ret = status.map_err(Status::from_raw).map(|_| client_chan);
                                completer.send(ret).unwrap();
                            }
                            Err(_) => completer.send(Err(Status::INVALID_ARGS)).unwrap(),
                        };
                    }
                };
            })?;
        }
        let client_chan = match task_result.await.unwrap() {
            Err(e) => {
                // We need to shutdown the dispatcher if start failed.
                self.shutdown(driver_request);
                return Err(e);
            }
            Ok(client_chan) => client_chan,
        };

        let (driver_stop_requested_signaler, driver_stop_requested) = oneshot::channel();
        let (driver_request_sender, driver_request_receiver) = oneshot::channel();
        let (exit_sender, exit_receiver) = oneshot::channel();
        scope.spawn_local(async move {
            // This needs to run in the context of a thread running fuchsia_async, which means the
            // driver host's initial thread.
            let mut stream = driver_request.into_stream();

            futures::select! {
                // The only request is stop and if we receive it, we assume it's a stop request.
                _ = stream.try_next().fuse() => {
                    driver_stop_requested_signaler.send(()).unwrap();
                }
                _ = exit_receiver.fuse() => (),
            };
            driver_request_sender
                .send(ServerEnd::new(
                    Arc::into_inner(stream.into_inner().0)
                        .expect("outstanding references to channel, possibly unhandeled messages?")
                        .into_channel()
                        .into(),
                ))
                .unwrap();
        });

        let inner = self.inner.lock();
        let dispatcher = inner.dispatcher.as_ref().expect("dispatcher should always be valid");
        let dispatcher_ref = dispatcher.clone();
        let dispatcher_ref2 = dispatcher.clone();
        let weak_self = Arc::downgrade(&self);
        dispatcher.spawn_task(async move {
            // Wait for driver manager to issue stop or the driver to have dropped its end of the
            // driver channel.
            futures::select! {
                _ = driver_stop_requested.fuse() => {
                    let stop_msg = fidl_fdf::DriverRequest::stop_as_message(fdf::Arena::new())
                        .expect("Failed to create stop message");
                    match client_chan.write(stop_msg) {
                        Ok(()) => {
                            // Wait for client_chan to receive peer_closed.
                            match client_chan.read_bytes(dispatcher_ref).await {
                                Err(e) => {
                                    assert_eq!(e, Status::PEER_CLOSED, "Unexpected status {e}");
                                }
                                Ok(None) => {
                                    log::error!("Expected PEER_CLOSED, received empty payload. Shuttingdown driver.");
                                }
                                Ok(Some(_msg)) => {
                                    log::error!("Expected PEER_CLOSED, received payload");
                                }
                            };
                        }
                        Err(e) => {
                            // It's possible the driver already decided to close its channel
                            // before recieving stop.
                            assert_eq!(e, Status::PEER_CLOSED, "Unexpected status {e}");
                            log::error!("Driver has already closed its channel end before recieving stop");
                        }
                    };
                },
                _ = client_chan.read_bytes(dispatcher_ref2).fuse() => {
                    let _ = exit_sender.send(());
                },
            };

            if let Some(strong_self) = weak_self.upgrade() {
                strong_self.shutdown(driver_request_receiver.await.unwrap());
            } else {
                log::error!("Failed to upgrade weak pointer to driver");
            }
        })?;

        Ok(())
    }

    pub fn get_url(&self) -> &str {
        return self.url.as_str();
    }

    pub fn duplicate_node_token(&self) -> Option<fidl::Event> {
        self.inner.lock().node_token.as_ref().map(|token| {
            token
                .duplicate_handle(fidl::Rights::SAME_RIGHTS)
                .expect("Failed to duplicate node token handle.")
        })
    }

    /// Shutdown the driver. The process is asynchronous. All references to the driver should be
    /// dropped prior to invoking this method.
    /// This method will do nothing if invoked multiple times or if runtime_handle was never
    /// initialized.
    fn shutdown(&self, driver_request: ServerEnd<fdh::DriverMarker>) {
        let runtime_handle = self.inner.lock().runtime_handle.take();
        let shutdown_signaler = self.inner.lock().shutdown_signaler.take();
        if let Some(runtime_handle) = runtime_handle {
            runtime_handle.shutdown(move |runtime_handle| {
                // SAFETY: This is safe because we previously leaked the arc when creating the
                // driver. Recovering through the shutdown callback is the expected flow.
                let this = unsafe { Arc::from_raw(runtime_handle.0) };
                if let Some(shutdown_signaler) = shutdown_signaler {
                    // This can fail if start fails.
                    let _ = shutdown_signaler.send(Arc::downgrade(&this));
                }
                let _ = driver_request.close_with_epitaph(zx::Status::OK);
                // This should be the last strong reference to the driver. The destroy hook will run
                // in its destructor.
                Arc::try_unwrap(this)
                    .expect("Someone unexpected is holding onto a reference of driver");
            });
        };
    }
}

impl PartialEq<fdf_env::UnownedDriver> for Driver {
    fn eq(&self, other: &fdf_env::UnownedDriver) -> bool {
        self.inner.lock().runtime_handle.as_ref().map(|h| h == other).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    #[fuchsia::test]
    async fn smoke_test() {
        assert!(true);
    }
}
