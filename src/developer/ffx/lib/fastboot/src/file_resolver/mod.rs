// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_trait::async_trait;
use std::io::Write;
use std::path::Path;

pub mod gcs;
pub mod resolvers;

#[async_trait(?Send)]
pub trait FileResolver {
    fn manifest(&self) -> &Path;
    async fn get_file<W: Write>(&mut self, writer: &mut W, file: &str) -> Result<String>;
}
