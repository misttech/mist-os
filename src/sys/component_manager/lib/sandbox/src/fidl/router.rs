// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fidl::RemotableCapability;
use crate::{Capability, CapabilityBound, Dict, DirEntry, Request, Router, RouterResponse};
use fidl::AsHandleRef;
use router_error::{Explain, RouterError};
use std::sync::Arc;
use vfs::directory::entry::{self, DirectoryEntry, DirectoryEntryAsync, EntryInfo, GetEntryInfo};
use vfs::execution_scope::ExecutionScope;
use {fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, zx};

impl From<Request> for fsandbox::RouteRequest {
    fn from(request: Request) -> Self {
        let (token, server) = zx::EventPair::create();
        request.target.register(token.get_koid().unwrap(), server);
        fsandbox::RouteRequest {
            requesting: Some(fsandbox::InstanceToken { token }),
            metadata: Some(request.metadata.into()),
            ..Default::default()
        }
    }
}

impl TryFrom<fsandbox::DictionaryRouterRouteResponse> for RouterResponse<Dict> {
    type Error = crate::RemoteError;

    fn try_from(resp: fsandbox::DictionaryRouterRouteResponse) -> Result<Self, Self::Error> {
        Ok(match resp {
            fsandbox::DictionaryRouterRouteResponse::Dictionary(dict) => {
                RouterResponse::<Dict>::Capability(dict.try_into()?)
            }
            fsandbox::DictionaryRouterRouteResponse::Unavailable(_) => RouterResponse::Unavailable,
        })
    }
}

/// Binds a Route request from fidl to the Rust [Router::Route] API. Shared by
/// [Router] server implementations.
pub(crate) async fn route_from_fidl<T, R>(
    router: &Router<T>,
    payload: fsandbox::RouteRequest,
) -> Result<R, fsandbox::RouterError>
where
    T: CapabilityBound,
    R: TryFrom<RouterResponse<T>, Error = fsandbox::RouterError>,
{
    let resp = match (payload.requesting, payload.metadata) {
        (Some(token), Some(metadata)) => {
            let capability =
                crate::fidl::registry::get(token.token.as_handle_ref().get_koid().unwrap());
            let component = match capability {
                Some(crate::Capability::Instance(c)) => c,
                Some(_) => return Err(fsandbox::RouterError::InvalidArgs),
                None => return Err(fsandbox::RouterError::InvalidArgs),
            };
            let Capability::Dictionary(metadata) =
                Capability::try_from(fsandbox::Capability::Dictionary(metadata)).unwrap()
            else {
                return Err(fsandbox::RouterError::InvalidArgs);
            };
            let request = Request { target: component, metadata };
            router.route(Some(request), false).await?
        }
        (None, None) => router.route(None, false).await?,
        _ => {
            return Err(fsandbox::RouterError::InvalidArgs);
        }
    };
    resp.try_into()
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
                router.into_directory_entry(fio::DirentType::Service, scope.clone()),
            )),
            Capability::DictionaryRouter(router) => {
                Capability::DirEntry(DirEntry::new(router.into_directory_entry(
                    // TODO: Should we convert the DirEntry to a Directory here?
                    fio::DirentType::Service,
                    scope.clone(),
                )))
            }
            Capability::DirEntryRouter(router) => {
                Capability::DirEntry(DirEntry::new(router.into_directory_entry(
                    // TODO(https://fxbug.dev/340891837): This assumes the DirEntry type is
                    // Service. Unfortunately, with the current API there is no good way to get the
                    // DirEntry type in advance. This problem should go away once we revamp or
                    // remove DirEntry.
                    fio::DirentType::Service,
                    scope.clone(),
                )))
            }
            other => other,
        };
        out.insert(key, value).ok();
    }
    out
}

impl<T: CapabilityBound + Clone> Router<T>
where
    Capability: From<T>,
{
    pub fn into_directory_entry(
        self,
        entry_type: fio::DirentType,
        scope: ExecutionScope,
    ) -> Arc<dyn DirectoryEntry> {
        struct RouterEntry<T: CapabilityBound> {
            router: Router<T>,
            entry_type: fio::DirentType,
            scope: ExecutionScope,
        }

        impl<T: CapabilityBound + Clone> DirectoryEntry for RouterEntry<T>
        where
            Capability: From<T>,
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

        impl<T: CapabilityBound> GetEntryInfo for RouterEntry<T> {
            fn entry_info(&self) -> EntryInfo {
                EntryInfo::new(fio::INO_UNKNOWN, self.entry_type)
            }
        }

        impl<T: CapabilityBound + Clone> DirectoryEntryAsync for RouterEntry<T>
        where
            Capability: From<T>,
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
                    Ok(RouterResponse::<T>::Capability(c)) => Ok(Capability::from(c)),
                    Ok(RouterResponse::<T>::Unavailable) => {
                        return Err(zx::Status::NOT_FOUND);
                    }
                    Ok(RouterResponse::<T>::Debug(_)) => {
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
                        match capability.try_into_directory_entry(self.scope.clone()) {
                            Ok(open) => return open.open_entry(open_request),
                            Err(_) => RouterError::NotSupported,
                        }
                    }
                    Err(error) => error, // Routing failed (e.g. broken route).
                };
                Err(error.as_zx_status())
            }
        }

        Arc::new(RouterEntry { router: self, entry_type, scope })
    }
}
