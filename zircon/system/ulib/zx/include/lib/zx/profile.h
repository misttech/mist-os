// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZX_PROFILE_H_
#define LIB_ZX_PROFILE_H_

#include <lib/zx/handle.h>
#include <lib/zx/job.h>
#include <lib/zx/object.h>
#include <lib/zx/resource.h>
#include <zircon/availability.h>

namespace zx {

class profile final : public object<profile> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_PROFILE;

  constexpr profile() = default;

  explicit profile(zx_handle_t value) : object(value) {}

  explicit profile(handle&& h) : object(h.release()) {}

  profile(profile&& other) : object(other.release()) {}

  profile& operator=(profile&& other) {
    reset(other.release());
    return *this;
  }

  static zx_status_t create(const resource& profile_rsrc, uint32_t options,
                            const zx_profile_info_t* info, profile* result) ZX_AVAILABLE_SINCE(7);
} ZX_AVAILABLE_SINCE(7);

using unowned_profile = unowned<profile> ZX_AVAILABLE_SINCE(7);

}  // namespace zx

#endif  // LIB_ZX_PROFILE_H_
