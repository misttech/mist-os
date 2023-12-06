// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, Context, Error, Result},
    fidl::endpoints,
    fidl::endpoints::ControlHandle,
    fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_testing_harness::{OperationError, RealmProxy_Request, RealmProxy_RequestStream},
    fuchsia_async as fasync,
    fuchsia_component_test::RealmInstance,
    fuchsia_zircon as zx,
    futures::{Future, StreamExt, TryStreamExt},
    tracing::error,
};

// RealmProxy mediates a test suite's access to the services in a test realm.
pub trait RealmProxy {
    // Connects to the named service in this proxy's realm.
    //
    // If the connection fails, the resulting [OperationError] is determined
    // by the [RealmProxy] implementation.
    fn connect_to_named_protocol(
        &mut self,
        protocol: &str,
        server_end: zx::Channel,
    ) -> Result<(), OperationError>;
}

// A default [RealmProxy] implementation that mediates access to a specific [RealmInstance].
struct RealmInstanceProxy(RealmInstance);

impl RealmProxy for RealmInstanceProxy {
    fn connect_to_named_protocol(
        &mut self,
        protocol: &str,
        server_end: zx::Channel,
    ) -> Result<(), OperationError> {
        let res =
            self.0.root.connect_request_to_named_protocol_at_exposed_dir(protocol, server_end);

        if let Some(err) = res.err() {
            error!("{:?}", err);
            return Err(OperationError::Failed);
        }

        Ok(())
    }
}

// serve_with_proxy uses [proxy] to handle all requests from [stream].
//
// This function is useful when implementing a custom test harness that serves
// other protocols in addition to the RealmProxy protocol.
//
// # Example Usage
//
// ```
// #[fuchsia::main(logging = true)]
// async fn main() -> Result<(), Error> {
//   let mut fs = ServiceFs::new();
//
//   fs.dir("svc").add_fidl_service(|stream| {
//     fasync::Task::spawn(async move {
//       let realm = build_realm().await.unwrap();
//       let realm_proxy = MyCustomRealmProxy(realm);
//       realm_proxy::service::serve_with_proxy(realm_proxy, stream).await.unwrap();
//     }).detach();
//   });
//
//   fs.take_and_serve_directory_handle()?;
//   fs.collect::<()>().await;
//   Ok(())
// }
// ```
pub async fn serve_with_proxy<P: RealmProxy>(
    mut proxy: P,
    mut stream: RealmProxy_RequestStream,
) -> Result<(), Error> {
    while let Some(Ok(request)) = stream.next().await {
        match request {
            RealmProxy_Request::ConnectToNamedProtocol {
                protocol, server_end, responder, ..
            } => {
                let res = proxy.connect_to_named_protocol(protocol.as_str(), server_end);
                responder.send(res)?;
            }
        }
    }

    Ok(())
}

// serve proxies all requests in [stream] to [realm].
//
// # Example Usage
//
// ```
// #[fuchsia::main(logging = true)]
// async fn main() -> Result<(), Error> {
//   let mut fs = ServiceFs::new();
//
//   fs.dir("svc").add_fidl_service(|stream| {
//     fasync::Task::spawn(async move {
//       let realm = build_realm().await.unwrap();
//       realm_proxy::service::serve(realm, stream).await.unwrap();
//     }).detach();
//   });
//
//   fs.take_and_serve_directory_handle()?;
//   fs.collect::<()>().await;
//   Ok(())
// }
// ```
pub async fn serve(realm: RealmInstance, stream: RealmProxy_RequestStream) -> Result<(), Error> {
    let proxy = RealmInstanceProxy(realm);
    serve_with_proxy(proxy, stream).await
}

/// Dispatches incoming connections on `receiver_stream to `request_stream_handler`.
/// `receiver_stream` is a sandbox receiver channel.
///
/// Example:
///
/// async fn handle_echo_request_stream(mut stream: fecho::EchoRequestStream) {
///     while let Ok(Some(_request)) = stream.try_next().await {
///         // ... handle request ...
///     }
/// }
/// ...
/// task_group.spawn(async move {
///     let _ = realm_proxy::service::handle_receiver::<fecho::EchoMarker, _, _>(
///         echo_receiver_stream,
///         handle_echo_request_stream,
///     )
///     .await
///     .map_err(|e| {
///         error!("Failed to serve echo stream: {}", e);
///     });
/// });
pub async fn handle_receiver<T, Fut, F>(
    mut receiver_stream: fsandbox::ReceiverRequestStream,
    request_stream_handler: F,
) -> Result<(), Error>
where
    T: endpoints::ProtocolMarker,
    Fut: Future<Output = ()> + Send,
    F: Fn(T::RequestStream) -> Fut + Send + Sync + Copy + 'static,
{
    let mut receive_tasks = fasync::TaskGroup::new();
    while let Some(request) =
        receiver_stream.try_next().await.context("failed to read request from stream")?
    {
        match request {
            fsandbox::ReceiverRequest::Clone2 { .. } => {
                unimplemented!()
            }
            fsandbox::ReceiverRequest::Receive { capability, control_handle } => {
                let fsandbox::Capability::Handle(handle_cap_client_end) = capability else {
                    control_handle.shutdown_with_epitaph(zx::Status::INVALID_ARGS);
                    return Err(anyhow!("capability is not a HandleCapability"));
                };
                let handle_cap_proxy = handle_cap_client_end.into_proxy().unwrap();
                receive_tasks.spawn(async move {
                    // This assumes the HandleCapability vends only a single handle, so it does
                    // not call GetHandle in a loop, as that would return Unavailable after
                    // the first handle.
                    let result = match handle_cap_proxy.get_handle().await {
                        Ok(result) => result,
                        Err(err) => {
                            error!("failed to call GetHandle: {:?}", err);
                            return;
                        }
                    };
                    let handle = match result {
                        Ok(handle) => handle,
                        Err(err) => {
                            error!("failed to get handle: {:?}", err);
                            return;
                        }
                    };
                    let server_end = endpoints::ServerEnd::<T>::new(fidl::Channel::from(handle));
                    let stream: T::RequestStream = server_end.into_stream().unwrap();
                    request_stream_handler(stream).await;
                });
            }
            fsandbox::ReceiverRequest::_UnknownMethod { .. } => {
                unimplemented!()
            }
        }
    }
    Ok(())
}
