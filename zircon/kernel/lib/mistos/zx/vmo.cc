// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/vmo.h"

#include <string.h>
#include <zircon/errors.h>

#include <fbl/ref_ptr.h>

#include "zx_priv.h"

#define LOCAL_TRACE ZX_GLOBAL_TRACE(0)

static_assert(ZX_CACHE_POLICY_CACHED == ARCH_MMU_FLAG_CACHED,
              "Cache policy constant mismatch - CACHED");
static_assert(ZX_CACHE_POLICY_UNCACHED == ARCH_MMU_FLAG_UNCACHED,
              "Cache policy constant mismatch - UNCACHED");
static_assert(ZX_CACHE_POLICY_UNCACHED_DEVICE == ARCH_MMU_FLAG_UNCACHED_DEVICE,
              "Cache policy constant mismatch - UNCACHED_DEVICE");
static_assert(ZX_CACHE_POLICY_WRITE_COMBINING == ARCH_MMU_FLAG_WRITE_COMBINING,
              "Cache policy constant mismatch - WRITE_COMBINING");
static_assert(ZX_CACHE_POLICY_MASK == ARCH_MMU_FLAG_CACHE_MASK,
              "Cache policy constant mismatch - CACHE_MASK");

namespace zx {

namespace {

struct CreateStats {
  uint32_t flags;
  size_t size;
};

zx::result<CreateStats> parse_create_syscall_flags(uint32_t flags, size_t size) {
  CreateStats res = {0, size};

  if (flags & ZX_VMO_RESIZABLE) {
    if (flags & ZX_VMO_UNBOUNDED) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    res.flags |= VmObjectPaged::kResizable;
    flags &= ~ZX_VMO_RESIZABLE;
  }
  if (flags & ZX_VMO_DISCARDABLE) {
    res.flags |= VmObjectPaged::kDiscardable;
    flags &= ~ZX_VMO_DISCARDABLE;
  }
  if (flags & ZX_VMO_UNBOUNDED) {
    if (size != 0) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
    flags &= ~ZX_VMO_UNBOUNDED;
    res.size = VmObjectPaged::max_size();
  }

  if (flags) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(res);
}

}  // namespace

VmoStorage::VmoStorage(fbl::RefPtr<VmObject> vmo, fbl::RefPtr<ContentSizeManager> content_size_mgr,
                       InitialMutability initial_mutability)
    : koid_(KernelObjectId::Generate()),
      vmo_(std::move(vmo)),
      content_size_mgr_(std::move(content_size_mgr)),
      /*pager_koid_(pager_koid),*/
      initial_mutability_(initial_mutability) {
  vmo_->set_user_id(koid_);

  rights_ |= (vmo_->is_resizable() ? ZX_RIGHT_RESIZE : 0);
}

zx_status_t vmo::create(uint64_t size, uint32_t options, vmo* out) {
  LTRACEF("size %#" PRIx64 "\n", size);

  zx::result<CreateStats> parse_result = parse_create_syscall_flags(options, size);
  if (parse_result.is_error()) {
    return parse_result.error_value();
  }
  CreateStats stats = parse_result.value();

  // create a vm object
  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t res = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY /*| PMM_ALLOC_FLAG_CAN_WAIT*/,
                                          stats.flags, stats.size, &vmo);
  if (res != ZX_OK)
    return res;

  fbl::RefPtr<ContentSizeManager> content_size_manager;
  res = ContentSizeManager::Create(size, &content_size_manager);
  if (res != ZX_OK) {
    return res;
  }

  fbl::AllocChecker ac;
  auto storage =
      fbl::MakeRefCountedChecked<VmoStorage>(&ac, std::move(vmo), std::move(content_size_manager),
                                             VmoStorage::InitialMutability::kMutable);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  out->reset(std::move(storage));
  return ZX_OK;
}

zx_status_t vmo::read(void* data, uint64_t offset, size_t len) const {
  LTRACEF("data %p offset %#" PRIx64 " len %#" PRIx64 "\n", data, offset, len);
  if (data == nullptr) {
    // nullptr is no valid kernel address it will fail inside Read
    // but as len is zero it ok, we just do nothing.
    if (len == 0) {
      return ZX_OK;
    }
    return ZX_ERR_INVALID_ARGS;
  }

  const fbl::RefPtr<VmoStorage>& tmp = get();
  if (!tmp) {
    return ZX_ERR_BAD_HANDLE;
  }
  return tmp->vmo_->Read(data, offset, len);
}

