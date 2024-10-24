// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_io as fio;
use futures::future::BoxFuture;
use router_error::{Explain, RouterError};
use sandbox::{
    Capability, CapabilityBound, Dict, DirEntry, RemotableCapability, SpecificRouter,
    SpecificRouterResponse,
};
use std::sync::Arc;
use vfs::directory::entry::{self, DirectoryEntry, DirectoryEntryAsync, EntryInfo, GetEntryInfo};
use vfs::execution_scope::ExecutionScope;

/// A trait to add functions to SpecificRouter that know about the component manager
/// types.
pub trait RouterExt<T: CapabilityBound>: Send + Sync {
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
        entry_type: fio::DirentType,
        scope: ExecutionScope,
        errors_fn: F,
    ) -> Arc<dyn DirectoryEntry>
    where
        for<'a> F: Fn(&'a RouterError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static;
}

/// Returns a [Dict] equivalent to `dict`, but with all routers replaced with [DirEntry].
///
/// This is an alternative to [Dict::try_into_directory_entry] when the [Dict] contains routers,
/// because at one time routers were not part the sandbox library.
// TODO:(https://fxrev.dev/374983288): Merge this with [Dict::try_into_directory_entry].
pub fn dict_routers_to_dir_entry(scope: &ExecutionScope, dict: &Dict) -> Dict {
    let out = Dict::new();
    for (key, value) in dict.enumerate() {
        let Ok(value) = value else {
            // This capability is not cloneable. Skip it.
            continue;
        };
        let value = match value {
            Capability::Dictionary(dict) => {
                Capability::Dictionary(dict_routers_to_dir_entry(scope, &dict))
            }
            Capability::ConnectorRouter(router) => Capability::DirEntry(DirEntry::new(
                router.into_directory_entry(fio::DirentType::Service, scope.clone(), |_| None),
            )),
            Capability::DictionaryRouter(router) => {
                Capability::DirEntry(DirEntry::new(router.into_directory_entry(
                    // TODO: Should we convert the DirEntry to a Directory here?
                    fio::DirentType::Service,
                    scope.clone(),
                    |_| None,
                )))
            }
            Capability::DirEntryRouter(router) => {
                Capability::DirEntry(DirEntry::new(router.into_directory_entry(
                    // TODO: This assumes the DirEntry type is Service. Unfortunately, with the
                    // current API there is no good way to get the DirEntry type in advance.
                    // This problem should go away once we revamp or remove DirEntry.
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

impl<T: CapabilityBound + Clone> RouterExt<T> for SpecificRouter<T>
where
    Capability: From<T>,
{
    fn into_directory_entry<F>(
        self,
        entry_type: fio::DirentType,
        scope: ExecutionScope,
        errors_fn: F,
    ) -> Arc<dyn DirectoryEntry>
    where
        for<'a> F: Fn(&'a RouterError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static,
    {
        struct RouterEntry<T: CapabilityBound, F: 'static + Send + Sync> {
            router: SpecificRouter<T>,
            entry_type: fio::DirentType,
            scope: ExecutionScope,
            errors_fn: F,
        }

        impl<T: CapabilityBound + Clone, F: 'static + Send + Sync> DirectoryEntry for RouterEntry<T, F>
        where
            Capability: From<T>,
            for<'a> F: Fn(&'a RouterError) -> Option<BoxFuture<'a, ()>> + Send + Sync + 'static,
        {
            fn open_entry(
                self: Arc<Self>,
                mut request: entry::OpenRequest<'_>,
            ) -> Result<(), zx::Status> {
                request.set_scope(self.scope.clone());
                request.spawn(self);
                Ok(())
            }
        }

        impl<T: CapabilityBound, F: 'static + Send + Sync> GetEntryInfo for RouterEntry<T, F> {
            fn entry_info(&self) -> EntryInfo {
                EntryInfo::new(fio::INO_UNKNOWN, self.entry_type)
            }
        }

        impl<T: CapabilityBound + Clone, F: 'static + Send + Sync> DirectoryEntryAsync for RouterEntry<T, F>
        where
            Capability: From<T>,
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
                let result = match self.router.route(None, false).await {
                    Ok(SpecificRouterResponse::<T>::Capability(c)) => Ok(Capability::from(c)),
                    Ok(SpecificRouterResponse::<T>::Unavailable) => {
                        return Err(zx::Status::NOT_FOUND);
                    }
                    Ok(SpecificRouterResponse::<T>::Debug(_)) => {
                        // This shouldn't happen.
                        return Err(zx::Status::INTERNAL);
                    }
                    Err(e) => Err(e),
                };
                let error = match result {
                    Ok(capability) => {
                        let capability = match capability {
                            // HACK: Dict needs special casing because [Dict::try_into_open]
                            // is unaware of routers.
                            // TODO:(https://fxrev.dev/374983288): Merge this with
                            // [Dict::try_into_directory_entry].
                            Capability::Dictionary(d) => {
                                dict_routers_to_dir_entry(&self.scope, &d).into()
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

        Arc::new(RouterEntry { router: self.clone(), entry_type, scope, errors_fn })
    }
}
