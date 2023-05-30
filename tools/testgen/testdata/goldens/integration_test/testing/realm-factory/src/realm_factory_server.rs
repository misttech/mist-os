// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::realm_factory_impl::*;

use {
    anyhow::{Error, Result},
    fidl_test_examplecomponent as ftest,
    fidl_server::*,
    fuchsia_async as fasync,
    futures::lock::Mutex,
    std::sync::Arc,
};

pub(crate) struct RealmFactoryServer {
    imp: Arc<Mutex<RealmFactoryImpl>>,
}
impl RealmFactoryServer {
    pub fn new(imp: RealmFactoryImpl) -> Self {
        Self { imp: Arc::new(Mutex::new(imp)) }
    }
}

#[async_trait::async_trait]
impl AsyncRequestHandler<ftest::RealmFactoryMarker> for RealmFactoryServer {
    async fn handle_request(&self, request: ftest::RealmFactoryRequest) -> Result<(), Error> {
        match request {
            ftest::RealmFactoryRequest::SetRealmOptions { options, responder } => {
                let mut imp = self.imp.lock().await;
                imp.set_realm_options(options)?;
                responder.send(Ok(()))?;
                Ok(())
            }

            ftest::RealmFactoryRequest::CreateRealm { realm_server, responder } => {
                let mut imp = self.imp.lock().await;
                let realm = imp.create_realm().await?;
                fasync::Task::spawn(async move {
                    let request_stream = realm_server.into_stream().unwrap();
                    realm_proxy::service::serve(realm, request_stream).await.unwrap();
                })
                .detach();
                responder.send(Ok(()))?;
                Ok(())
            }
        }
    }
}