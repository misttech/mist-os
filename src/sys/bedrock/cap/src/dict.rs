// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{AnyCapability, Capability, Remote},
    crate::handle::Handle,
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
#[derive(Debug)]
pub struct Dict {
    pub entries: HashMap<Key, AnyCapability>,
}

impl Capability for Dict {}

impl Dict {
    pub fn new() -> Self {
        Dict { entries: HashMap::new() }
    }

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
                    let mut result = match self.entries.entry(key) {
                        Entry::Occupied(_) => Err(fbedrock::DictError::AlreadyExists),
                        Entry::Vacant(entry) => {
                            entry.insert(Box::new(Handle::from(value)));
                            Ok(())
                        }
                    };
                    responder.send(&mut result).context("failed to send response")?;
                }
                fbedrock::DictRequest::Remove { key, responder } => {
                    let cap = self.entries.remove(&key);
                    let mut result = match cap {
                        Some(cap) => {
                            let (handle, fut) = cap.to_zx_handle();
                            if let Some(fut) = fut {
                                entry_tasks.push(fasync::Task::spawn(fut));
                            }
                            Ok(handle)
                        }
                        None => Err(fbedrock::DictError::NotFound),
                    };
                    responder.send(&mut result).context("failed to send response")?;
                }
            }
        }

        Ok(())
    }
}

impl Remote for Dict {
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

#[cfg(test)]
mod tests {
    use {
        crate::{dict::*, handle::Handle},
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
        let mut dict = Dict::new();

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
        let mut dict = Dict::new();

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

    /// Tests that the `Dict.Insert` returns `ALREADY_EXISTS` when there is already an item with
    /// the same key.
    #[fuchsia::test]
    async fn insert_already_exists() -> Result<(), Error> {
        let mut dict = Dict::new();

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
        let mut dict = Dict::new();

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
}
