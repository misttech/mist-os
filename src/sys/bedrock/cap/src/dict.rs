// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{AnyCapability, AnyCloneCapability, Capability, Remote},
    anyhow::{Context, Error},
    fidl::endpoints::create_request_stream,
    fidl_fuchsia_component_bedrock as fbedrock, fuchsia_async as fasync, fuchsia_zircon as zx,
    fuchsia_zircon::HandleBased,
    futures::TryStreamExt,
    futures::{future::BoxFuture, FutureExt},
    std::collections::hash_map::Entry,
    std::collections::HashMap,
};

pub type Key = String;

/// A capability that represents a dictionary of capabilities.
#[derive(Debug, Clone)]
pub struct Dict<T> {
    pub entries: HashMap<Key, T>,
}

impl<T> Dict<T> {
    pub fn new() -> Self {
        Dict { entries: HashMap::new() }
    }
}

impl<T: ?Sized + Capability> Dict<Box<T>>
where
    Box<T>: TryFrom<zx::Handle>,
{
    /// Serve the `fuchsia.component.bedrock.Dict` protocol for this `Dict`.
    pub async fn serve_dict(
        &mut self,
        mut stream: fbedrock::DictRequestStream,
    ) -> Result<(), Error> {
        // Tasks that serve the zx handles for entries removed from this dict.
        let mut entry_tasks = vec![];

        while let Some(request) =
            stream.try_next().await.context("failed to read request from stream")?
        {
            match request {
                fbedrock::DictRequest::Insert { key, value, responder, .. } => {
                    let result = match self.entries.entry(key) {
                        Entry::Occupied(_) => Err(fbedrock::DictError::AlreadyExists),
                        Entry::Vacant(entry) => match Box::<T>::try_from(value) {
                            Ok(cap) => {
                                entry.insert(cap);
                                Ok(())
                            }
                            Err(_) => Err(fbedrock::DictError::BadHandle),
                        },
                    };
                    responder.send(result).context("failed to send response")?;
                }
                fbedrock::DictRequest::Remove { key, responder } => {
                    let cap = self.entries.remove(&key);
                    let result = match cap {
                        Some(cap) => {
                            let (handle, fut) = cap.to_zx_handle();
                            if let Some(fut) = fut {
                                entry_tasks.push(fasync::Task::spawn(fut));
                            }
                            Ok(handle)
                        }
                        None => Err(fbedrock::DictError::NotFound),
                    };
                    responder.send(result).context("failed to send response")?;
                }
            }
        }

        Ok(())
    }

    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (dict_client_end, dict_stream) =
            create_request_stream::<fbedrock::DictMarker>().unwrap();

        let fut = async move {
            let mut dict = *self;
            dict.serve_dict(dict_stream).await.expect("failed to serve Dict");
        };

        (dict_client_end.into_handle(), Some(fut.boxed()))
    }
}

impl Remote for Dict<AnyCapability> {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        self.to_zx_handle()
    }
}

impl Remote for Dict<AnyCloneCapability> {
    fn to_zx_handle(self: Box<Self>) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        self.to_zx_handle()
    }
}

impl Capability for Dict<AnyCapability> {}
impl Capability for Dict<AnyCloneCapability> {}

#[cfg(test)]
mod tests {
    use {
        crate::{
            dict::*,
            handle::{CloneHandle, Handle},
        },
        anyhow::{anyhow, Error},
        assert_matches::assert_matches,
        fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_component_bedrock as fbedrock,
        fuchsia_zircon::{self as zx, AsHandleRef},
        futures::try_join,
    };

    const CAP_KEY: &str = "cap";

    /// Tests that the `Dict` contains an entry for a capability inserted via `Dict.Insert`,
    /// and that the value is the same capability.
    #[fuchsia::test]
    async fn serve_insert() -> Result<(), Error> {
        let mut dict = Dict::<AnyCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        let client = async move {
            dict_proxy
                .insert(CAP_KEY, event.into_handle())
                .await
                .context("failed to call Insert")?
                .map_err(|err| anyhow!("failed to insert: {:?}", err))?;

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        // Inserting adds the entry to `entries`.
        assert_eq!(dict.entries.len(), 1);

        // The entry that was inserted should now be in `entries`.
        let handle = dict
            .entries
            .get(CAP_KEY)
            .ok_or_else(|| anyhow!("not in entries after insert"))?
            .downcast_ref::<Handle>()
            .ok_or_else(|| anyhow!("entry is not a Handle"))?;
        let got_koid = handle.get_koid().unwrap();
        assert_eq!(got_koid, expected_koid);

        Ok(())
    }

    /// Tests that removing an entry from the `Dict` via `Dict.Remove` yields the same capability
    /// that was previously inserted.
    #[fuchsia::test]
    async fn serve_remove() -> Result<(), Error> {
        let mut dict = Dict::<AnyCapability>::new();

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        // Put the event into the dict as a `Handle`.
        dict.entries.insert(CAP_KEY.to_string(), Box::new(Handle::from(event.into_handle())));
        assert_eq!(dict.entries.len(), 1);

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let cap = dict_proxy
                .remove(CAP_KEY)
                .await
                .context("failed to call Remove")?
                .map_err(|err| anyhow!("failed to remove: {:?}", err))?;

            // The entry should be the same one that was previously inserted.
            let got_koid = cap.get_koid().unwrap();
            assert_eq!(got_koid, expected_koid);

            Ok(())
        };

        try_join!(client, server).map(|_| ())?;

        // Removing the entry with Remove should remove it from `entries`.
        assert!(dict.entries.is_empty());

        Ok(())
    }

