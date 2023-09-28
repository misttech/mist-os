// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use once_cell::sync::OnceCell;

mod not_fuchsia;

pub use not_fuchsia::*;

static HOIST: OnceCell<Hoist> = OnceCell::new();

pub fn hoist() -> &'static Hoist {
    // otherwise, don't return it until something sets it up.
    HOIST.get().expect("Tried to get overnet hoist before it was initialized")
}

/// On non-fuchsia OS', call this at the start of the program to enable the global hoist.
pub fn init_hoist() -> Result<&'static Hoist, Error> {
    let hoist = Hoist::new(None)?;
    init_hoist_with(hoist)
}

/// On non-fuchsia OS', call this at the start of the program to make the provided hoist global
pub fn init_hoist_with(hoist: Hoist) -> Result<&'static Hoist, Error> {
    HOIST
        .set(hoist.clone())
        .map_err(|_| anyhow::anyhow!("Tried to set global hoist more than once"))?;
    HOIST.get().context("Failed to retrieve the hoist we created back from the cell we put it in")
}
