// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};

/// The type if intl configuration to be used.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
pub enum Type {
    /// Intl services are not used at all. Some very basic configurations such
    /// as bringup don't need internationalization support.
    #[default]
    None,
    /// The default intl services bundle is used.
    Default,
    /// The special small footprint intl services bundle is used.
    ///
    /// Some small footprint deployments need to be extra conscious of the
    /// storage space used.
    Small,
}

/// Platform configuration options for the input area.
#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IntlConfig {
    /// The intl configuration type in use.
    pub config_type: Type,
}
