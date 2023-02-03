// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::format_err;

pub enum FlatlandExampleMethod {
    Start,
    Stop,
}

impl std::str::FromStr for FlatlandExampleMethod {
    type Err = anyhow::Error;

    fn from_str(method: &str) -> Result<Self, Self::Err> {
        match method {
            "Start" => Ok(Self::Start),
            "Stop" => Ok(Self::Stop),
            _ => return Err(format_err!("Invalid FlatlandExample Facade SL4F method: {}", method)),
        }
    }
}
