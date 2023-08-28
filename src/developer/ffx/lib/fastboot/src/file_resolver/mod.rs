// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use anyhow::Result;
use std::io::Write;
use std::path::Path;

pub mod resolvers;
pub mod gcs;

#[async_trait(?Send)]
pub trait FileResolver {
    fn manifest(&self) -> &Path;
    async fn get_file<W: Write>(&mut self, writer: &mut W, file: &str) -> Result<String>;
}
