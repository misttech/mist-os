// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    cap::{dict::Key, open::Open, AnyCapability, Dict},
    fuchsia_runtime::{HandleInfo, HandleType},
    futures::channel::mpsc::UnboundedSender,
    futures::future::{BoxFuture, FutureExt},
    futures::stream::{FuturesUnordered, StreamExt},
    namespace::Namespace,
    process_builder::StartupHandle,
    processargs::ProcessArgs,
    std::collections::HashMap,
    std::iter::once,
    thiserror::Error,
};

mod namespace;

/// How to deliver a particular capability from a dict to an Elf process. Broadly speaking,
/// one could either deliver a capability using namespace entries, or using numbered handles.
pub enum Delivery {
    /// Install the capability as a `fuchsia.io` object, within some parent directory serviced by
    /// the framework, and discoverable at a path such as "/svc/foo/bar".
    ///
    /// As a result, a namespace entry will be created in the resulting processargs, corresponding
    /// to the parent directory, e.g. "/svc/foo".
    ///
    /// For example, installing a `cap::open::Open` at "/svc/fuchsia.examples.Echo" will
    /// cause the framework to spin up a `fuchsia.io/Directory` implementation backing "/svc",
    /// containing a filesystem object named "fuchsia.examples.Echo".
    ///
    /// Not all capability types are installable as `fuchsia.io` objects. A one-shot handle is not
    /// supported because `fuchsia.io` does not have a protocol for delivering one-shot handles.
    /// Use [Delivery::Handle] for those.
    NamespacedObject(cm_types::Path),

    /// Install the capability as a `fuchsia.io` object by creating a namespace entry at the
    /// provided path. The difference between [Delivery::NamespacedObject] and
    /// [Delivery::NamespaceEntry] is that the former will create a namespace entry at the parent
    /// directory.
    NamespaceEntry(cm_types::Path),

    /// Installs the Zircon handle representation of this capability at the processargs slot
    /// described by [HandleInfo].
    ///
    /// The following handle types are disallowed because they will collide with the implementation
    /// of incoming namespace and outgoing directory:
    ///
    /// - [HandleType::NamespaceDirectory]
    /// - [HandleType::DirectoryRequest]
    ///
    Handle(HandleInfo),
}

pub enum DeliveryMapEntry {
    Delivery(Delivery),
    Dict(DeliveryMap),
}

/// A nested dictionary mapping capability names to delivery method.
///
/// Each entry in a [Dict] should have a corresponding entry here describing how the
/// capability will be delivered to the process. If a [Dict] has a nested [Dict], then there
/// will be a corresponding nested [DeliveryMapEntry::Dict] containing the [DeliveryMap] for the
/// capabilities in the nested [Dict].
pub type DeliveryMap = HashMap<Key, DeliveryMapEntry>;

/// Visits `dict` and installs its capabilities into appropriate locations in the
/// `processargs`, as determined by a `delivery_map`.
///
/// If the process opens non-existent paths within one of the namespace entries served
/// by the framework, that path will be sent down `not_found`. Callers should either monitor
/// the stream, or drop the receiver, to prevent unbounded buffering.
///
/// On success, returns a future that services connection requests to those capabilities.
/// The future will complete if there is no more work possible, such as if all connections
/// to the items in the dictionary are closed.
pub fn add_to_processargs(
    dict: Dict,
    processargs: &mut ProcessArgs,
    delivery_map: &DeliveryMap,
    not_found: UnboundedSender<String>,
) -> Result<BoxFuture<'static, ()>, DeliveryError> {
    let mut futures: Vec<BoxFuture<'static, ()>> = Vec::new();
    let mut namespace = Namespace::new(not_found);

    // Iterate over the delivery map.
    // Take entries away from dict and install them accordingly.
    let dict = visit_map(delivery_map, dict, &mut |cap: AnyCapability, delivery: &Delivery| {
        match delivery {
            Delivery::NamespacedObject(path) => {
                let open: Open = cap
                    .try_into()
                    .map_err(|_| DeliveryError::NamespacedObjectDoesNotSupportOpen(path.clone()))?;
                namespace.add_open(open, path).map_err(DeliveryError::NamespaceError)
            }
            // TODO: implement namespace entry
            Delivery::NamespaceEntry(_) => todo!(),
            Delivery::Handle(info) => {
                processargs.add_handles(once(translate_handle(cap, info, &mut futures)?));
                Ok(())
            }
        }
    })?;

    // Finally, verify that all dict entries are empty (or empty dictionaries).
    check_empty_dict(&dict)?;

    let (namespace, namespace_fut) = namespace.serve();
    processargs.namespace_entries.extend(namespace);
    futures.push(namespace_fut);

    let mut futures_unordered = FuturesUnordered::new();
    futures_unordered.extend(futures);

    let fut = async move { while let Some(()) = futures_unordered.next().await {} };
    Ok(fut.boxed())
}

