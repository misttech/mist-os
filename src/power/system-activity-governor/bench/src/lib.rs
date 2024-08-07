// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Integration test that can help to check the connection to the server
//! and the fidl call.

mod work;

use anyhow::Result;

#[cfg(test)]
mod tests {
    use super::*;
    #[fuchsia::test]
    fn test() -> Result<()> {
        let sag_arc = work::obtain_sag_proxy();

        work::execute(&sag_arc);
        Ok(())
    }
}
