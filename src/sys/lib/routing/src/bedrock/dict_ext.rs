// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::request_metadata::METADATA_KEY_TYPE;
use crate::error::RoutingError;
use async_trait::async_trait;
use cm_rust::CapabilityTypeName;
use cm_types::{IterablePath, Name, RelativePath};
use fidl_fuchsia_component_sandbox as fsandbox;
use moniker::ExtendedMoniker;
use router_error::RouterError;
use sandbox::{
    Capability, CapabilityBound, Connector, Data, Dict, DirConnector, DirEntry, Request, Routable,
    Router, RouterResponse,
};
use std::fmt::Debug;

#[async_trait]
pub trait DictExt {
    /// Returns the capability at the path, if it exists. Returns `None` if path is empty.
    fn get_capability(&self, path: &impl IterablePath) -> Option<Capability>;

    /// Looks up a top-level router in this [Dict] with return type `T`. If it's not found (or it's
    /// not a router) returns a router that always returns `not_found_error`. If `path` has one
    /// segment and a router was found, returns that router.
    ///
    /// If `path` is a multi-segment path, the returned router performs a [Dict] lookup with the
    /// remaining path relative to the top-level router (see [LazyGet::lazy_get]).
    ///
    /// REQUIRES: `path` is not empty.
    fn get_router_or_not_found<T>(
        &self,
        path: &impl IterablePath,
        not_found_error: RoutingError,
    ) -> Router<T>
    where
        T: CapabilityBound,
        Router<T>: TryFrom<Capability>;

    /// Inserts the capability at the path. Intermediary dictionaries are created as needed.
    fn insert_capability(
        &self,
        path: &impl IterablePath,
        capability: Capability,
    ) -> Result<(), fsandbox::CapabilityStoreError>;

    /// Removes the capability at the path, if it exists, and returns it.
    fn remove_capability(&self, path: &impl IterablePath) -> Option<Capability>;

    /// Looks up the element at `path`. When encountering an intermediate router, use `request` to
    /// request the underlying capability from it. In contrast, `get_capability` will return
    /// `None`.
    ///
    /// Note that the return value can contain any capability type, instead of a parameterized `T`.
    /// This is because some callers work with a generic capability and don't care about the
    /// specific type. Callers who do care can use `TryFrom` to cast to the expected
    /// [RouterResponse] type.
    async fn get_with_request<'a>(
        &self,
        moniker: &ExtendedMoniker,
        path: &'a impl IterablePath,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Option<GenericRouterResponse>, RouterError>;
}

/// The analogue of a [RouterResponse] that can hold any type of capability. This is the
/// return type of [DictExt::get_with_request].
#[derive(Debug)]
pub enum GenericRouterResponse {
    /// Routing succeeded and returned this capability.
    Capability(Capability),

    /// Routing succeeded, but the capability was marked unavailable.
    Unavailable,

    /// Routing succeeded in debug mode, `Data` contains the debug data.
    Debug(Data),
}

impl<T: CapabilityBound> TryFrom<GenericRouterResponse> for RouterResponse<T> {
    // Returns the capability's debug typename.
    type Error = &'static str;

    fn try_from(r: GenericRouterResponse) -> Result<Self, Self::Error> {
        let r = match r {
            GenericRouterResponse::Capability(c) => {
                let debug_name = c.debug_typename();
                RouterResponse::<T>::Capability(c.try_into().map_err(|_| debug_name)?)
            }
            GenericRouterResponse::Unavailable => RouterResponse::<T>::Unavailable,
            GenericRouterResponse::Debug(d) => RouterResponse::<T>::Debug(d),
        };
        Ok(r)
    }
}

