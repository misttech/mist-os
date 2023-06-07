// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "taskfinder.h"

zx::result<std::vector<std::pair<zx_koid_t, zx::handle>>> TaskFinder::FindHandles() {
  auto status = WalkRootJobTree();
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(found_handles_));
}
// Each of these methods visits the corresponding task type. If any On*()
// method returns a value other than ZX_OK, the enumeration stops. See
// |task_callback_t| for a description of parameters.
zx_status_t TaskFinder::OnJob(int depth, zx_handle_t job, zx_koid_t koid, zx_koid_t parent_koid) {
  if (jobs_.find(koid) != jobs_.end()) {
    zx::handle dup;
    zx_status_t result = zx::unowned_handle(job)->duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
    if (result != ZX_OK) {
      return result;
    }
    found_handles_.emplace_back(koid, std::move(dup));
  }
  return ZX_OK;
}

zx_status_t TaskFinder::OnProcess(int depth, zx_handle_t process, zx_koid_t koid,
                                  zx_koid_t parent_koid) {
  if (processes_.find(koid) != processes_.end()) {
    zx::handle dup;
    zx_status_t result = zx::unowned_handle(process)->duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
    if (result != ZX_OK) {
      return result;
    }
    found_handles_.emplace_back(koid, std::move(dup));
  }
  return ZX_OK;
}

zx_status_t TaskFinder::OnThread(int depth, zx_handle_t process, zx_koid_t koid,
                                 zx_koid_t parent_koid) {
  if (threads_.find(koid) != threads_.end()) {
    zx::handle dup;
    zx_status_t result = zx::unowned_handle(process)->duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
    if (result != ZX_OK) {
      return result;
    }
    found_handles_.emplace_back(koid, std::move(dup));
  }
  return ZX_OK;
}
