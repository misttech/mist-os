// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_SCHEDULER_ROLE_H_
#define LIB_SCHEDULER_ROLE_H_

#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>

#include <string_view>

//
// # Fuchsia Scheduler C++ API
//
// Utility functions for calling the fuchsia.scheduler.ProfileProvider role API.
//
// These functions automatically handle service connection and handle duplication to minimize
// client boilerplate.
//

namespace fuchsia_scheduler {

zx_status_t SetRoleForVmar(zx::unowned_vmar vmar, std::string_view role);
zx_status_t SetRoleForRootVmar(std::string_view role);
zx_status_t SetRoleForThread(zx::unowned_thread thread, std::string_view role);
zx_status_t SetRoleForThisThread(std::string_view role);

}  // namespace fuchsia_scheduler

#endif  // LIB_SCHEDULER_ROLE_H_
