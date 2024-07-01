// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::RoutingError;
use async_trait::async_trait;
use cm_types::IterablePath;
use fidl_fuchsia_component_sandbox as fsandbox;
use router_error::RouterError;
use sandbox::{Capability, Dict, Request, Routable};

#[async_trait]
pub trait DictExt {
    /// Returns the capability at the path, if it exists. Returns `None` if path is empty.
    fn get_capability(&self, path: &impl IterablePath) -> Option<Capability>;

    /// Inserts the capability at the path. Intermediary dictionaries are created as needed.
    fn insert_capability(
        &self,
        path: &impl IterablePath,
        capability: Capability,
    ) -> Result<(), fsandbox::DictionaryError>;

    /// Removes the capability at the path, if it exists.
    fn remove_capability(&self, path: &impl IterablePath);

    /// Looks up the element at `path`. When encountering an intermediate router, use `request`
    /// to request the underlying capability from it. In contrast, `get_capability` will return
    /// `None`.
    async fn get_with_request<'a>(
        &self,
        path: &'a impl IterablePath,
        request: Request,
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

    fn insert_capability(
        &self,
        path: &impl IterablePath,
        capability: Capability,
    ) -> Result<(), fsandbox::DictionaryError> {
        let mut segments = path.iter_segments();
        let mut current_name = segments.next().expect("path must be non-empty");
        let mut current_dict = self.clone();
        loop {
            match segments.next() {
                Some(next_name) => {
                    let sub_dict = {
                        match current_dict.get(current_name) {
                            Ok(Some(cap)) => {
                                cap.to_dictionary().ok_or(fsandbox::DictionaryError::NotFound)?
                            }
                            Ok(None) => {
                                let dict = Dict::new();
                                current_dict.insert(
                                    current_name.clone(),
                                    Capability::Dictionary(dict.clone()),
                                )?;
                                dict
                            }
                            Err(_) => return Err(fsandbox::DictionaryError::NotFound),
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
        path: &'a impl IterablePath,
        request: Request,
    ) -> Result<Option<Capability>, RouterError> {
        let mut current_capability: Capability = self.clone().into();
        for next_name in path.iter_segments() {
            // We have another name but no subdictionary, so exit.
            let Capability::Dictionary(current_dict) = &current_capability else { return Ok(None) };

            // Get the capability.
            let capability =
                current_dict.get(next_name).map_err(|_| RoutingError::BedrockNotCloneable)?;

            // The capability doesn't exist.
            let Some(capability) = capability else { return Ok(None) };

            // Resolve the capability, this is a noop if it's not a router.
            current_capability = capability.route(request.clone()).await?;
        }
        Ok(Some(current_capability))
    }
}
