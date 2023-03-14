// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::lock::RwLock;
use std::sync::Arc;

const DEFAULT_HOST_NAME: &[u8] = b"local";
const DEFAULT_DOMAIN_NAME: &[u8] = b"local";

pub type UtsNamespaceHandle = Arc<RwLock<UtsNamespace>>;

// Unix Time-sharing Namespace (UTS) information.
// Stores the hostname and domainname for a specific process.
//
// See https://man7.org/linux/man-pages/man7/uts_namespaces.7.html
#[derive(Clone)]
pub struct UtsNamespace {
    pub hostname: Vec<u8>,
    pub domainname: Vec<u8>,
}

impl Default for UtsNamespace {
    fn default() -> Self {
        Self { hostname: DEFAULT_HOST_NAME.to_owned(), domainname: DEFAULT_DOMAIN_NAME.to_owned() }
    }
}
