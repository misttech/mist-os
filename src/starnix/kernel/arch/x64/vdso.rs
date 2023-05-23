// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::arch::x86_64::_rdtsc;
use fuchsia_zircon as zx;
use process_builder::elf_parse;
use zerocopy::AsBytes;

use crate::types::{errno, from_status_like_fdio, uapi, Errno};

pub const HAS_VDSO: bool = true;

/// Overwrite the constants in the vDSO with the values obtained from Zircon.
pub fn set_vdso_constants(vdso_vmo: &zx::Vmo) -> Result<(), Errno> {
    let headers = elf_parse::Elf64Headers::from_vmo(vdso_vmo).map_err(|_| errno!(EINVAL))?;
    // The entry point in the vDSO stores the offset at which the constants are located. See vdso/vdso.ld for details.
    let constants_offset = headers.file_header().entry;

    let clock = zx::Clock::create(zx::ClockOpts::MONOTONIC | zx::ClockOpts::AUTO_START, None)
        .expect("failed to create clock");
    let details = clock.get_details().expect("Failed to get clock details");
    let zx_ticks = zx::ticks_get();
    let raw_ticks;
    unsafe {
        raw_ticks = _rdtsc();
    }
    // Compute the offset as a difference between the value returned by zx_get_ticks() and the value
    // in the register. This is not precise as the reads are not synchronised.
    // TODO(fxb/124586): Support updates to this value.
    let ticks_offset = zx_ticks - (raw_ticks as i64);

    let vdso_consts: uapi::vdso_constants = uapi::vdso_constants {
        raw_ticks_to_ticks_offset: ticks_offset,
        ticks_to_mono_numerator: details.ticks_to_synthetic.rate.synthetic_ticks,
        ticks_to_mono_denominator: details.ticks_to_synthetic.rate.reference_ticks,
    };
    vdso_vmo
        .write(vdso_consts.as_bytes(), constants_offset as u64)
        .map_err(|status| from_status_like_fdio!(status))?;
    Ok(())
}

pub fn get_sigreturn_offset(_vdso_vmo: &zx::Vmo) -> Result<Option<u64>, Errno> {
    Ok(None)
}
