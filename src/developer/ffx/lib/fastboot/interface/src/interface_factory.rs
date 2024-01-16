// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncWrite};

///////////////////////////////////////////////////////////////////////////////
// Factory
//

#[async_trait(?Send)]
pub trait InterfaceFactoryBase<T: AsyncRead + AsyncWrite + Unpin> {
    async fn open(&mut self) -> Result<T>;
    async fn close(&self);
    async fn rediscover(&mut self) -> Result<()>;
}

#[async_trait(?Send)]
pub trait InterfaceFactory<T: AsyncRead + AsyncWrite + Unpin>:
    std::fmt::Debug + InterfaceFactoryBase<T>
{
}
