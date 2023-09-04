// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/memfs/vnode_file.h"

#include <lib/zx/stream.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls-next.h>

#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/memfs/memfs.h"

namespace memfs {

VnodeFile::VnodeFile(Memfs& memfs) : Vnode(memfs), memfs_(memfs) {}

VnodeFile::~VnodeFile() {
  fbl::RefPtr<fs::Vnode> file = FreePagedVmo();
  // FreePagedVmo is being called from the destructor so PagedVnode shouldn't have been holding onto
  // reference at this point.
  ZX_DEBUG_ASSERT(file == nullptr);
}

fs::VnodeProtocolSet VnodeFile::GetProtocols() const { return fs::VnodeProtocol::kFile; }

zx_status_t VnodeFile::CreateStream(uint32_t stream_options, zx::stream* out_stream) {
  std::lock_guard lock(mutex_);
  if (zx_status_t status = CreateBackingStoreIfNeeded(); status != ZX_OK) {
    return status;
  }
  return zx::stream::create(stream_options, paged_vmo(), 0u, out_stream);
}

void VnodeFile::DidModifyStream() { UpdateModified(); }

zx_status_t VnodeFile::GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo* out_vmo) {
  std::lock_guard lock(mutex_);
  if (zx_status_t status = CreateBackingStoreIfNeeded(); status != ZX_OK) {
    return status;
  }
  size_t content_size = GetContentSize();
  // Let clients map and set the names of their VMOs.
  zx_rights_t rights = ZX_RIGHTS_BASIC | ZX_RIGHT_MAP | ZX_RIGHT_GET_PROPERTY;
  rights |= (flags & fuchsia_io::wire::VmoFlags::kRead) ? ZX_RIGHT_READ : 0;
  rights |= (flags & fuchsia_io::wire::VmoFlags::kWrite) ? ZX_RIGHT_WRITE : 0;
  rights |= (flags & fuchsia_io::wire::VmoFlags::kExecute) ? ZX_RIGHT_EXECUTE : 0;
  zx::vmo result;
  if (flags & fuchsia_io::wire::VmoFlags::kPrivateClone) {
    rights |= ZX_RIGHT_SET_PROPERTY;  // Only allow object_set_property on private VMO.
    if (zx_status_t status = paged_vmo().create_child(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE, 0,
                                                      content_size, &result);
        status != ZX_OK) {
      return status;
    }

    if (zx_status_t status = result.replace(rights, &result); status != ZX_OK) {
      return status;
    }
  } else {
    if (zx_status_t status = paged_vmo().duplicate(rights, &result); status != ZX_OK) {
      return status;
    }
  }

  *out_vmo = std::move(result);
  return ZX_OK;
}

zx_status_t VnodeFile::GetAttributes(fs::VnodeAttributes* attr) {
  uint64_t content_size = 0;
  {
    fs::SharedLock lock(mutex_);
    if (paged_vmo().is_valid()) {
      content_size = GetContentSize();
      UpdateModifiedIfVmoChanged();
    }
  }

  *attr = fs::VnodeAttributes();
  attr->inode = ino_;
  attr->mode = V_TYPE_FILE | V_IRUSR | V_IWUSR | V_IRGRP | V_IROTH;
  attr->content_size = content_size;
  attr->storage_size = fbl::round_up(attr->content_size, GetPageSize());
  attr->link_count = link_count_;
  attr->creation_time = create_time_;
  attr->modification_time = modify_time_;
  return ZX_OK;
}

zx_status_t VnodeFile::GetNodeInfoForProtocol([[maybe_unused]] fs::VnodeProtocol protocol,
                                              [[maybe_unused]] fs::Rights rights,
                                              fs::VnodeRepresentation* info) {
  *info = fs::VnodeRepresentation::File();
  return ZX_OK;
}

zx_status_t VnodeFile::Truncate(size_t length) {
  std::lock_guard lock(mutex_);
  if (zx_status_t status = CreateBackingStoreIfNeeded(); status != ZX_OK) {
    return status;
  }
  if (zx_status_t status = paged_vmo().set_size(length); status != ZX_OK) {
    return status;
  }

  UpdateModified();
  return ZX_OK;
}

zx_status_t VnodeFile::CreateBackingStoreIfNeeded() {
  // TODO(fxbug.dev/116484): Use a fixed sized VMO.
  return EnsureCreatePagedVmo(0, ZX_VMO_RESIZABLE).status_value();
}

uint64_t VnodeFile::GetContentSize() const {
  ZX_DEBUG_ASSERT(paged_vmo().is_valid());
  uint64_t content_size = 0u;
  zx_status_t status = paged_vmo().get_prop_content_size(&content_size);
  ZX_DEBUG_ASSERT(status == ZX_OK);
  return content_size;
}

bool VnodeFile::SupportsClientSideStreams() {
#if defined(MEMFS_ENABLE_CLIENT_SIDE_STREAMS)
  return true;
#else
  return false;
#endif
}

zx_status_t VnodeFile::CloseNode() {
  fs::SharedLock lock(mutex_);
  UpdateModifiedIfVmoChanged();
  return ZX_OK;
}

void VnodeFile::Sync(SyncCallback closure) {
  closure(ZX_OK);
  fs::SharedLock lock(mutex_);
  UpdateModifiedIfVmoChanged();
}

void VnodeFile::UpdateModifiedIfVmoChanged() {
#if defined(MEMFS_ENABLE_CLIENT_SIDE_STREAMS)
  if (!paged_vmo().is_valid()) {
    return;
  }

  zx_pager_vmo_stats vmo_stats;
  zx_status_t status =
      zx_pager_query_vmo_stats(memfs_.pager_for_next_vdso_syscalls().get(), paged_vmo().get(),
                               ZX_PAGER_RESET_VMO_STATS, &vmo_stats, sizeof(vmo_stats));
  ZX_ASSERT(status == ZX_OK);
  if (vmo_stats.modified == ZX_PAGER_VMO_STATS_MODIFIED) {
    UpdateModified();
  }
#endif
}

}  // namespace memfs
