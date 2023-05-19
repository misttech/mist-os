// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Error, Result},
    fidl_fuchsia_testing_harness::{OperationError, RealmProxy_Request, RealmProxy_RequestStream},
    fuchsia_component_test::RealmInstance,
    fuchsia_zircon as zx,
    futures::StreamExt,
    tracing::{error, info},
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
        info!("received {:?}", request);
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
