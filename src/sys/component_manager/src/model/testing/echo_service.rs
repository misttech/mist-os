// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use cm_types::Name;
use fidl_fidl_examples_routing_echo::{EchoRequest, EchoRequestStream};
use futures::TryStreamExt;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref ECHO_CAPABILITY: Name = "builtin.Echo".parse().unwrap();
}

pub struct EchoProtocol;

impl EchoProtocol {
    pub async fn serve(mut stream: EchoRequestStream) -> Result<(), anyhow::Error> {
        while let Some(EchoRequest::EchoString { value, responder }) =
            stream.try_next().await.unwrap()
        {
            responder.send(value.as_ref().map(|s| &**s)).unwrap();
        }
        Ok(())
    }
}
