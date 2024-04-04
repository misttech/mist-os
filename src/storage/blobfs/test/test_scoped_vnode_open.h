// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_BLOBFS_TEST_TEST_SCOPED_VNODE_OPEN_H_
#define SRC_STORAGE_BLOBFS_TEST_TEST_SCOPED_VNODE_OPEN_H_

#include <zircon/assert.h>

#include "src/storage/lib/vfs/cpp/vnode.h"

namespace blobfs {

// Scoped wrapper for Vnodes that asserts open/closing nodes to succeed. This avoids test cases
// proceeding in an invalid state if these operations fail, and ensures the node's open count is
// correctly managed in tests.
class TestScopedVnodeOpen {
 public:
  explicit TestScopedVnodeOpen(const fbl::RefPtr<fs::Vnode>& node) : vnode_(node) {
    zx_status_t status = vnode_->Open(nullptr);
    ZX_ASSERT_MSG(status == ZX_OK, "Failed to open node: %s", zx_status_get_string(status));
  }

  ~TestScopedVnodeOpen() {
    zx_status_t status = vnode_->Close();
    ZX_ASSERT_MSG(status == ZX_OK, "Failed to close node: %s", zx_status_get_string(status));
  }

 private:
  fbl::RefPtr<fs::Vnode> vnode_ = nullptr;
};

}  // namespace blobfs

#endif  // SRC_STORAGE_BLOBFS_TEST_TEST_SCOPED_VNODE_OPEN_H_
