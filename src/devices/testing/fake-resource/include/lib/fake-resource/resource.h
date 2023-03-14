// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_TESTING_FAKE_RESOURCE_INCLUDE_LIB_FAKE_RESOURCE_RESOURCE_H_
#define SRC_DEVICES_TESTING_FAKE_RESOURCE_INCLUDE_LIB_FAKE_RESOURCE_RESOURCE_H_

#include <lib/zx/resource.h>
#include <zircon/syscalls/smc.h>
#include <zircon/types.h>

#include <functional>
#include <utility>
#include <vector>

// Create a fake root resource.
zx_status_t fake_root_resource_create(zx_handle_t* out);
// Fake |zx_smc_call| will use this vector return zx_smc_result values based on
// matching zx_smc_parameter_t structures. The first matching result is used.
// This vector is only used if a handler has been armed by
// fake_smc_set_handler().
zx_status_t fake_smc_set_results(
    const zx::unowned_resource& smc,
    const std::vector<std::pair<zx_smc_parameters_t, zx_smc_result_t>>& results);
// Set a handler to call on the next |zx_smc_call|.
zx_status_t fake_smc_set_handler(
    const zx::unowned_resource& smc,
    std::function<void(const zx_smc_parameters_t*, zx_smc_result_t*)> handler);
// Clears any existing handler and results table.
zx_status_t fake_smc_unset(const zx::unowned_resource& smc);
#endif  // SRC_DEVICES_TESTING_FAKE_RESOURCE_INCLUDE_LIB_FAKE_RESOURCE_RESOURCE_H_
