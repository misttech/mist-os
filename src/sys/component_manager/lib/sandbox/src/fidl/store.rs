// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::dict::Key;
use crate::fidl::registry;
use crate::{Capability, Connector, Dict, DirConnector, Message};
use fidl::handle::Signals;
use fidl::AsHandleRef;
use futures::{FutureExt, TryStreamExt};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::{self, Arc, Weak};
use tracing::warn;
use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

type Store = sync::Mutex<HashMap<u64, Capability>>;

pub async fn serve_capability_store(
    mut stream: fsandbox::CapabilityStoreRequestStream,
) -> Result<(), fidl::Error> {
    let outer_store: Arc<Store> = Arc::new(Store::new(Default::default()));
    while let Some(request) = stream.try_next().await? {
        let mut store = outer_store.lock().unwrap();
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
            fsandbox::CapabilityStoreRequest::ConnectorCreate { id, receiver, responder } => {
                let result = (|| {
                    let connector = Connector::new_with_owned_receiver(receiver);
                    insert_capability(&mut store, id, Capability::Connector(connector))
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::ConnectorOpen { id, server_end, responder } => {
                let result = (|| {
                    let this = get_connector(&store, id)?;
                    let _ = this.send(Message { channel: server_end });
                    Ok(())
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::DirConnectorCreate { id, receiver, responder } => {
                let result = (|| {
                    let connector = DirConnector::new_with_owned_receiver(receiver);
                    insert_capability(&mut store, id, Capability::DirConnector(connector))
                })();
                responder.send(result)?;
            }
            fsandbox::CapabilityStoreRequest::DirConnectorOpen { id, server_end, responder } => {
                let result = (|| {
                    let this = get_dir_connector(&store, id)?;
                    let _ = this.send(server_end);
                    Ok(())
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
                        if store.contains_key(&dest_id.id) {
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
                        store.insert(dest_id.id, cap);
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
                    let stream = server_end.into_stream();
                    let mut this = this.lock();
                    this.tasks().spawn(serve_dictionary_keys_iterator(keys, stream));
                    Ok(())
                })();
                responder.send(result)?
            }
            fsandbox::CapabilityStoreRequest::DictionaryEnumerate {
                id,
                iterator: server_end,
                responder,
            } => {
                let result = (|| {
                    let this = get_dictionary(&store, id)?;
                    let items = this.enumerate();
                    let stream = server_end.into_stream();
                    let mut this = this.lock();
                    this.tasks().spawn(serve_dictionary_enumerate_iterator(
                        Arc::downgrade(&outer_store),
                        items,
                        stream,
                    ));
                    Ok(())
                })();
                responder.send(result)?
            }
            fsandbox::CapabilityStoreRequest::DictionaryDrain {
                id,
                iterator: server_end,
                responder,
            } => {
                let result = (|| {
                    let this = get_dictionary(&store, id)?;
                    // Take out entries, replacing with an empty BTreeMap.
                    // They are dropped if the caller does not request an iterator.
                    let items = this.drain();
                    if let Some(server_end) = server_end {
                        let stream = server_end.into_stream();
                        let mut this = this.lock();
                        this.tasks().spawn(serve_dictionary_drain_iterator(
                            Arc::downgrade(&outer_store),
                            items,
                            stream,
                        ));
                    }
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

async fn serve_dictionary_keys_iterator(
    mut keys: impl Iterator<Item = Key>,
    mut stream: fsandbox::DictionaryKeysIteratorRequestStream,
) {
    while let Ok(Some(request)) = stream.try_next().await {
        match request {
            fsandbox::DictionaryKeysIteratorRequest::GetNext { responder } => {
                let mut chunk = vec![];
                for _ in 0..fsandbox::MAX_DICTIONARY_ITERATOR_CHUNK {
                    match keys.next() {
                        Some(key) => {
                            chunk.push(key.into());
                        }
                        None => break,
                    }
                }
                let _ = responder.send(&chunk);
            }
            fsandbox::DictionaryKeysIteratorRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown DictionaryKeysIterator request");
            }
        }
    }
}

async fn serve_dictionary_enumerate_iterator(
    store: Weak<Store>,
    mut items: impl Iterator<Item = (Key, Result<Capability, ()>)>,
    mut stream: fsandbox::DictionaryEnumerateIteratorRequestStream,
) {
    while let Ok(Some(request)) = stream.try_next().await {
        let Some(store) = store.upgrade() else {
            return;
        };
        let mut store = store.lock().unwrap();
        match request {
            fsandbox::DictionaryEnumerateIteratorRequest::GetNext {
                start_id,
                limit,
                responder,
            } => {
                let result = (|| {
                    let mut next_id = start_id;
                    let chunk = get_next_chunk(&*store, &mut items, &mut next_id, limit)?;
                    let end_id = next_id;

                    let chunk: Vec<_> = chunk
                        .into_iter()
                        .map(|(key, value)| {
                            if let Some((capability, id)) = value {
                                store.insert(id, capability);
                                fsandbox::DictionaryOptionalItem {
                                    key: key.into(),
                                    value: Some(Box::new(fsandbox::WrappedCapabilityId { id })),
                                }
                            } else {
                                fsandbox::DictionaryOptionalItem { key: key.into(), value: None }
                            }
                        })
                        .collect();
                    Ok((chunk, end_id))
                })();
                let err = result.is_err();
                let _ = responder.send(result);
                if err {
                    return;
                }
            }
            fsandbox::DictionaryEnumerateIteratorRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown DictionaryEnumerateIterator request");
            }
        }
    }
}

async fn serve_dictionary_drain_iterator(
    store: Weak<Store>,
    items: impl Iterator<Item = (Key, Capability)>,
    mut stream: fsandbox::DictionaryDrainIteratorRequestStream,
) {
    // Transform iterator to be compatible with get_next_chunk()
    let mut items = items.map(|(key, capability)| (key, Ok(capability)));
    while let Ok(Some(request)) = stream.try_next().await {
        let Some(store) = store.upgrade() else {
            return;
        };
        let mut store = store.lock().unwrap();
        match request {
            fsandbox::DictionaryDrainIteratorRequest::GetNext { start_id, limit, responder } => {
                let result = (|| {
                    let mut next_id = start_id;
                    let chunk = get_next_chunk(&*store, &mut items, &mut next_id, limit)?;
                    let end_id = next_id;

                    let chunk: Vec<_> = chunk
                        .into_iter()
                        .map(|(key, value)| {
                            let value = value.expect("unreachable: all values are present");
                            let (capability, id) = value;
                            store.insert(id, capability);
                            fsandbox::DictionaryItem { key: key.into(), value: id }
                        })
                        .collect();
                    Ok((chunk, end_id))
                })();
                match result {
                    Ok((chunk, id)) => {
                        let _ = responder.send(Ok((&chunk[..], id)));
                    }
                    Err(e) => {
                        let _ = responder.send(Err(e));
                        return;
                    }
                }
            }
            fsandbox::DictionaryDrainIteratorRequest::_UnknownMethod { ordinal, .. } => {
                warn!(%ordinal, "Unknown DictionaryDrainIterator request");
            }
        }
    }
}

fn get_next_chunk(
    store: &HashMap<u64, Capability>,
    items: &mut impl Iterator<Item = (Key, Result<Capability, ()>)>,
    next_id: &mut u64,
    limit: u32,
) -> Result<Vec<(Key, Option<(Capability, fsandbox::CapabilityId)>)>, fsandbox::CapabilityStoreError>
{
    if limit == 0 || limit > fsandbox::MAX_DICTIONARY_ITERATOR_CHUNK {
        return Err(fsandbox::CapabilityStoreError::InvalidArgs);
    }

    let mut chunk = vec![];
    for _ in 0..limit {
        match items.next() {
            Some((key, value)) => {
                let value = match value {
                    Ok(value) => {
                        let id = *next_id;
                        // Pre-flight check: if an id is unavailable, return early
                        // and don't make any changes to the store.
                        if store.contains_key(&id) {
                            return Err(fsandbox::CapabilityStoreError::IdAlreadyExists);
                        }
                        *next_id += 1;
                        Some((value, id))
                    }
                    Err(_) => None,
                };
                chunk.push((key, value));
            }
            None => break,
        }
    }
    Ok(chunk)
}

fn get_connector(
    store: &HashMap<u64, Capability>,
    id: u64,
) -> Result<&Connector, fsandbox::CapabilityStoreError> {
    let conn = store.get(&id).ok_or_else(|| fsandbox::CapabilityStoreError::IdNotFound)?;
    if let Capability::Connector(conn) = conn {
        Ok(conn)
    } else {
        Err(fsandbox::CapabilityStoreError::WrongType)
    }
}

fn get_dir_connector(
    store: &HashMap<u64, Capability>,
    id: u64,
) -> Result<&DirConnector, fsandbox::CapabilityStoreError> {
    let conn = store.get(&id).ok_or_else(|| fsandbox::CapabilityStoreError::IdNotFound)?;
    if let Capability::DirConnector(conn) = conn {
        Ok(conn)
    } else {
        Err(fsandbox::CapabilityStoreError::WrongType)
    }
}

fn get_dictionary(
    store: &HashMap<u64, Capability>,
    id: u64,
) -> Result<&Dict, fsandbox::CapabilityStoreError> {
    let dict = store.get(&id).ok_or_else(|| fsandbox::CapabilityStoreError::IdNotFound)?;
    if let Capability::Dictionary(dict) = dict {
        Ok(dict)
    } else {
        Err(fsandbox::CapabilityStoreError::WrongType)
    }
}

fn insert_capability(
    store: &mut HashMap<u64, Capability>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Data;
    use assert_matches::assert_matches;
    use fidl::{endpoints, HandleBased};

    #[fuchsia::test]
    async fn import_export() {
        let (store, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));

        let (ch, _) = fidl::Channel::create();
        let handle = ch.into_handle();
        let handle_koid = handle.get_koid().unwrap();
        let cap1 = Capability::Handle(handle.into());
        let cap2 = Capability::Data(Data::Int64(42));
        store.import(1, cap1.into()).await.unwrap().unwrap();
        store.import(2, cap2.into()).await.unwrap().unwrap();

        let cap1 = store.export(1).await.unwrap().unwrap();
        let cap2 = store.export(2).await.unwrap().unwrap();
        assert_matches!(
            cap1,
            fsandbox::Capability::Handle(h) if h.get_koid().unwrap() == handle_koid
        );
        assert_matches!(
            cap2,
            fsandbox::Capability::Data(fsandbox::Data::Int64(i)) if i == 42
        );
    }

    #[fuchsia::test]
    async fn import_error() {
        let (store, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));

        let cap1 = Capability::Data(Data::Int64(42));
        store.import(1, cap1.try_clone().unwrap().into()).await.unwrap().unwrap();
        assert_matches!(
            store.import(1, cap1.into()).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::IdAlreadyExists)
        );

        let (token, _) = fidl::EventPair::create();
        let bad_connector = fsandbox::Capability::Connector(fsandbox::Connector { token });
        assert_matches!(
            store.import(2, bad_connector).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::BadCapability)
        );
    }

    #[fuchsia::test]
    async fn export_error() {
        let (store, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));

        let cap1 = Capability::Data(Data::Int64(42));
        store.import(1, cap1.try_clone().unwrap().into()).await.unwrap().unwrap();

        assert_matches!(
            store.export(2).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::IdNotFound)
        );
    }

    #[fuchsia::test]
    async fn drop() {
        let (store, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));

        let (ch, _) = fidl::Channel::create();
        let handle = ch.into_handle();
        let handle_koid = handle.get_koid().unwrap();
        let cap1 = Capability::Handle(handle.into());
        let cap2 = Capability::Data(Data::Int64(42));
        store.import(1, cap1.into()).await.unwrap().unwrap();
        store.import(2, cap2.into()).await.unwrap().unwrap();

        // Drop capability 2. It's no longer in the store.
        store.drop(2).await.unwrap().unwrap();
        assert_matches!(
            store.export(1).await.unwrap(),
            Ok(fsandbox::Capability::Handle(h)) if h.get_koid().unwrap() == handle_koid
        );
        assert_matches!(
            store.export(2).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::IdNotFound)
        );

        // Id 2 can be reused.
        let cap2 = Capability::Data(Data::Int64(84));
        store.import(2, cap2.into()).await.unwrap().unwrap();
        assert_matches!(
            store.export(2).await.unwrap(),
            Ok(fsandbox::Capability::Data(fsandbox::Data::Int64(i))) if i == 84
        );
    }

    #[fuchsia::test]
    async fn drop_error() {
        let (store, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));

        let cap1 = Capability::Data(Data::Int64(42));
        store.import(1, cap1.into()).await.unwrap().unwrap();

        assert_matches!(
            store.drop(2).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::IdNotFound)
        );
    }

    #[fuchsia::test]
    async fn duplicate() {
        let (store, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));

        let (event, _) = fidl::EventPair::create();
        let handle = event.into_handle();
        let handle_koid = handle.get_koid().unwrap();
        let cap1 = Capability::Handle(handle.into());
        store.import(1, cap1.into()).await.unwrap().unwrap();
        store.duplicate(1, 2).await.unwrap().unwrap();
        store.drop(1).await.unwrap().unwrap();

        let cap1 = store.export(2).await.unwrap().unwrap();
        assert_matches!(
            cap1,
            fsandbox::Capability::Handle(h) if h.get_koid().unwrap() == handle_koid
        );
    }

    #[fuchsia::test]
    async fn duplicate_error() {
        let (store, stream) =
            endpoints::create_proxy_and_stream::<fsandbox::CapabilityStoreMarker>().unwrap();
        let _server = fasync::Task::spawn(serve_capability_store(stream));

        assert_matches!(
            store.duplicate(1, 2).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::IdNotFound)
        );

        let cap1 = Capability::Data(Data::Int64(42));
        let cap2 = Capability::Data(Data::Int64(84));
        store.import(1, cap1.into()).await.unwrap().unwrap();
        store.import(2, cap2.into()).await.unwrap().unwrap();
        assert_matches!(
            store.duplicate(1, 2).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::IdAlreadyExists)
        );

        // Channels do not support duplication.
        let (ch, _) = fidl::Channel::create();
        let handle = ch.into_handle();
        let cap1 = Capability::Handle(handle.into());
        store.import(3, cap1.into()).await.unwrap().unwrap();
        assert_matches!(
            store.duplicate(3, 4).await.unwrap(),
            Err(fsandbox::CapabilityStoreError::NotDuplicatable)
        );
    }
}
