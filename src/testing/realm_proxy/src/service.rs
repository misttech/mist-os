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

// A [RealmProxy] that mediates access to the services in a [RealmInstance].
pub struct RealmInstanceProxy {
    instance: RealmInstance,
}

impl RealmInstanceProxy {
    // from_instance creates a new RealmInstanceProxy.
    //
    // Protocol connections are forward to the given [instance].
    //
    // # Example usage
    //
    // ```
    // let builder = RealmBuilder::new().await?;
    // let realm = build_test_realm(&mut builder);
    // RealmInstanceProxy::from_instance(realm);
    // ```
    pub fn from_instance(instance: RealmInstance) -> Self {
        Self { instance: instance }
    }
}

impl RealmProxy for RealmInstanceProxy {
    fn connect_to_named_protocol(
        &mut self,
        protocol: &str,
        server_end: zx::Channel,
    ) -> Result<(), OperationError> {
        let res = self
            .instance
            .root
            .connect_request_to_named_protocol_at_exposed_dir(protocol, server_end);

        if let Some(err) = res.err() {
            error!("{:?}", err);
            return Err(OperationError::Failed);
        }

        Ok(())
    }
}

// handle_request_stream uses [realm_proxy] to handle all requests from [stream].
//
// This function is useful when implementing a custom test harness that serves
// other protocols in addition to the RealmProxy protocol.
//
// # Example Usage
//
// ```
// enum IncomingService {
//     RealmProxy(RealmProxy_RequestStream),
// }
//
// #[fuchsia::main(logging_tags = ["proxy"])]
// async fn main() -> Result<(), Error> {
//     let mut fs = ServiceFs::new();
//     fs.dir("svc").add_fidl_service(IncomingService::RealmProxy);
//     fs.take_and_serve_directory_handle()?;
//     fs.for_each_concurrent(None, |IncomingService::RealmProxy(stream)| async {
//         let realm_proxy = RealmInstanceProxy::from_builder(setup_realm);
//         handle_request_stream(realm_proxy, stream).await.unwrap();
//     })
//     .await;
//     Ok(())
// }
// ```
pub async fn handle_request_stream<P: RealmProxy>(
    mut realm_proxy: P,
    mut stream: RealmProxy_RequestStream,
) -> Result<(), Error> {
    while let Some(Ok(request)) = stream.next().await {
        info!("received {:?}", request);
        match request {
            RealmProxy_Request::ConnectToNamedProtocol {
                protocol, server_end, responder, ..
            } => {
                let mut res = realm_proxy.connect_to_named_protocol(protocol.as_str(), server_end);
                responder.send(&mut res)?;
            }
        }
    }

    Ok(())
}
