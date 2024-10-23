// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::RoutingError;
use async_trait::async_trait;
use cm_types::{IterablePath, RelativePath};
use fidl_fuchsia_component_sandbox as fsandbox;
use moniker::ExtendedMoniker;
use router_error::RouterError;
use sandbox::{Capability, Dict, Request, Routable, Router};
use std::fmt::Debug;

#[async_trait]
pub trait DictExt {
    /// Returns the capability at the path, if it exists. Returns `None` if path is empty.
    fn get_capability(&self, path: &impl IterablePath) -> Option<Capability>;

    /// Looks up a top-level [Router] in this [Dict]. If it's not found (or it's not a [Router])
    /// returns a [Router] that always returns `not_found_error`. If `path` has one segment
    /// and a [Router] was found, returns that [Router].
    ///
    /// If `path` is a multi-segment path, the returned [Router] performs a [Dict] lookup with the
    /// remaining path relative to the top-level [Router] (see [LazyGet::lazy_get]).
    ///
    /// REQUIRES: `path` is not empty.
    fn get_router_or_not_found(
        &self,
        path: &impl IterablePath,
        not_found_error: RoutingError,
    ) -> Router;

    /// Inserts the capability at the path. Intermediary dictionaries are created as needed.
    fn insert_capability(
        &self,
        path: &impl IterablePath,
        capability: Capability,
    ) -> Result<(), fsandbox::CapabilityStoreError>;

    /// Removes the capability at the path, if it exists.
    fn remove_capability(&self, path: &impl IterablePath);

    /// Looks up the element at `path`. When encountering an intermediate router, use `request`
    /// to request the underlying capability from it. In contrast, `get_capability` will return
    /// `None`.
    async fn get_with_request<'a>(
        &self,
        moniker: impl Into<ExtendedMoniker> + Send,
        path: &'a impl IterablePath,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Option<Capability>, RouterError>;
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
                    // Lifetimes are weird here with the MutexGuard, so we do this in two steps
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

    fn get_router_or_not_found(
        &self,
        path: &impl IterablePath,
        not_found_error: RoutingError,
    ) -> Router {
        let mut segments = path.iter_segments();
        let root = segments.next().expect("path must be nonempty");

        #[derive(Debug)]
        struct ErrorRouter {
            not_found_error: RouterError,
        }

        #[async_trait]
        impl Routable for ErrorRouter {
            async fn route(
                &self,
                _request: Option<Request>,
                _debug: bool,
            ) -> Result<Capability, RouterError> {
                Err(self.not_found_error.clone())
            }
        }

        /// This uses the same algorithm as [LazyGet], but that is implemented for
        /// [SpecificRouter<Dict>] while this is implemented for [Router]. This duplication will go
        /// away once [Router] is replaced with [SpecificRouter].
        #[derive(Debug)]
        struct ScopedDictRouter<P: IterablePath + Debug + 'static> {
            router: Router,
            path: P,
            not_found_error: RoutingError,
        }

        #[async_trait]
        impl<P: IterablePath + Debug + 'static> Routable for ScopedDictRouter<P> {
            async fn route(
                &self,
                request: Option<Request>,
                debug: bool,
            ) -> Result<Capability, RouterError> {
                // If `debug` is true, that should only apply to the capability at `path`.
                // Here we're looking up the containing dictionary, so set `debug = false`, to
                // obtain the actual Dict and not its debug info.
                let init_request = request.as_ref().map(|r| r.try_clone()).transpose()?;
                match self.router.route(init_request, false).await? {
                    Capability::Dictionary(dict) => {
                        let request = request.as_ref().map(|r| r.try_clone()).transpose()?;
                        let maybe_capability = dict
                            .get_with_request(
                                self.not_found_error.clone(),
                                &self.path,
                                request,
                                debug,
                            )
                            .await?;
                        maybe_capability.ok_or_else(|| self.not_found_error.clone().into())
                    }
                    _ => Err(RoutingError::BedrockMemberAccessUnsupported {
                        moniker: self.not_found_error.clone().into(),
                    }
                    .into()),
                }
            }
        }

        let Ok(Some(Capability::Router(router))) = self.get(root) else {
            return Router::new(ErrorRouter { not_found_error: not_found_error.into() });
        };
        if segments.next().is_none() {
            // No nested lookup necessary.
            return router;
        }
        let mut segments = path.iter_segments();
        let _ = segments.next().unwrap();
        let path = RelativePath::from(segments.map(|s| s.clone()).collect::<Vec<_>>());

        Router::new(ScopedDictRouter { router, path, not_found_error: not_found_error.into() })
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
                                    current_name.clone(),
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
                    return current_dict.insert(current_name.clone(), capability);
                }
            }
        }
    }

    fn remove_capability(&self, path: &impl IterablePath) {
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
                        return;
                    }
                    current_dict = sub_dict.unwrap();
                    current_name = next_name;
                }
                None => {
                    current_dict.remove(current_name);
                    return;
                }
            }
        }
    }

    async fn get_with_request<'a>(
        &self,
        moniker: impl Into<ExtendedMoniker> + Send,
        path: &'a impl IterablePath,
        request: Option<Request>,
        debug: bool,
    ) -> Result<Option<Capability>, RouterError> {
        let mut current_capability: Capability = self.clone().into();
        let moniker = moniker.into();
        let num_segments = path.iter_segments().count();
        for (next_idx, next_name) in path.iter_segments().enumerate() {
            // We have another name but no subdictionary, so exit.
            let Capability::Dictionary(current_dict) = &current_capability else { return Ok(None) };

            // Get the capability.
            let capability = current_dict
                .get(next_name)
                .map_err(|_| RoutingError::BedrockNotCloneable { moniker: moniker.clone() })?;

            // The capability doesn't exist.
            let Some(capability) = capability else { return Ok(None) };

            // Resolve the capability, this is a noop if it's not a router.
            let debug = if next_idx < num_segments - 1 {
                // If `request.debug` is true, that should only apply to the capability at `path`.
                // Since we're not looking up the final path segment, set `debug = false`, to
                // obtain the actual Dict and not its debug info.
                false
            } else {
                debug
            };
            let request = request.as_ref().map(|r| r.try_clone()).transpose()?;
            current_capability = capability.route(request, debug).await?;
        }
        Ok(Some(current_capability))
    }
}