#[async_trait]
impl DictExt for Dict {
    fn get_capability(&self, path: &impl IterablePath) -> Option<Capability> {
        let mut segments = path.iter_segments();
        let Some(mut current_name) = segments.next() else { return Some(self.clone().into()) };
        let mut current_dict = self.clone();
        loop {
            match segments.next() {
                Some(next_name) => {
                    let sub_dict = current_dict
                        .get(current_name)
                        .ok()
                        .flatten()
                        .and_then(|value| value.to_dictionary())?;
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => return current_dict.get(current_name).ok().flatten(),
            }
        }
    }

    fn get_router_or_not_found<T>(
        &self,
        path: &impl IterablePath,
        not_found_error: RoutingError,
    ) -> Router<T>
    where
        T: CapabilityBound,
        Router<T>: TryFrom<Capability>,
    {
        let mut segments = path.iter_segments();
        let root = segments.next().expect("path must be nonempty");

        #[derive(Debug)]
        struct ErrorRouter {
            not_found_error: RouterError,
        }

        #[async_trait]
        impl<T: CapabilityBound> Routable<T> for ErrorRouter {
            async fn route(
                &self,
                _request: Option<Request>,
                _debug: bool,
            ) -> Result<RouterResponse<T>, RouterError> {
                Err(self.not_found_error.clone())
            }
        }

        /// This uses the same algorithm as [LazyGet], but that is implemented for
        /// [Router<Dict>] while this is implemented for [Router]. This duplication will go
        /// away once [Router] is replaced with [Router].
        #[derive(Debug)]
        struct ScopedDictRouter<P: IterablePath + Debug + 'static> {
            router: Router<Dict>,
            path: P,
            not_found_error: RoutingError,
        }

        #[async_trait]
        impl<P: IterablePath + Debug + 'static, T: CapabilityBound> Routable<T> for ScopedDictRouter<P> {
            async fn route(
                &self,
                request: Option<Request>,
                debug: bool,
            ) -> Result<RouterResponse<T>, RouterError> {
                // If `debug` is true, that should only apply to the capability at `path`.
                // Here we're looking up the containing dictionary, so set `debug = false`, to
                // obtain the actual Dict and not its debug info. For the same reason, we need
                // to set the capability type on the first request to Dictionary.
                let init_request = request_with_dictionary_replacement(request.as_ref())?;
                match self.router.route(init_request, false).await? {
                    RouterResponse::<Dict>::Capability(dict) => {
                        let moniker: ExtendedMoniker = self.not_found_error.clone().into();
                        let resp =
                            dict.get_with_request(&moniker, &self.path, request, debug).await?;
                        let resp =
                            resp.ok_or_else(|| RouterError::from(self.not_found_error.clone()))?;
                        let resp = resp.try_into().map_err(|debug_name: &'static str| {
                            RoutingError::BedrockWrongCapabilityType {
                                expected: T::debug_typename().into(),
                                actual: debug_name.into(),
                                moniker,
                            }
                        })?;
                        Ok(resp)
                    }
                    _ => Err(RoutingError::BedrockMemberAccessUnsupported {
                        moniker: self.not_found_error.clone().into(),
                    }
                    .into()),
                }
            }
        }

        if segments.next().is_none() {
            // No nested lookup necessary.
            let Some(router) =
                self.get(root).ok().flatten().and_then(|cap| Router::<T>::try_from(cap).ok())
            else {
                return Router::<T>::new(ErrorRouter { not_found_error: not_found_error.into() });
            };
            return router;
        }

        let Some(cap) = self.get(root).ok().flatten() else {
            return Router::<T>::new(ErrorRouter { not_found_error: not_found_error.into() });
        };
        let router = match cap {
            Capability::Dictionary(d) => Router::<Dict>::new_ok(d),
            Capability::DictionaryRouter(r) => r,
            _ => {
                return Router::<T>::new(ErrorRouter { not_found_error: not_found_error.into() });
            }
        };

        let mut segments = path.iter_segments();
        let _ = segments.next().unwrap();
        let path = RelativePath::from(segments.collect::<Vec<_>>());

        Router::<T>::new(ScopedDictRouter { router, path, not_found_error: not_found_error.into() })
    }

    fn insert_capability(
        &self,
        path: &impl IterablePath,
        capability: Capability,
    ) -> Result<(), fsandbox::CapabilityStoreError> {
        let mut segments = path.iter_segments();
        let mut current_name = segments.next().expect("path must be non-empty");
        let mut current_dict = self.clone();
        loop {
            match segments.next() {
                Some(next_name) => {
                    let sub_dict = {
                        match current_dict.get(current_name) {
                            Ok(Some(cap)) => cap
                                .to_dictionary()
                                .ok_or(fsandbox::CapabilityStoreError::ItemNotFound)?,
                            Ok(None) => {
                                let dict = Dict::new();
                                current_dict.insert(
                                    current_name.into(),
                                    Capability::Dictionary(dict.clone()),
                                )?;
                                dict
                            }
                            Err(_) => return Err(fsandbox::CapabilityStoreError::ItemNotFound),
                        }
                    };
                    current_dict = sub_dict;

                    current_name = next_name;
                }
                None => {
                    return current_dict.insert(current_name.into(), capability);
                }
            }
        }
    }

    fn remove_capability(&self, path: &impl IterablePath) -> Option<Capability> {
        let mut segments = path.iter_segments();
        let mut current_name = segments.next().expect("path must be non-empty");
        let mut current_dict = self.clone();
        loop {
            match segments.next() {
                Some(next_name) => {
                    let sub_dict = current_dict
                        .get(current_name)
                        .ok()
                        .flatten()
                        .and_then(|value| value.to_dictionary());
                    if sub_dict.is_none() {
                        // The capability doesn't exist, there's nothing to remove.
                        return None;
                    }
                    current_dict = sub_dict.unwrap();
                    current_name = next_name;
                }
                None => {
                    return current_dict.remove(current_name);
                }
            }
        }
    }

    async fn get_with_request<'a>(
        &self,
        moniker: &ExtendedMoniker,
        path: &'a impl IterablePath,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Option<GenericRouterResponse>, RouterError> {
        let mut current_dict = self.clone();
        let num_segments = path.iter_segments().count();
        for (next_idx, next_name) in path.iter_segments().enumerate() {
            // Get the capability.
            let capability = current_dict
                .get(next_name)
                .map_err(|_| RoutingError::BedrockNotCloneable { moniker: moniker.clone() })?;

            // The capability doesn't exist.
            let Some(capability) = capability else {
                return Ok(None);
            };

            // Resolve the capability, this is a noop if it's not a router.
            let debug = if next_idx < num_segments - 1 {
                // If `request.debug` is true, that should only apply to the capability at `path`.
                // Since we're not looking up the final path segment, set `debug = false`, to
                // obtain the actual Dict and not its debug info.
                false
            } else {
                debug
            };

            if next_idx < num_segments - 1 {
                // Not at the end of the path yet, so there's more nesting. We expect to
                // have found a [Dict], or a [Dict] router -- traverse into this [Dict].
                let request = request_with_dictionary_replacement(request.as_ref())?;
                match capability {
                    Capability::Dictionary(d) => {
                        current_dict = d;
                    }
                    Capability::DictionaryRouter(r) => match r.route(request, false).await? {
                        RouterResponse::<Dict>::Capability(d) => {
                            current_dict = d;
                        }
                        RouterResponse::<Dict>::Unavailable => {
                            return Ok(Some(GenericRouterResponse::Unavailable));
                        }
                        RouterResponse::<Dict>::Debug(d) => {
                            // This shouldn't happen (we passed debug=false). Just pass it up
                            // the chain so the caller can decide how to deal with it.
                            return Ok(Some(GenericRouterResponse::Debug(d)));
                        }
                    },
                    _ => {
                        return Err(RoutingError::BedrockWrongCapabilityType {
                            expected: Dict::debug_typename().into(),
                            actual: capability.debug_typename().into(),
                            moniker: moniker.clone(),
                        }
                        .into());
                    }
                }
            } else {
                // We've reached the end of our path. The last capability should have type
                // `T` or `Router<T>`.
                //
                // There's a bit of repetition here because this function supports multiple router
                // types.
                let request = request.as_ref().map(|r| r.try_clone()).transpose()?;
                let capability: Capability = match capability {
                    Capability::DictionaryRouter(r) => match r.route(request, debug).await? {
                        RouterResponse::<Dict>::Capability(c) => c.into(),
                        RouterResponse::<Dict>::Unavailable => {
                            return Ok(Some(GenericRouterResponse::Unavailable));
                        }
                        RouterResponse::<Dict>::Debug(d) => {
                            return Ok(Some(GenericRouterResponse::Debug(d)));
                        }
                    },
                    Capability::ConnectorRouter(r) => match r.route(request, debug).await? {
                        RouterResponse::<Connector>::Capability(c) => c.into(),
                        RouterResponse::<Connector>::Unavailable => {
                            return Ok(Some(GenericRouterResponse::Unavailable));
                        }
                        RouterResponse::<Connector>::Debug(d) => {
                            return Ok(Some(GenericRouterResponse::Debug(d)));
                        }
                    },
                    Capability::DataRouter(r) => match r.route(request, debug).await? {
                        RouterResponse::<Data>::Capability(c) => c.into(),
                        RouterResponse::<Data>::Unavailable => {
                            return Ok(Some(GenericRouterResponse::Unavailable));
                        }
                        RouterResponse::<Data>::Debug(d) => {
                            return Ok(Some(GenericRouterResponse::Debug(d)));
                        }
                    },
                    Capability::DirEntryRouter(r) => match r.route(request, debug).await? {
                        RouterResponse::<DirEntry>::Capability(c) => c.into(),
                        RouterResponse::<DirEntry>::Unavailable => {
                            return Ok(Some(GenericRouterResponse::Unavailable));
                        }
                        RouterResponse::<DirEntry>::Debug(d) => {
                            return Ok(Some(GenericRouterResponse::Debug(d)));
                        }
                    },
                    Capability::DirConnectorRouter(r) => match r.route(request, debug).await? {
                        RouterResponse::<DirConnector>::Capability(c) => c.into(),
                        RouterResponse::<DirConnector>::Unavailable => {
                            return Ok(Some(GenericRouterResponse::Unavailable));
                        }
                        RouterResponse::<DirConnector>::Debug(d) => {
                            return Ok(Some(GenericRouterResponse::Debug(d)));
                        }
                    },
                    other => other,
                };
                return Ok(Some(GenericRouterResponse::Capability(capability)));
            }
        }
        unreachable!("get_with_request: All cases are handled in the loop");
    }
}

/// Creates a clone of `request` that is identical except `"type"` is set to `dictionary`
/// if it is not already. If `request` is `None`, `None` will be returned.
///
/// This is convenient for router lookups of nested paths, since all lookups except the last
/// segment are dictionary lookups.
pub(super) fn request_with_dictionary_replacement(
    request: Option<&Request>,
) -> Result<Option<Request>, RoutingError> {
    Ok(request.as_ref().map(|r| r.try_clone()).transpose()?.map(|r| {
        let _ = r.metadata.insert(
            Name::new(METADATA_KEY_TYPE).unwrap(),
            Capability::Data(Data::String(CapabilityTypeName::Dictionary.to_string())),
        );
        r
    }))
}
