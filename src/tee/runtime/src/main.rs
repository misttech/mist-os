// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod ta_loader;

use std::{ffi::CString, fs::File, io::Read};

use anyhow::Error;

fn read_ta_name() -> Result<CString, Error> {
    let mut f = File::open("/pkg/data/ta_name")
        .map_err(|e| anyhow::anyhow!("Could not open ta_name file: {e}"))?;
    let mut s = String::from("lib");
    let _ = f
        .read_to_string(&mut s)
        .map_err(|e| anyhow::anyhow!("Could not read ta_name file: {e}"))?;
    s.push_str(".so");
    CString::new(s).map_err(|_| anyhow::anyhow!("nul found in name string"))
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let name = read_ta_name()?;
    let ta = ta_loader::load_ta(&name)?;

    // TODO: Expose fuchsia.tee.Application protocol and wire up to |ta|.
    let _ = ta;

    // Generate link-time references to the symbols that we want to expose to
    // the TA so that the definitions will be retained by the linker.
    let _ = std::hint::black_box(tee_internal_impl::binding_stubs::exposed_c_entry_points());

    Ok(())
}
