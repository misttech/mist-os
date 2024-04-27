// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVELOPER_VSOCK_SSHD_HOST_DATA_DIR_H_
#define SRC_DEVELOPER_VSOCK_SSHD_HOST_DATA_DIR_H_

#include <lib/async-loop/cpp/loop.h>

#include "src/storage/memfs/memfs.h"
#include "src/storage/memfs/vnode_dir.h"

zx::result<> BuildDataDir(async::Loop& loop, memfs::Memfs* memfs,
                          fbl::RefPtr<memfs::VnodeDir> root);

#endif  // SRC_DEVELOPER_VSOCK_SSHD_HOST_DATA_DIR_H_
