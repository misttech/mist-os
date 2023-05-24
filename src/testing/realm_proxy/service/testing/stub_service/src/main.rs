// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_test::{EventPairHolderRequest, EventPairHolderRequestStream};

use {
    anyhow::{Error, Result},
    fidl_fuchsia_test::{
        ExposedRequest, ExposedRequestStream, NotExposedRequest, NotExposedRequestStream,
    },
    fuchsia_component::server::ServiceFs,
    futures::{StreamExt, TryFutureExt},
    tracing::{error, info},
};

enum IncomingService {
    Exposed(ExposedRequestStream),
    EventPairHolder(EventPairHolderRequestStream),
    NotExposed(NotExposedRequestStream),
}

#[fuchsia::main(logging = true)]
async fn main() -> Result<(), Error> {
    info!("starting");

    let mut fs = ServiceFs::new();
    fs.dir("svc")
        .add_fidl_service(IncomingService::Exposed)
        .add_fidl_service(IncomingService::EventPairHolder)
        .add_fidl_service(IncomingService::NotExposed);

    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |service| async {
        let mut server = Server::new();
        server.serve(service).unwrap_or_else(move |e| error!("run_server error: {:?}", e)).await;
        info!("received event pairs {:?}", server.event_pairs);
    })
    .await;

    Ok(())
}

struct Server {
    event_pairs: Vec<fidl::EventPair>,
}

impl Server {
    fn new() -> Self {
        Self { event_pairs: vec![] }
    }

    async fn serve(&mut self, service: IncomingService) -> Result<(), Error> {
        match service {
            IncomingService::Exposed(stream) => self.serve_exposed(stream).await?,
            IncomingService::EventPairHolder(stream) => self.serve_event_holder(stream).await?,
            IncomingService::NotExposed(stream) => self.serve_not_exposed(stream).await?,
        };

        Ok(())
    }

    // Implements fuchsia.test.Exposed.
    // It just sends an empty response to prevent an error when the test invokes
    // Call() to confirm that this protocol is proxied.
    async fn serve_exposed(&self, mut stream: ExposedRequestStream) -> Result<(), Error> {
        while let Some(Ok(request)) = stream.next().await {
            info!("serve_exposed: {:?}", request);
            match request {
                ExposedRequest::Call { responder } => {
                    responder.send()?;
                }
            };
        }
        Ok(())
    }

    // Implements fuchsia.test.NotExposed.
    // It just sends an empty response and should never receive a request since
    // we're not proxying this protocol in the tests.
    async fn serve_not_exposed(&self, mut stream: NotExposedRequestStream) -> Result<(), Error> {
        while let Some(Ok(request)) = stream.next().await {
            info!("serve_not_exposed: {:?}", request);
            match request {
                NotExposedRequest::Call { responder } => {
                    responder.send()?;
                }
            };
        }

        Ok(())
    }

    // Implements fuchsia.test.EventPairHolder.
    // It just holds onto every event pair sent by the caller.
    async fn serve_event_holder(
        &mut self,
        mut stream: EventPairHolderRequestStream,
    ) -> Result<(), Error> {
        while let Some(Ok(request)) = stream.next().await {
            info!("serve_event_holder: {:?}", request);
            match request {
                EventPairHolderRequest::Hold { event_pair, .. } => {
                    self.event_pairs.push(event_pair)
                }
            };
        }

        Ok(())
    }
}