    /// Tests that `Dict.Insert` returns `ALREADY_EXISTS` when there is already an item with
    /// the same key.
    #[fuchsia::test]
    async fn insert_already_exists() -> Result<(), Error> {
        let mut dict = Dict::<AnyCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let event = zx::Event::create();

            // Insert an entry.
            dict_proxy
                .insert(CAP_KEY, event.into_handle())
                .await
                .context("failed to call Insert")?
                .map_err(|err| anyhow!("failed to insert: {:?}", err))?;

            let event = zx::Event::create();

            // Inserting again should return an error.
            let result = dict_proxy
                .insert(CAP_KEY, event.into_handle())
                .await
                .context("failed to call Insert")?;
            assert_matches!(result, Err(fbedrock::DictError::AlreadyExists));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that the `Dict.Remove` returns `NOT_FOUND` when there is no item with the given key.
    #[fuchsia::test]
    async fn remove_not_found() -> Result<(), Error> {
        let mut dict = Dict::<AnyCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            // Removing an item from an empty dict should fail.
            let result = dict_proxy.remove(CAP_KEY).await.context("failed to call Remove")?;
            assert_matches!(result, Err(fbedrock::DictError::NotFound));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that `Dict<AnyCloneCapability>.Insert` let the client to insert a cloneable handle.
    #[fuchsia::test]
    async fn clone_insert() -> Result<(), Error> {
        let mut dict = Dict::<AnyCloneCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let event = zx::Event::create();
            let handle = event.into_handle();

            let result =
                dict_proxy.insert(CAP_KEY, handle).await.context("failed to call Insert")?;
            assert_matches!(result, Ok(()));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that `Dict<AnyCloneCapability>.Insert` returns `BAD_HANDLE` when the handle
    /// does not have ZX_RIGHT_DUPLICATE.
    #[fuchsia::test]
    async fn clone_insert_bad_handle() -> Result<(), Error> {
        let mut dict = Dict::<AnyCloneCapability>::new();

        let (dict_proxy, dict_stream) = create_proxy_and_stream::<fbedrock::DictMarker>()?;
        let server = dict.serve_dict(dict_stream);

        let client = async move {
            let event = zx::Event::create();

            // Create a handle without ZX_RIGHT_DUPLICATE.
            let handle =
                event.as_handle_ref().duplicate(zx::Rights::BASIC - zx::Rights::DUPLICATE).unwrap();

            let result =
                dict_proxy.insert(CAP_KEY, handle).await.context("failed to call Insert")?;
            assert_matches!(result, Err(fbedrock::DictError::BadHandle));

            Ok(())
        };

        try_join!(client, server).map(|_| ())
    }

    /// Tests that cloning a `Dict<AnyCloneCapability>` with an item results in a Dict that
    /// contains a handle to the same object.
    #[fuchsia::test]
    async fn clone_contains_same_object() -> Result<(), Error> {
        let mut dict = Dict::<AnyCloneCapability>::new();

        // Create an event and get its koid.
        let event = zx::Event::create();
        let expected_koid = event.get_koid().unwrap();

        // Put the event into the dict as a `CloneHandle`.
        dict.entries.insert(
            CAP_KEY.to_string(),
            Box::new(CloneHandle::try_from(event.into_handle()).unwrap()),
        );
        assert_eq!(dict.entries.len(), 1);

        let dict_clone = dict.clone();

        assert_eq!(dict_clone.entries.len(), 1);
        let event_clone = dict.entries.remove(CAP_KEY).unwrap();

        let (handle, _) = event_clone.to_zx_handle();
        let got_koid = handle.as_handle_ref().get_koid().unwrap();

        assert_eq!(got_koid, expected_koid);

        Ok(())
    }
}
