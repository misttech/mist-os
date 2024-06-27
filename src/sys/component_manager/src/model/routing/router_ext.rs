// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::{ExtendedInstance, WeakComponentInstance, WeakExtendedInstance};
use ::routing::error::ComponentInstanceError;
use futures::future::BoxFuture;
use router_error::{Explain, RouterError};
use sandbox::{
    Capability, Dict, DirEntry, RemotableCapability, Request, Router, WeakInstanceToken,
};
use std::sync::Arc;
use vfs::directory::entry::{self, DirectoryEntry, DirectoryEntryAsync, EntryInfo};
use vfs::execution_scope::ExecutionScope;
use {fidl_fuchsia_io as fio, fuchsia_zircon as zx};

/// A trait to add functions to Router that know about the component manager
/// types.
pub trait RouterExt: Send + Sync {
    /// Returns a [Dict] equivalent to `dict`, but with all [Router]s replaced with [Open].
    ///
    /// This is an alternative to [Dict::try_into_open] when the [Dict] contains [Router]s, since
    /// [Router] is not currently a type defined by the sandbox library.
    fn dict_routers_to_open(
        weak_component: &WeakInstanceToken,
        scope: &ExecutionScope,
        dict: &Dict,
    ) -> Dict;

    /// Converts the [Router] capability into DirectoryEntry such that open requests
    /// will be fulfilled via the specified `request` on the router.
    ///
    /// `entry_type` is the type of the entry when the DirectoryEntry is accessed through a `fuchsia.io`
    /// connection.
    ///
    /// Routing and open tasks are spawned on `scope`.
    ///
    /// When routing failed while exercising the returned DirectoryEntry, errors will be
    /// sent to `errors_fn`.
    fn into_directory_entry<F>(
        self,
        request: Request,
        entry_type: fio::DirentType,
        scope: ExecutionScope,
        errors_fn: F,
    ) -> Arc<dyn DirectoryEntry>
    where
        for<'a> F: Fn(&'a RouterError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static;
}

impl RouterExt for Router {
    fn dict_routers_to_open(
        weak_component: &WeakInstanceToken,
        scope: &ExecutionScope,
        dict: &Dict,
    ) -> Dict {
        let out = Dict::new();
        for (key, value) in dict.enumerate() {
            let Ok(value) = value else {
                // This capability is not cloneable. Skip it.
                continue;
            };
            let value = match value {
                Capability::Dictionary(dict) => {
                    Capability::Dictionary(Self::dict_routers_to_open(weak_component, scope, &dict))
                }
                Capability::Router(router) => {
                    let request = Request {
                        target: weak_component.clone(),
                        // Use the weakest availability, so that it gets immediately upgraded to
                        // the availability in `router`.
                        availability: cm_types::Availability::Transitional,
                        debug: false,
                    };
                    // TODO: Should we convert the Open to a Directory here if the Router wraps a
                    // Dict?
                    Capability::DirEntry(DirEntry::new(router.into_directory_entry(
                        request,
                        fio::DirentType::Service,
                        scope.clone(),
                        |_| None,
                    )))
                }
                other => other,
            };
            out.insert(key, value).ok();
        }
        out
    }

    fn into_directory_entry<F>(
        self,
        request: Request,
        entry_type: fio::DirentType,
        scope: ExecutionScope,
        errors_fn: F,
    ) -> Arc<dyn DirectoryEntry>
    where
        for<'a> F: Fn(&'a RouterError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static,
    {
        struct RouterEntry<F> {
            router: Router,
            request: Request,
            entry_type: fio::DirentType,
            scope: ExecutionScope,
            errors_fn: F,
        }

        impl<F> DirectoryEntry for RouterEntry<F>
        where
            for<'a> F: Fn(&'a RouterError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static,
        {
            fn entry_info(&self) -> EntryInfo {
                EntryInfo::new(fio::INO_UNKNOWN, self.entry_type)
            }

            fn open_entry(
                self: Arc<Self>,
                mut request: entry::OpenRequest<'_>,
            ) -> Result<(), zx::Status> {
                request.set_scope(self.scope.clone());
                request.spawn(self);
                Ok(())
            }
        }

        impl<F> DirectoryEntryAsync for RouterEntry<F>
        where
            for<'a> F: Fn(&'a RouterError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static,
        {
            async fn open_entry_async(
                self: Arc<Self>,
                open_request: entry::OpenRequest<'_>,
            ) -> Result<(), zx::Status> {
                // Hold a guard to prevent this task from being dropped during component
                // destruction.  This task is tied to the target component.
                let _guard = open_request.scope().active_guard();

                // Request a capability from the `router`.
                let result = self.router.route(self.request.clone()).await;
                let error = match result {
                    Ok(capability) => {
                        let capability = match capability {
                            // HACK: Dict needs special casing because [Dict::try_into_open]
                            // is unaware of [Router].
                            Capability::Dictionary(d) => {
                                Router::dict_routers_to_open(&self.request.target, &self.scope, &d)
                                    .into()
                            }
                            Capability::Unit(_) => {
                                return Err(zx::Status::NOT_FOUND);
                            }
                            cap => cap,
                        };
                        match capability.try_into_directory_entry() {
                            Ok(open) => return open.open_entry(open_request),
                            Err(e) => errors::OpenError::DoesNotSupportOpen(e).into(),
                        }
                    }
                    Err(error) => error, // Routing failed (e.g. broken route).
                };
                if let Some(fut) = (self.errors_fn)(&error) {
                    fut.await;
                }
                Err(error.as_zx_status())
            }
        }

        Arc::new(RouterEntry { router: self.clone(), request, entry_type, scope, errors_fn })
    }
}

/// A trait to add functions WeakComponentInstancethat know about the component
/// manager types.
pub trait WeakInstanceTokenExt {
    /// Create a new token for a component instance or component_manager.
    fn new(instance: WeakExtendedInstance) -> WeakInstanceToken;

    /// Create a new token for a component instance.
    fn new_component(instance: WeakComponentInstance) -> WeakInstanceToken {
        WeakInstanceToken::new(WeakExtendedInstance::Component(instance))
    }

    /// Upgrade this token to the underlying instance.
    fn to_instance(self) -> WeakExtendedInstance;

    /// Get a reference to the underlying instance.
    fn as_ref(&self) -> &WeakExtendedInstance;

    /// Get a strong reference to the underlying instance.
    fn upgrade(&self) -> Result<ExtendedInstance, ComponentInstanceError>;

    /// Get the moniker for this component.
    fn moniker(&self) -> moniker::ExtendedMoniker;

    #[cfg(test)]
    fn invalid() -> WeakInstanceToken {
        WeakInstanceToken::new_component(WeakComponentInstance::invalid())
    }
}

impl WeakInstanceTokenExt for WeakInstanceToken {
    fn new(instance: WeakExtendedInstance) -> Self {
        Self { inner: Arc::new(instance) }
    }

    fn to_instance(self) -> WeakExtendedInstance {
        self.as_ref().clone()
    }

    fn as_ref(&self) -> &WeakExtendedInstance {
        match self.inner.as_any().downcast_ref::<WeakExtendedInstance>() {
            Some(instance) => &instance,
            None => panic!(),
        }
    }

    fn upgrade(&self) -> Result<ExtendedInstance, ComponentInstanceError> {
        self.as_ref().upgrade()
    }

    fn moniker(&self) -> moniker::ExtendedMoniker {
        self.as_ref().extended_moniker()
    }
}