#[derive(Error, Debug)]
pub enum DeliveryError {
    #[error("the key `{0}` is not found in the dict")]
    NotInDict(Key),

    #[error("wrong type: the delivery map expected `{0}` to be a nested Dict in the dict")]
    NotADict(Key),

    #[error("unused capability in dict: `{0}`")]
    UnusedCapability(Key),

    #[error("handle type `{0:?}` is not allowed to be installed into processargs")]
    UnsupportedHandleType(HandleType),

    #[error("namespace configuration error: `{0}`")]
    NamespaceError(namespace::NamespaceError),

    #[error("to install the capability as a namespaced object at `{0}`, it must support Open")]
    NamespacedObjectDoesNotSupportOpen(cm_types::Path),
}

fn translate_handle(
    cap: AnyCapability,
    info: &HandleInfo,
    handle_futures: &mut Vec<BoxFuture<'static, ()>>,
) -> Result<StartupHandle, DeliveryError> {
    validate_handle_type(info.handle_type())?;

    let (h, fut) = cap.to_zx_handle();
    if let Some(fut) = fut {
        handle_futures.push(fut);
    }

    Ok(StartupHandle { handle: h, info: *info })
}

fn visit_map(
    map: &DeliveryMap,
    mut dict: Dict,
    f: &mut impl FnMut(AnyCapability, &Delivery) -> Result<(), DeliveryError>,
) -> Result<Dict, DeliveryError> {
    for (key, entry) in map {
        match dict.entries.remove(key) {
            Some(value) => match entry {
                DeliveryMapEntry::Delivery(delivery) => f(value, delivery)?,
                DeliveryMapEntry::Dict(sub_map) => {
                    let nested_dict: Box<Dict> =
                        value.downcast().map_err(|_| DeliveryError::NotADict(key.to_owned()))?;
                    dict.entries
                        .insert(key.to_owned(), Box::new(visit_map(sub_map, *nested_dict, f)?));
                }
            },
            None => return Err(DeliveryError::NotInDict(key.to_owned())),
        }
    }
    Ok(dict)
}

fn check_empty_dict(dict: &Dict) -> Result<(), DeliveryError> {
    for (key, entry) in dict.entries.iter() {
        if let Some(nested_dict) = entry.downcast_ref::<Dict>() {
            // Recursively check nested dictionary.
            check_empty_dict(&nested_dict)?;
        } else {
            return Err(DeliveryError::UnusedCapability(key.to_owned()));
        }
    }
    Ok(())
}

