// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod boot;
pub mod common;
pub mod file_resolver;
pub mod info;
pub mod lock;
pub mod manifest;
pub mod unlock;
pub mod util;

////////////////////////////////////////////////////////////////////////////////
// tests
pub mod test {
    use crate::file_resolver::FileResolver;
    use anyhow::Result;
    use async_trait::async_trait;

    pub struct TestResolver {}

    impl TestResolver {
        pub fn new() -> Self {
            Self {}
        }
    }

    #[async_trait(?Send)]
    impl FileResolver for TestResolver {
        async fn get_file(&mut self, file: &str) -> Result<String> {
            Ok(file.to_owned())
        }
    }
}
