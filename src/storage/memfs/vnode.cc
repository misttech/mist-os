// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/memfs/vnode.h"

#include "src/storage/lib/vfs/cpp/paged_vnode.h"
#include "src/storage/memfs/dnode.h"

namespace memfs {

std::atomic<uint64_t> Vnode::ino_ctr_ = 0;
std::atomic<uint64_t> Vnode::deleted_ino_ctr_ = 0;

Vnode::Vnode(Memfs& memfs)
    : fs::PagedVnode(memfs), ino_(ino_ctr_.fetch_add(1, std::memory_order_relaxed)) {
  std::timespec ts;
  if (std::timespec_get(&ts, TIME_UTC)) {
    create_time_ = modify_time_ = zx_time_from_timespec(ts);
  }
}

Vnode::~Vnode() { deleted_ino_ctr_.fetch_add(1, std::memory_order_relaxed); }

fs::VnodeAttributesQuery Vnode::SupportedMutableAttributes() const {
  return fs::VnodeAttributesQuery::kModificationTime;
}

zx::result<> Vnode::UpdateAttributes(const fs::VnodeAttributesUpdate& attributes) {
  // TODO(https://fxbug.dev/340626555): Add support for creation time and POSIX mode/uid/gid.
  modify_time_ = attributes.modification_time.value_or(modify_time_);
  return zx::ok();
}

void Vnode::Sync(SyncCallback closure) {
  // Since this filesystem is in-memory, all data is already up-to-date in
  // the underlying storage
  closure(ZX_OK);
}

void Vnode::UpdateModified() const {
  std::timespec ts;
  if (std::timespec_get(&ts, TIME_UTC)) {
    modify_time_ = zx_time_from_timespec(ts);
  } else {
    modify_time_ = 0;
  }
}

}  // namespace memfs
