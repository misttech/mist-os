// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls/iob.h>
#include <zircon/syscalls/policy.h>
#include <zircon/types.h>

#include <cstdint>

#include <fbl/alloc_checker.h>
#include <ktl/move.h>
#include <object/handle.h>
#include <object/io_buffer_dispatcher.h>
#include <object/process_dispatcher.h>

// zx_status_t zx_iob_create
zx_status_t sys_iob_create(uint64_t options, user_in_ptr<const void> regions, uint64_t num_regions,
                           user_out_handle* ep0_out, user_out_handle* ep1_out) {
  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }
  auto up = ProcessDispatcher::GetCurrent();
  zx_status_t res = up->EnforceBasicPolicy(ZX_POL_NEW_IOB);
  if (res != ZX_OK) {
    return res;
  }

  if (num_regions > ZX_IOB_MAX_REGIONS) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AllocChecker ac;
  IoBufferDispatcher::RegionArray copied_regions{&ac, num_regions};

  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  res = regions.reinterpret<const zx_iob_region_t>().copy_array_from_user(copied_regions.get(),
                                                                          num_regions);
  if (res != ZX_OK) {
    return res;
  }

  KernelHandle<IoBufferDispatcher> handle0, handle1;
  zx_rights_t rights;
  zx_status_t result = IoBufferDispatcher::Create(options, copied_regions, up->attribution_object(),
                                                  &handle0, &handle1, &rights);
  if (result != ZX_OK) {
    return result;
  }

  result = ep0_out->make(ktl::move(handle0), rights);
  if (result == ZX_OK) {
    result = ep1_out->make(ktl::move(handle1), rights);
  }
  return result;
}