zx_status_t vmo::write(const void* data, uint64_t offset, size_t len) const {
  LTRACEF("data %p offset %#" PRIx64 " len %#" PRIx64 "\n", data, offset, len);
  if (data == nullptr) {
    // nullptr is not valid kernel address it will fail inside Write
    // but as len is zero it ok, we just do nothing.
    if (len == 0) {
      return ZX_OK;
    }
    return ZX_ERR_INVALID_ARGS;
  }

  const fbl::RefPtr<VmoStorage>& tmp = get();
  if (!tmp) {
    return ZX_ERR_BAD_HANDLE;
  }
  return tmp->vmo_->Write(data, offset, len);
}

zx_status_t vmo::create_child(uint32_t options, uint64_t offset, uint64_t size, bool copy_prop,
                              vmo* result) const {
  LTRACE;
  if (!get()) {
    return ZX_ERR_BAD_HANDLE;
  }

  zx_status_t status;
  fbl::RefPtr<VmObject> child_vmo;
  bool no_write = false;

  // Resizing a VMO requires the WRITE permissions, but NO_WRITE forbids the WRITE permissions, as
  // such it does not make sense to create a VMO with both of these.
  if ((options & ZX_VMO_CHILD_NO_WRITE) && (options & ZX_VMO_CHILD_RESIZABLE)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Writable is a property of the handle, not the object, so we consume this option here before
  // calling CreateChild.
  if (options & ZX_VMO_CHILD_NO_WRITE) {
    no_write = true;
    options &= ~ZX_VMO_CHILD_NO_WRITE;
  }

  // Save a copy of the rights for later.
  zx_rights_t in_rights = get()->rights_;

  // VmObjectDispatcher::CreateChild
  LTRACEF("options 0x%x offset %#" PRIx64 " size %#" PRIx64 "\n", options, offset, size);

  // clone the vmo into a new one
  status = create_child_internal(options, offset, size, copy_prop, &child_vmo);
  DEBUG_ASSERT((status == ZX_OK) == (child_vmo != nullptr));
  if (status != ZX_OK)
    return status;

  DEBUG_ASSERT(child_vmo);

  // This checks that the child VMO is explicitly created with ZX_VMO_CHILD_SNAPSHOT.
  // There are other ways that VMOs can be effectively immutable, for instance if the VMO is
  // created with ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE and meets certain criteria it will be
  // "upgraded" to a snapshot. However this behavior is not guaranteed at the API level.
  // A choice was made to conservatively only mark VMOs as immutable when the user explicitly
  // creates a VMO in a way that is guaranteed at the API level to always output an immutable VMO.
  auto initial_mutability = VmoStorage::InitialMutability::kMutable;
  if (no_write && (options & ZX_VMO_CHILD_SNAPSHOT)) {
    initial_mutability = VmoStorage::InitialMutability::kImmutable;
  }

  fbl::RefPtr<ContentSizeManager> content_size_manager;
  // A reference child shares the same content size manager as the parent.
  if (options & ZX_VMO_CHILD_REFERENCE) {
    content_size_manager = fbl::RefPtr(get()->content_size_mgr_.get());
  } else {
    status = ContentSizeManager::Create(size, &content_size_manager);
    if (status != ZX_OK) {
      return status;
    }
  }

  // create a VmoStorage
  fbl::AllocChecker ac;
  auto storage = fbl::MakeRefCountedChecked<VmoStorage>(
      &ac, std::move(child_vmo), std::move(content_size_manager), initial_mutability);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  zx_rights_t default_rights = storage->rights_;

  // Set the rights to the new handle to no greater than the input (parent) handle minus the RESIZE
  // right, which is added independently based on ZX_VMO_CHILD_RESIZABLE; it is possible for a
  // non-resizable parent to have a resizable child and vice versa. Always allow GET/SET_PROPERTY so
  // the user can set ZX_PROP_NAME on the new clone.
  zx_rights_t rights = (in_rights & ~ZX_RIGHT_RESIZE) |
                       (options & ZX_VMO_CHILD_RESIZABLE ? ZX_RIGHT_RESIZE : 0) |
                       ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_SET_PROPERTY;

  // Unless it was explicitly requested to be removed, WRITE can be added to CoW clones at the
  // expense of executability.
  if (no_write) {
    rights &= ~ZX_RIGHT_WRITE;
    // NO_WRITE and RESIZABLE cannot be specified together, so we should not have the RESIZE
    // right.
    DEBUG_ASSERT((rights & ZX_RIGHT_RESIZE) == 0);
  } else if (options & (ZX_VMO_CHILD_SNAPSHOT | ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE |
                        ZX_VMO_CHILD_SNAPSHOT_MODIFIED)) {
    rights &= ~ZX_RIGHT_EXECUTE;
    rights |= ZX_RIGHT_WRITE;
  }

  // make sure we're somehow not elevating rights beyond what a new vmo should have
  DEBUG_ASSERT(((default_rights | ZX_RIGHT_EXECUTE) & rights) == rights);

  storage->rights_ = rights;

  result->reset(std::move(storage));
  return ZX_OK;
}

zx_status_t vmo::create_child_internal(uint32_t options, uint64_t offset, uint64_t size,
                                       bool copy_name, fbl::RefPtr<VmObject>* child_vmo) const {
  LTRACE;
  fbl::RefPtr<VmObject> vmo = get()->vmo_;

  // Clones are not supported for discardable VMOs.
  if (vmo->is_discardable()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (options & ZX_VMO_CHILD_SLICE) {
    // No other flags are valid for slices.
    options &= ~ZX_VMO_CHILD_SLICE;
    if (options) {
      return ZX_ERR_INVALID_ARGS;
    }
    return vmo->CreateChildSlice(offset, size, copy_name, child_vmo);
  }

  Resizability resizable = Resizability::NonResizable;
  if (options & ZX_VMO_CHILD_REFERENCE) {
    options &= ~ZX_VMO_CHILD_REFERENCE;
    if (options & ZX_VMO_CHILD_RESIZABLE) {
      resizable = Resizability::Resizable;
      options &= ~ZX_VMO_CHILD_RESIZABLE;
    }
    if (options) {
      return ZX_ERR_INVALID_ARGS;
    }
    return vmo->CreateChildReference(resizable, offset, size, copy_name, nullptr, child_vmo);
  }

  // Check for mutually-exclusive child type flags.
  CloneType type;
  if (options & ZX_VMO_CHILD_SNAPSHOT) {
    options &= ~ZX_VMO_CHILD_SNAPSHOT;
    type = CloneType::Snapshot;
  } else if (options & ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE) {
    options &= ~ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE;
    type = CloneType::SnapshotAtLeastOnWrite;
  } else if (options & ZX_VMO_CHILD_SNAPSHOT_MODIFIED) {
    options &= ~ZX_VMO_CHILD_SNAPSHOT_MODIFIED;
    type = CloneType::SnapshotModified;
  } else {
    return ZX_ERR_INVALID_ARGS;
  }

  if (options & ZX_VMO_CHILD_RESIZABLE) {
    resizable = Resizability::Resizable;
    options &= ~ZX_VMO_CHILD_RESIZABLE;
  }

  if (options)
    return ZX_ERR_INVALID_ARGS;

  return vmo->CreateClone(resizable, type, offset, size, copy_name, child_vmo);
}

zx_status_t vmo::range_op(uint32_t op, uint64_t offset, uint64_t size, void* buffer,
                          size_t buffer_size) const {
  LTRACEF("op %u offset %#" PRIx64 " size %#" PRIx64 " buffer %p buffer_size %zu\n", op, offset,
          size, buffer, buffer_size);

  fbl::RefPtr<VmObject> vmo = get()->vmo_;

  switch (op) {
    case ZX_VMO_OP_COMMIT: {
      auto status = vmo->CommitRange(offset, size);
      return status;
    }
    case ZX_VMO_OP_DECOMMIT: {
      // TODO: handle partial decommits
      auto status = vmo->DecommitRange(offset, size);
      return status;
    }
    case ZX_VMO_OP_LOCK: {
      zx_vmo_lock_state_t lock_state = {};
      zx_status_t status = vmo->LockRange(offset, size, &lock_state);
      if (status != ZX_OK) {
        return status;
      }
      // If an error is encountered from this point on, the lock operation MUST be reverted
      // before returning.

      if (buffer_size < sizeof(zx_vmo_lock_state_t)) {
        // Undo the lock before returning an error.
        vmo->UnlockRange(offset, size);
        return ZX_ERR_INVALID_ARGS;
      }
      memcpy(buffer, &lock_state, sizeof(zx_vmo_lock_state_t));
      return ZX_OK;
    }
    case ZX_VMO_OP_TRY_LOCK:
      return vmo->TryLockRange(offset, size);
    case ZX_VMO_OP_UNLOCK:
      return vmo->UnlockRange(offset, size);
    case ZX_VMO_OP_CACHE_SYNC:
      return vmo->CacheOp(offset, size, VmObject::CacheOpType::Sync);
    case ZX_VMO_OP_CACHE_INVALIDATE:
      return vmo->CacheOp(offset, size, VmObject::CacheOpType::Invalidate);
    case ZX_VMO_OP_CACHE_CLEAN:
      return vmo->CacheOp(offset, size, VmObject::CacheOpType::Clean);
    case ZX_VMO_OP_CACHE_CLEAN_INVALIDATE:
      return vmo->CacheOp(offset, size, VmObject::CacheOpType::CleanInvalidate);
    case ZX_VMO_OP_ZERO:
      return vmo->ZeroRange(offset, size);
    case ZX_VMO_OP_ALWAYS_NEED:
      return vmo->HintRange(offset, size, VmObject::EvictionHint::AlwaysNeed);
    case ZX_VMO_OP_DONT_NEED:
      return vmo->HintRange(offset, size, VmObject::EvictionHint::DontNeed);
    default:
      return ZX_ERR_INVALID_ARGS;
  }
}

template <typename T>
inline T VmoInfoToVersion(const zx_info_vmo_t& vmo);

template <>
inline zx_info_vmo_t VmoInfoToVersion(const zx_info_vmo_t& vmo) {
  return vmo;
}

template <>
inline zx_info_vmo_v1_t VmoInfoToVersion(const zx_info_vmo_t& vmo) {
  zx_info_vmo_v1_t vmo_v1 = {};
  vmo_v1.koid = vmo.koid;
  memcpy(vmo_v1.name, vmo.name, sizeof(vmo.name));
  vmo_v1.size_bytes = vmo.size_bytes;
  vmo_v1.parent_koid = vmo.parent_koid;
  vmo_v1.num_children = vmo.num_children;
  vmo_v1.num_mappings = vmo.num_mappings;
  vmo_v1.share_count = vmo.share_count;
  vmo_v1.flags = vmo.flags;
  vmo_v1.committed_bytes = vmo.committed_bytes;
  vmo_v1.handle_rights = vmo.handle_rights;
  vmo_v1.cache_policy = vmo.cache_policy;
  return vmo_v1;
}

template <>
inline zx_info_vmo_v2_t VmoInfoToVersion(const zx_info_vmo_t& vmo) {
  zx_info_vmo_v2_t vmo_v2 = {};
  vmo_v2.koid = vmo.koid;
  memcpy(vmo_v2.name, vmo.name, sizeof(vmo.name));
  vmo_v2.size_bytes = vmo.size_bytes;
  vmo_v2.parent_koid = vmo.parent_koid;
  vmo_v2.num_children = vmo.num_children;
  vmo_v2.num_mappings = vmo.num_mappings;
  vmo_v2.share_count = vmo.share_count;
  vmo_v2.flags = vmo.flags;
  vmo_v2.committed_bytes = vmo.committed_bytes;
  vmo_v2.handle_rights = vmo.handle_rights;
  vmo_v2.cache_policy = vmo.cache_policy;
  vmo_v2.metadata_bytes = vmo.metadata_bytes;
  vmo_v2.committed_change_events = vmo.committed_change_events;
  return vmo_v2;
}

zx_info_vmo_t VmoToInfoEntry(const VmObject* vmo, VmoOwnership ownership,
                             zx_rights_t handle_rights) {
  zx_info_vmo_t entry = {};
  entry.koid = vmo->user_id();
  vmo->get_name(entry.name, sizeof(entry.name));
  entry.size_bytes = vmo->size();
  entry.parent_koid = vmo->parent_user_id();
  entry.num_children = vmo->num_children();
  entry.num_mappings = vmo->num_mappings();
  entry.share_count = vmo->share_count();
  entry.flags = (vmo->is_paged() ? ZX_INFO_VMO_TYPE_PAGED : ZX_INFO_VMO_TYPE_PHYSICAL) |
                (vmo->is_resizable() ? ZX_INFO_VMO_RESIZABLE : 0) |
                (vmo->is_discardable() ? ZX_INFO_VMO_DISCARDABLE : 0) |
                (vmo->is_user_pager_backed() ? ZX_INFO_VMO_PAGER_BACKED : 0) |
                (vmo->is_contiguous() ? ZX_INFO_VMO_CONTIGUOUS : 0);
  // As an implementation detail, both ends of an IOBuffer keep a child reference to a shared parent
  // which is dropped. Since references aren't normally attributed pages otherwise, we specifically
  // request their page count.
  VmObject::AttributionCounts page_counts = ownership == VmoOwnership::kIoBuffer
                                                ? vmo->AttributedPagesInReferenceOwner()
                                                : vmo->AttributedPages();
  entry.committed_bytes = page_counts.uncompressed * PAGE_SIZE;
  entry.populated_bytes = (page_counts.compressed + page_counts.uncompressed) * PAGE_SIZE;
  entry.cache_policy = vmo->GetMappingCachePolicy();
  switch (ownership) {
    case VmoOwnership::kHandle:
      entry.flags |= ZX_INFO_VMO_VIA_HANDLE;
      entry.handle_rights = handle_rights;
      break;
    case VmoOwnership::kMapping:
      entry.flags |= ZX_INFO_VMO_VIA_MAPPING;
      break;
    case VmoOwnership::kIoBuffer:
      entry.flags |= ZX_INFO_VMO_VIA_IOB_HANDLE;
      entry.handle_rights = handle_rights;
      break;
  }
  if (vmo->child_type() == VmObject::ChildType::kCowClone) {
    entry.flags |= ZX_INFO_VMO_IS_COW_CLONE;
  }
  entry.metadata_bytes = vmo->HeapAllocationBytes();
  // Only events that change committed pages are different kinds of reclamation.
  entry.committed_change_events = vmo->ReclamationEventCount();
  return entry;
}

zx_info_vmo_t vmo::get_vmo_info(zx_rights_t rights) const {
  zx_info_vmo_t info = VmoToInfoEntry(get()->vmo_.get(), VmoOwnership::kHandle, rights);
  if (get()->initial_mutability_ == VmoStorage::InitialMutability::kImmutable) {
    info.flags |= ZX_INFO_VMO_IMMUTABLE;
  }
  return info;
}

zx_status_t vmo::get_info(uint32_t topic, void* buffer, size_t buffer_size, size_t* actual_count,
                          size_t* avail_count) const {
  LTRACE;
  if (!get()) {
    return ZX_ERR_BAD_HANDLE;
  }

  switch (topic) {
    case ZX_INFO_VMO_V1:
    case ZX_INFO_VMO_V2:
    case ZX_INFO_VMO: {
      zx_info_vmo_t entry = get_vmo_info(0);
      if (topic == ZX_INFO_VMO_V1) {
        zx_info_vmo_v1_t versioned_vmo = VmoInfoToVersion<zx_info_vmo_v1_t>(entry);
        //  The V1 layout is a subset of V2
        return single_record_result(buffer, buffer_size, actual_count, avail_count, versioned_vmo);
      } else if (topic == ZX_INFO_VMO_V2) {
        zx_info_vmo_v2_t versioned_vmo = VmoInfoToVersion<zx_info_vmo_v2_t>(entry);
        //  The V2 layout is a subset of V3
        return single_record_result(buffer, buffer_size, actual_count, avail_count, versioned_vmo);
      } else {
        return single_record_result(buffer, buffer_size, actual_count, avail_count, entry);
      }
    }
    case ZX_INFO_HANDLE_BASIC: {
      zx_rights_t rights = ZX_RIGHT_NONE;

      // build the info structure
      zx_info_handle_basic_t info = {
          .koid = 0,
          .rights = rights,
          .type = ZX_OBJ_TYPE_VMO,
          .related_koid = 0,
          .reserved = 0u,
          .padding1 = {},
      };
      return single_record_result(buffer, buffer_size, actual_count, avail_count, info);
    }
    case ZX_INFO_HANDLE_COUNT: {
      zx_info_handle_count_t info = {.handle_count =
                                         static_cast<uint32_t>(get()->vmo_->ref_count_debug())};
      return single_record_result(buffer, buffer_size, actual_count, avail_count, info);
    }
    default:
      LTRACEF("[NOT_SUPPORTED] Topic %d\n", topic);
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t vmo::get_property(uint32_t property, void* value, size_t size) const {
  LTRACE;
  if (!get()) {
    return ZX_ERR_BAD_HANDLE;
  }

  switch (property) {
    case ZX_PROP_NAME: {
      if (size < ZX_MAX_NAME_LEN)
        return ZX_ERR_BUFFER_TOO_SMALL;
      char name[ZX_MAX_NAME_LEN] = {};
      get()->vmo_->get_name(name, ZX_MAX_NAME_LEN);
      memcpy(value, name, ZX_MAX_NAME_LEN);
      return ZX_OK;
    }
    case ZX_PROP_VMO_CONTENT_SIZE: {
      if (size < sizeof(uint64_t)) {
        return ZX_ERR_BUFFER_TOO_SMALL;
      }
      *reinterpret_cast<uint64_t*>(value) = get()->content_size_mgr_->GetContentSize();
      return ZX_OK;
    }
    default:
      LTRACEF("[NOT_SUPPORTED] Property %d\n", property);
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t vmo::set_property(uint32_t property, const void* value, size_t size) const {
  LTRACE;
  if (!get()) {
    return ZX_ERR_BAD_HANDLE;
  }

  switch (property) {
    case ZX_PROP_NAME: {
      if (size >= ZX_MAX_NAME_LEN)
        size = ZX_MAX_NAME_LEN - 1;
      // char name[ZX_MAX_NAME_LEN - 1];
      // memcpy(name, value, size);
      return get()->vmo_->set_name(static_cast<const char*>(value), size);
    }
    case ZX_PROP_VMO_CONTENT_SIZE: {
      if (size < sizeof(uint64_t)) {
        return ZX_ERR_BUFFER_TOO_SMALL;
      }
      return set_content_size(*reinterpret_cast<const uint64_t*>(value));
    }
    default:
      LTRACEF("[NOT_SUPPORTED] Property %d\n", property);
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t vmo::set_size(uint64_t size) const {
  const fbl::RefPtr<VmoStorage>& thiz = get();
  if (!thiz) {
    return ZX_ERR_BAD_HANDLE;
  }

  // VMOs that are not resizable should fail with ZX_ERR_UNAVAILABLE for backwards compatibility,
  // which will be handled by the SetSize call below. Only validate the RESIZE right if the VMO is
  // resizable.
  if (thiz->vmo_->is_resizable() && (thiz->rights_ & ZX_RIGHT_RESIZE) == 0) {
    return ZX_ERR_ACCESS_DENIED;
  }

  // do the operation
  fbl::RefPtr<ContentSizeManager> content_size_mgr = thiz->content_size_mgr_;
  ContentSizeManager::Operation op;
  Guard<Mutex> guard{content_size_mgr->lock()};

  content_size_mgr->BeginSetContentSizeLocked(size, &op, &guard);

  uint64_t size_aligned = ROUNDUP(size, PAGE_SIZE);
  // Check for overflow when rounding up.
  if (size_aligned < size) {
    op.AssertParentLockHeld();
    op.CancelLocked();
    return ZX_ERR_OUT_OF_RANGE;
  }

  zx_status_t status = thiz->vmo_->Resize(size_aligned);
  if (status != ZX_OK) {
    op.AssertParentLockHeld();
    op.CancelLocked();
    return status;
  }

  uint64_t remaining = size_aligned - size;
  if (remaining > 0) {
    // TODO(https://fxbug.dev/42053728): Determine whether failure to ZeroRange here should undo
    // this operation.
    //
    // Dropping the lock here is fine, as an `Operation` only needs to be locked when initializing,
    // committing, or cancelling.
    guard.CallUnlocked([&] { thiz->vmo_->ZeroRange(size, remaining); });
  }

  op.AssertParentLockHeld();
  op.CommitLocked();
  return status;
}

zx_status_t vmo::set_content_size(uint64_t content_size) const {
  const fbl::RefPtr<VmoStorage>& thiz = get();
  if (!thiz) {
    return ZX_ERR_BAD_HANDLE;
  }

  fbl::RefPtr<ContentSizeManager> content_size_mgr = thiz->content_size_mgr_;
  ContentSizeManager::Operation op;
  Guard<Mutex> guard{content_size_mgr->lock()};
  content_size_mgr->BeginSetContentSizeLocked(content_size, &op, &guard);

  uint64_t vmo_size = thiz->vmo_->size();
  if (content_size < vmo_size) {
    // TODO(https://fxbug.dev/42053728): Determine whether failure to ZeroRange here should undo
    // this operation.
    //
    // Dropping the lock here is fine, as an `Operation` only needs to be locked when initializing,
    // committing, or cancelling.
    guard.CallUnlocked([&] { thiz->vmo_->ZeroRange(content_size, vmo_size - content_size); });
  }

  op.AssertParentLockHeld();
  op.CommitLocked();
  return ZX_OK;
}

template <>
bool operator<(const unowned<vmo>& a, const unowned<vmo>& b) {
  return a->get()->vmo()->user_id() < b->get()->vmo()->user_id();
}

}  // namespace zx
