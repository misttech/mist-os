// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::arch::x86_64::_rdtsc;
use fuchsia_zircon as zx;
use process_builder::elf_parse;
use std::sync::Arc;
use zerocopy::AsBytes;

use crate::{
    mm::PAGE_SIZE,
    types::{errno, from_status_like_fdio, uapi, Errno},
};

pub const HAS_VDSO: bool = true;

pub fn calculate_ticks_offset() -> i64 {
    let mut ticks_offset: i64 = i64::MIN;
    let mut min_read_diff: i64 = i64::MAX;
    // Assuming zx_get_ticks() is based on the TSC, estimate the offset between the raw value
    // from `rdtsc` and the ticks as returned by zx_get_ticks(). Since the reads will not be
    // made at the same time, the result will not be presice, so do the estimation several
    // times and choose the measurement with the smallest error bars.
    // TODO(fxb/127692): Obtain this value from Zircon
    for _i in 0..5 {
        let raw_ticks;
        let zx_read_first = zx::ticks_get();
        unsafe {
            raw_ticks = _rdtsc();
        }
        let zx_read_second = zx::ticks_get();
        let read_diff = zx_read_second - zx_read_first;
        if read_diff < min_read_diff {
            min_read_diff = read_diff;
            let midpoint = zx_read_first + read_diff / 2;
            ticks_offset = midpoint - raw_ticks as i64;
        }
    }
    ticks_offset
}

pub fn set_vdso_constants(vdso_vmo: &zx::Vmo) -> Result<(), Errno> {
    let headers = elf_parse::Elf64Headers::from_vmo(vdso_vmo).map_err(|_| errno!(EINVAL))?;
    let constants_offset = headers.file_header().entry as u64;
    let vdso_consts: uapi::vdso_constants =
        uapi::vdso_constants { vvar_offset: constants_offset + *PAGE_SIZE as u64 };
    vdso_vmo
        .write(vdso_consts.as_bytes(), constants_offset as u64)
        .map_err(|status| from_status_like_fdio!(status))?;
    Ok(())
}

pub fn set_vvar_data(vvar_vmo: &Arc<zx::Vmo>) -> Result<(), Errno> {
    let clock = zx::Clock::create(zx::ClockOpts::MONOTONIC | zx::ClockOpts::AUTO_START, None)
        .expect("failed to create clock");
    let details = clock.get_details().expect("Failed to get clock details");
    let ticks_offset = calculate_ticks_offset();
    let vvar_data: uapi::vvar_data = uapi::vvar_data {
        raw_ticks_to_ticks_offset: ticks_offset,
        ticks_to_mono_numerator: details.ticks_to_synthetic.rate.synthetic_ticks,
        ticks_to_mono_denominator: details.ticks_to_synthetic.rate.reference_ticks,
    };
    vvar_vmo
        .write(vvar_data.as_bytes(), 0 as u64)
        .map_err(|status| from_status_like_fdio!(status))?;
    Ok(())
}

pub fn get_sigreturn_offset(_vdso_vmo: &zx::Vmo) -> Result<Option<u64>, Errno> {
    Ok(None)
}
