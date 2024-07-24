// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::dict::Key;
use crate::fidl::registry;
use crate::{Capability, Dict};
use fidl::handle::Signals;
use fidl::AsHandleRef;
use futures::{FutureExt, TryStreamExt};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use tracing::warn;
use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

type Store = HashMap<u64, Capability>;

pub async fn serve_capability_store(
    mut stream: fsandbox::CapabilityStoreRequestStream,
) -> Result<(), fidl::Error> {
    let mut store: Store = Default::default();
    while let Some(request) = stream.try_next().await? {
        match request {
            fsandbox::CapabilityStoreRequest::Duplicate { id, dest_id, responder } => {
                let result = (|| {
                    let cap =
                        store.get(&id).ok_or_else(|| fsandbox::CapabilityStoreError::IdNotFound)?;
                    let cap = cap
                        .try_clone()
                        .map_err(|_| fsandbox::CapabilityStoreError::NotDuplicatable)?;
                    insert_capability(&mut store, dest_id, cap)
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::Drop { id, responder } => {
                let result = store
                    .remove(&id)
                    .map(|_| ())
                    .ok_or_else(|| fsandbox::CapabilityStoreError::IdNotFound);
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::Export { id, responder } => {
                let result = (|| {
                    let cap = store
                        .remove(&id)
                        .ok_or_else(|| fsandbox::CapabilityStoreError::IdNotFound)?;
                    Ok(cap.into())
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::Import { id, capability, responder } => {
                let result = (|| {
                    let capability = capability
                        .try_into()
                        .map_err(|_| fsandbox::CapabilityStoreError::BadCapability)?;
                    insert_capability(&mut store, id, capability)
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::DictionaryCreate { id, responder } => {
                let result = insert_capability(&mut store, id, Capability::Dictionary(Dict::new()));
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::DictionaryLegacyImport {
                id,
                client_end,
                responder,
            } => {
                let result = (|| {
                    let capability = Dict::try_from(client_end)
                        .map_err(|_| fsandbox::CapabilityStoreError::BadCapability)?
                        .into();
                    insert_capability(&mut store, id, capability)
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::DictionaryLegacyExport {
                id,
                server_end,
                responder,
            } => {
                let result = (|| {
                    let cap = store
                        .remove(&id)
                        .ok_or_else(|| fsandbox::CapabilityStoreError::IdNotFound)?;
                    let Capability::Dictionary(_) = &cap else {
                        return Err(fsandbox::CapabilityStoreError::WrongType);
                    };
                    let koid = server_end.basic_info().unwrap().related_koid;
                    registry::insert(
                        cap,
                        koid,
                        fasync::OnSignals::new(server_end, Signals::OBJECT_PEER_CLOSED).map(|_| ()),
                    );
                    Ok(())
                })();
                responder.send(result)?
            }
            fsandbox::CapabilityStoreRequest::DictionaryInsert { id, item, responder } => {
                let result = (|| {
                    let this = get_dictionary(&store, id)?;
                    let this = this.clone();
                    let key =
                        item.key.parse().map_err(|_| fsandbox::CapabilityStoreError::InvalidKey)?;
                    let value = store
                        .remove(&item.value)
                        .ok_or_else(|| fsandbox::CapabilityStoreError::IdNotFound)?;
                    this.insert(key, value)
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::DictionaryGet { id, key, dest_id, responder } => {
                let result = (|| {
                    let this = get_dictionary(&store, id)?;
                    let key =
                        Key::new(key).map_err(|_| fsandbox::CapabilityStoreError::InvalidKey)?;
                    let cap = match this.get(&key) {
                        Ok(Some(cap)) => Ok(cap),
                        Ok(None) => {
                            (this.lock().not_found)(key.as_str());
                            Err(fsandbox::CapabilityStoreError::ItemNotFound)
                        }
                        Err(()) => Err(fsandbox::CapabilityStoreError::NotDuplicatable),
                    }?;
                    insert_capability(&mut store, dest_id, cap)
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::DictionaryRemove { id, key, dest_id, responder } => {
                let result = (|| {
                    let this = get_dictionary(&store, id)?;
                    let key =
                        Key::new(key).map_err(|_| fsandbox::CapabilityStoreError::InvalidKey)?;
                    // Check this before removing from the dictionary.
                    if let Some(dest_id) = dest_id.as_ref() {
                        if store.contains_key(&dest_id.value) {
                            return Err(fsandbox::CapabilityStoreError::IdAlreadyExists);
                        }
                    }
                    let cap = match this.remove(&key) {
                        Some(cap) => Ok(cap.into()),
                        None => {
                            (this.lock().not_found)(key.as_str());
                            Err(fsandbox::CapabilityStoreError::ItemNotFound)
                        }
                    }?;
                    if let Some(dest_id) = dest_id.as_ref() {
                        store.insert(dest_id.value, cap);
                    }
                    Ok(())
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::DictionaryCopy { id, dest_id, responder } => {
                let result = (|| {
                    let this = get_dictionary(&store, id)?;
                    let dict = this
                        .shallow_copy()
                        .map_err(|_| fsandbox::CapabilityStoreError::NotDuplicatable)?;
                    insert_capability(&mut store, dest_id, Capability::Dictionary(dict))
                })();
                responder.send(result)?
            }
            fsandbox::CapabilityStoreRequest::DictionaryKeys {
                id,
                iterator: server_end,
                responder,
            } => {
                let result = (|| {
                    let this = get_dictionary(&store, id)?;
                    let keys = this.keys();
                    let stream = server_end.into_stream().unwrap();
                    let mut this = this.lock();
                    this.tasks().spawn(crate::fidl::dict::serve_keys_iterator(keys, stream));
                    Ok(())
                })();
                responder.send(result)?
            }
            fsandbox::CapabilityStoreRequest::_UnknownMethod { ordinal, .. } => {
                warn!("Received unknown CapabilityStore request with ordinal {ordinal}");
            }
        }
    }
    Ok(())
}

fn get_dictionary(store: &Store, id: u64) -> Result<&Dict, fsandbox::CapabilityStoreError> {
    let dict = store.get(&id).ok_or_else(|| fsandbox::CapabilityStoreError::IdNotFound)?;
    if let Capability::Dictionary(dict) = dict {
        Ok(dict)
    } else {
        Err(fsandbox::CapabilityStoreError::WrongType)
    }
}

fn insert_capability(
    store: &mut Store,
    id: u64,
    cap: Capability,
) -> Result<(), fsandbox::CapabilityStoreError> {
    match store.entry(id) {
        Entry::Occupied(_) => Err(fsandbox::CapabilityStoreError::IdAlreadyExists),
        Entry::Vacant(entry) => {
            entry.insert(cap);
            Ok(())
        }
    }
}