fn validate_handle_type(handle_type: HandleType) -> Result<(), DeliveryError> {
    match handle_type {
        HandleType::NamespaceDirectory | HandleType::DirectoryRequest => {
            Err(DeliveryError::UnsupportedHandleType(handle_type))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod test_util {
    use {
        cap::{open::Open, Handle},
        fidl_fuchsia_io as fio, fuchsia_async as fasync, fuchsia_zircon as zx,
        fuchsia_zircon::HandleBased,
        vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope, service},
    };

    pub struct Receiver(pub async_channel::Receiver<Handle>);

    pub fn multishot() -> (Open, Receiver) {
        let (sender, receiver) = async_channel::unbounded::<Handle>();

        let open_fn = move |scope: ExecutionScope, channel: fasync::Channel| {
            let sender = sender.clone();
            scope.spawn(async move {
                let capability = Handle::from(channel.into_zx_channel().into_handle());
                let _ = sender.send(capability).await;
            });
        };
        let service = service::endpoint(open_fn);

        let open_fn = move |scope: ExecutionScope,
                            flags: fio::OpenFlags,
                            path: vfs::path::Path,
                            server_end: zx::Channel| {
            service.clone().open(scope, flags, path, server_end.into());
        };

        let open = Open::new(open_fn, fio::DirentType::Service);

        (open, Receiver(receiver))
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        anyhow::Result,
        assert_matches::assert_matches,
        fidl::endpoints::Proxy,
        fidl_fuchsia_io as fio, fuchsia_async as fasync,
        fuchsia_fs::directory::DirEntry,
        fuchsia_zircon as zx,
        fuchsia_zircon::{AsHandleRef, HandleBased, Peered},
        maplit::hashmap,
        namespace::ignore_not_found as ignore,
        std::str::FromStr,
        test_util::multishot,
    };

    #[fuchsia::test]
    async fn test_empty() -> Result<()> {
        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        let delivery_map = DeliveryMap::new();
        let fut = add_to_processargs(dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.namespace_entries.len(), 0);
        assert_eq!(processargs.handles.len(), 0);

        drop(processargs);
        fut.await;
        Ok(())
    }

    #[fuchsia::test]
    async fn test_handle() -> Result<()> {
        let (sock0, sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();
        let mut dict = Dict::new();
        dict.entries.insert("stdin".to_string(), sock0.into());
        let delivery_map = hashmap! {
            "stdin".to_string() => DeliveryMapEntry::Delivery(
                Delivery::Handle(HandleInfo::new(HandleType::FileDescriptor, 0))
            )
        };
        let fut = add_to_processargs(dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.namespace_entries.len(), 0);
        assert_eq!(processargs.handles.len(), 1);

        assert_eq!(processargs.handles[0].info.handle_type(), HandleType::FileDescriptor);
        assert_eq!(processargs.handles[0].info.arg(), 0);

        // Test connectivity.
        const PAYLOAD: &'static [u8] = b"Hello";
        let handles = std::mem::take(&mut processargs.handles);
        let sock0 = zx::Socket::from(handles.into_iter().next().unwrap().handle);
        assert_eq!(sock0.write(PAYLOAD).unwrap(), 5);
        let mut buf = [0u8; PAYLOAD.len() + 1];
        assert_eq!(sock1.read(&mut buf[..]), Ok(PAYLOAD.len()));
        assert_eq!(&buf[..PAYLOAD.len()], PAYLOAD);

        drop(processargs);
        fut.await;
        Ok(())
    }

    #[fuchsia::test]
    async fn test_nested_dict() -> Result<()> {
        let (sock0, _sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();

        // Put a socket at "/handles/stdin". This implements a capability bundling pattern.
        let mut handles = Dict::new();
        handles.entries.insert("stdin".to_string(), sock0.into());
        let mut dict = Dict::new();
        dict.entries.insert("handles".to_string(), Box::new(handles));

        let delivery_map = hashmap! {
            "handles".to_string() => DeliveryMapEntry::Dict(hashmap! {
                "stdin".to_string() => DeliveryMapEntry::Delivery(
                    Delivery::Handle(HandleInfo::new(HandleType::FileDescriptor, 0))
                )
            })
        };
        let fut = add_to_processargs(dict, &mut processargs, &delivery_map, ignore())?;

        assert_eq!(processargs.namespace_entries.len(), 0);
        assert_eq!(processargs.handles.len(), 1);

        assert_eq!(processargs.handles[0].info.handle_type(), HandleType::FileDescriptor);
        assert_eq!(processargs.handles[0].info.arg(), 0);

        drop(processargs);
        fut.await;
        Ok(())
    }

    #[fuchsia::test]
    fn test_wrong_dict_destructuring() {
        let (sock0, _sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();

        // The type of "/handles" is a socket capability but we try to open it as a dict and extract
        // a "stdin" inside. This should fail.
        let mut dict = Dict::new();
        dict.entries.insert("handles".to_string(), sock0.into());

        let delivery_map = hashmap! {
            "handles".to_string() => DeliveryMapEntry::Dict(hashmap! {
                "stdin".to_string() => DeliveryMapEntry::Delivery(
                    Delivery::Handle(HandleInfo::new(HandleType::FileDescriptor, 0))
                )
            })
        };

        assert_matches!(
            add_to_processargs(dict, &mut processargs, &delivery_map, ignore()).err().unwrap(),
            DeliveryError::NotADict(name)
            if &name == "handles"
        );
    }

    #[fuchsia::test]
    async fn test_handle_unused() {
        let (sock0, _sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();
        let mut dict = Dict::new();
        dict.entries.insert("stdin".to_string(), sock0.into());
        let delivery_map = DeliveryMap::new();

        assert_matches!(
            add_to_processargs(dict, &mut processargs, &delivery_map, ignore()).err().unwrap(),
            DeliveryError::UnusedCapability(name)
            if &name == "stdin"
        );
    }

    #[fuchsia::test]
    async fn test_handle_unsupported() {
        let (sock0, _sock1) = zx::Socket::create_stream();

        let mut processargs = ProcessArgs::new();
        let mut dict = Dict::new();
        dict.entries.insert("stdin".to_string(), sock0.into());
        let delivery_map = hashmap! {
            "stdin".to_string() => DeliveryMapEntry::Delivery(
                Delivery::Handle(HandleInfo::new(HandleType::DirectoryRequest, 0))
            )
        };

        assert_matches!(
            add_to_processargs(dict, &mut processargs, &delivery_map, ignore()).err().unwrap(),
            DeliveryError::UnsupportedHandleType(handle_type)
            if handle_type == HandleType::DirectoryRequest
        );
    }

    #[fuchsia::test]
    async fn test_handle_not_found() {
        let mut processargs = ProcessArgs::new();
        let dict = Dict::new();
        let delivery_map = hashmap! {
            "stdin".to_string() => DeliveryMapEntry::Delivery(
                Delivery::Handle(HandleInfo::new(HandleType::FileDescriptor, 0))
            )
        };

        assert_matches!(
            add_to_processargs(dict, &mut processargs, &delivery_map, ignore()).err().unwrap(),
            DeliveryError::NotInDict(name)
            if &name == "stdin"
        );
    }

    /// Two protocol capabilities in `/svc`. One of them has a receiver waiting for incoming
    /// requests. The other is disconnected from the receiver, which should close all incoming
    /// connections to that protocol.
    #[fuchsia::test]
    async fn test_namespace_end_to_end() -> Result<()> {
        let (open, receiver) = multishot();
        let peer_closed_open = multishot().0;

        let mut processargs = ProcessArgs::new();
        let mut dict = Dict::new();
        dict.entries.insert("normal".to_string(), Box::new(open));
        dict.entries.insert("closed".to_string(), Box::new(peer_closed_open));
        let delivery_map = hashmap! {
            "normal".to_string() => DeliveryMapEntry::Delivery(
                Delivery::NamespacedObject(cm_types::Path::from_str("/svc/fuchsia.Normal").unwrap())
            ),
            "closed".to_string() => DeliveryMapEntry::Delivery(
                Delivery::NamespacedObject(cm_types::Path::from_str("/svc/fuchsia.Closed").unwrap())
            )
        };
        let fut = add_to_processargs(dict, &mut processargs, &delivery_map, ignore())?;
        let fut = fasync::Task::spawn(fut);

        assert_eq!(processargs.handles.len(), 0);
        assert_eq!(processargs.namespace_entries.len(), 1);
        let entry = processargs.namespace_entries.pop().unwrap();
        assert_eq!(entry.path.to_str().unwrap(), "/svc");

        // Check that there are the expected two protocols inside the svc directory.
        let dir = entry.directory.into_proxy().unwrap();
        let mut entries = fuchsia_fs::directory::readdir(&dir).await.unwrap();
        let mut expectation = vec![
            DirEntry { name: "fuchsia.Normal".to_string(), kind: fio::DirentType::Service },
            DirEntry { name: "fuchsia.Closed".to_string(), kind: fio::DirentType::Service },
        ];
        entries.sort();
        expectation.sort();
        assert_eq!(entries, expectation);

        let dir = dir.into_channel().unwrap().into_zx_channel();

        // Connect to the protocol using namespace functionality.
        let (client_end, server_end) = zx::Channel::create();
        fdio::service_connect_at(&dir, "fuchsia.Normal", server_end).unwrap();

        // Make sure the server_end is received, and test connectivity.
        let server_end: zx::Channel = receiver.0.recv().await.unwrap().into_handle().into();
        client_end.signal_peer(zx::Signals::empty(), zx::Signals::USER_0).unwrap();
        server_end.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST).unwrap();

        // Connect to the closed protocol. Because the receiver is discarded, anything we send
        // should get peer-closed.
        let (client_end, server_end) = zx::Channel::create();
        fdio::service_connect_at(&dir, "fuchsia.Closed", server_end).unwrap();
        fasync::Channel::from_channel(client_end).unwrap().on_closed().await.unwrap();

        drop(dir);
        drop(processargs);
        fut.await;
        Ok(())
    }

    #[fuchsia::test]
    async fn dropping_future_stops_everything() -> Result<()> {
        let (open, _receiver) = multishot();

        let mut processargs = ProcessArgs::new();
        let mut dict = Dict::new();
        dict.entries.insert("a".to_string(), Box::new(open));
        let delivery_map = hashmap! {
            "a".to_string() => DeliveryMapEntry::Delivery(
                Delivery::NamespacedObject(cm_types::Path::from_str("/svc/a").unwrap())
            ),
        };
        let fut = add_to_processargs(dict, &mut processargs, &delivery_map, ignore())?;
        drop(fut);

        assert_eq!(processargs.namespace_entries.len(), 1);
        let dir = processargs.namespace_entries.pop().unwrap().directory.into_proxy().unwrap();
        let dir = dir.into_channel().unwrap().into_zx_channel();

        let (client_end, server_end) = zx::Channel::create();
        fdio::service_connect_at(&dir, "a", server_end).unwrap();
        fasync::Channel::from_channel(client_end).unwrap().on_closed().await.unwrap();
        Ok(())
    }
}
