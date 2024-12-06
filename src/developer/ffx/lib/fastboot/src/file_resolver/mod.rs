// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;

pub mod resolvers;

#[async_trait(?Send)]
pub trait FileResolver {
    async fn get_file(&mut self, file: &str) -> Result<String>;
}
