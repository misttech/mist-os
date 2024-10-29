// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_FAKE_BTI_CPP_FAKE_BTI_H_
#define LIB_DRIVER_FAKE_BTI_CPP_FAKE_BTI_H_

#include <lib/stdcompat/span.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <limits.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

namespace fake_bti {

// All physical addresses returned by zx_bti_pin with a fake BTI will be set to this value.
// PAGE_SIZE is chosen so that so superficial validity checks like "is the address correctly
// aligned" and "is the address non-zero" in the code under test will pass.
#define FAKE_BTI_PHYS_ADDR PAGE_SIZE

zx::result<zx::bti> CreateFakeBti();

// Like fake_bti_create, except zx_bti_pin will return the fake physical addresses in |paddrs|, or
// ZX_ERR_OUT_OF_RANGE if not enough address were specified. If |paddrs| is NULL or |paddr_count| is
// zero, each address is set to FAKE_BTI_PHYS_ADDR, and no range check is performed. |paddrs| must
// remain valid until the last call to zx_bti_pin is made.
zx::result<zx::bti> CreateFakeBtiWithPaddrs(cpp20::span<const zx_paddr_t> paddrs);

struct FakeBtiPinnedVmoInfo {
  zx_handle_t vmo;
  uint64_t size;
  uint64_t offset;
};

// Fake BTI stores all pinned VMOs for testing purposes. Tests can call this
// method to get duplicates of all pinned VMO handles, as well as the pinned
// pages' size and offset for each VMO.
//
// |out_vmo_info| points to a buffer containing |out_num_vmos| vmo info
// elements. The method writes no more than |out_num_vmos| elements to the
// buffer, and will write the actual count of vmo info elements to |actual_num_vmos|
// if the argument is not null.
//
// It's the caller's repsonsibility to close all the returned VMO handles.
zx::result<std::vector<FakeBtiPinnedVmoInfo>> GetPinnedVmo(zx_handle_t bti);

// Fake BTI stores all the fake physical addresses that is returned by |zx_bti_pin|.
// Tests can call this method to get the fake physical addresses corresponding to |vmo_info|.
// |out_paddrs| points to a buffer containing |out_num_paddrs| physical address elements.
// The method writes no more than |out_num_paddrs| elements to the buffer, and will write the actual
// number of physical addresses to |actual_num_paddrs| if the argument is not null.
zx::result<std::vector<zx_paddr_t>> GetVmoPhysAddress(zx_handle_t bti,
                                                      FakeBtiPinnedVmoInfo vmo_info);

}  // namespace fake_bti

#endif  // LIB_DRIVER_FAKE_BTI_CPP_FAKE_BTI_H_
