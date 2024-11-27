// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/object.h"

#include <trace.h>
#include <zircon/types.h>

#include <object/diagnostics.h>
#include <object/dispatcher.h>
#include <object/handle.h>
#include <object/process_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>

#define LOCAL_TRACE 0

namespace zx {

namespace {
template <typename T>
inline T VmoInfoToVersion(const zx_info_vmo_t& vmo);

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

// Copies a single record, |src_record|, into the user buffer |dst_buffer| of size
// |dst_buffer_size|.
//
// If the copy succeeds, the value 1 is copied into |user_avail| and |user_actual| (if non-null).
//
// If the copy fails because the buffer it too small, |user_avail| and |user_actual| will receive
// the values 1 and 0 respectively (if non-null).
template <typename T>
zx_status_t single_record_result(void* dst_buffer, size_t dst_buffer_size, size_t* user_actual,
                                 size_t* user_avail, const T& src_record) {
  size_t avail = 1;
  size_t actual;
  if (dst_buffer_size >= sizeof(T)) {
    memcpy(dst_buffer, &src_record, sizeof(T));
    actual = 1;
  } else {
    actual = 0;
  }
  if (user_actual) {
    *user_actual = actual;
  }
  if (user_avail) {
    *user_avail = avail;
  }
  if (actual == 0)
    return ZX_ERR_BUFFER_TOO_SMALL;
  return ZX_OK;
}

}  // namespace

zx_status_t object_base::get_info(uint32_t topic, void* _buffer, size_t buffer_size,
                                  size_t* _actual, size_t* _avail) const {
  if (!value_)
    return ZX_ERR_BAD_HANDLE;

  auto current_handle = value_->get();
  if (!current_handle)
    return ZX_ERR_BAD_HANDLE;

  LTRACEF("handle %p topic %u\n", current_handle, topic);

  fbl::RefPtr<Dispatcher> dispatcher = current_handle->dispatcher();
  switch (topic) {
    case ZX_INFO_HANDLE_VALID: {
      return is_valid();
    }
    case ZX_INFO_HANDLE_BASIC: {
      // TODO(https://fxbug.dev/42105279): Handle forward/backward compatibility issues
      // with changes to the struct.

      zx_rights_t rights = current_handle->rights();

      // build the info structure
      zx_info_handle_basic_t info = {
          .koid = dispatcher->get_koid(),
          .rights = rights,
          .type = dispatcher->get_type(),
          .related_koid = dispatcher->get_related_koid(),
          .reserved = 0u,
          .padding1 = {},
      };

      return single_record_result(_buffer, buffer_size, _actual, _avail, info);
    }
    case ZX_INFO_VMO_V1:
    case ZX_INFO_VMO_V2:
    case ZX_INFO_VMO: {
      // lookup the dispatcher from handle
      auto vmo = DownCastDispatcher<VmObjectDispatcher>(&dispatcher);
      zx_rights_t rights = current_handle->rights();
      zx_info_vmo_t entry = vmo->GetVmoInfo(rights);
      if (topic == ZX_INFO_VMO_V1) {
        zx_info_vmo_v1_t versioned_vmo = VmoInfoToVersion<zx_info_vmo_v1_t>(entry);
        // The V1 layout is a subset of V2
        return single_record_result(_buffer, buffer_size, _actual, _avail, versioned_vmo);
      } else if (topic == ZX_INFO_VMO_V2) {
        zx_info_vmo_v2_t versioned_vmo = VmoInfoToVersion<zx_info_vmo_v2_t>(entry);
        // The V2 layout is a subset of V3
        return single_record_result(_buffer, buffer_size, _actual, _avail, versioned_vmo);
      } else {
        return single_record_result(_buffer, buffer_size, _actual, _avail, entry);
      }
    }
    case ZX_INFO_VMAR: {
      auto vmar = DownCastDispatcher<VmAddressRegionDispatcher>(&dispatcher);
      if (!current_handle->HasRights(ZX_RIGHT_INSPECT)) {
        return ZX_ERR_ACCESS_DENIED;
      }

      auto real_vmar = vmar->vmar();
      zx_info_vmar_t info = {
          .base = real_vmar->base(),
          .len = real_vmar->size(),
      };

      return single_record_result(_buffer, buffer_size, _actual, _avail, info);
    }
    case ZX_INFO_HANDLE_COUNT: {
      if (!current_handle->HasRights(ZX_RIGHT_INSPECT)) {
        return ZX_ERR_ACCESS_DENIED;
      }

      zx_info_handle_count_t info = {.handle_count = Handle::Count(ktl::move(dispatcher))};

      return single_record_result(_buffer, buffer_size, _actual, _avail, info);
    }
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t object_base::get_property(uint32_t property, void* _value, size_t size) const {
  if (!_value)
    return ZX_ERR_INVALID_ARGS;

  if (!value_)
    return ZX_ERR_BAD_HANDLE;

  auto current_handle = value_->get();
  if (!current_handle)
    return ZX_ERR_BAD_HANDLE;

  zx_status_t status =
      current_handle->HasRights(ZX_RIGHT_GET_PROPERTY) ? ZX_OK : ZX_ERR_ACCESS_DENIED;
  if (status != ZX_OK)
    return status;
  fbl::RefPtr<Dispatcher> dispatcher = current_handle->dispatcher();

  switch (property) {
    case ZX_PROP_NAME: {
      if (size < ZX_MAX_NAME_LEN)
        return ZX_ERR_BUFFER_TOO_SMALL;
      char name[ZX_MAX_NAME_LEN] = {};
      status = dispatcher->get_name(name);
      if (status != ZX_OK) {
        return status;
      }
      memcpy(_value, name, ZX_MAX_NAME_LEN);
      return ZX_OK;
    }
    case ZX_PROP_PROCESS_DEBUG_ADDR: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_PROCESS_BREAK_ON_LOAD: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_PROCESS_VDSO_BASE_ADDRESS: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_PROCESS_HW_TRACE_CONTEXT_ID: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_SOCKET_RX_THRESHOLD: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_SOCKET_TX_THRESHOLD: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_EXCEPTION_STATE: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_EXCEPTION_STRATEGY: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_VMO_CONTENT_SIZE: {
      if (size < sizeof(uint64_t)) {
        return ZX_ERR_BUFFER_TOO_SMALL;
      }
      auto vmo = DownCastDispatcher<VmObjectDispatcher>(&dispatcher);
      if (!vmo) {
        return ZX_ERR_WRONG_TYPE;
      }

      uint64_t value = vmo->GetContentSize();
      memcpy(_value, &value, sizeof(value));

      return ZX_OK;
    }
    case ZX_PROP_STREAM_MODE_APPEND: {
      return ZX_ERR_NOT_SUPPORTED;
    }
#if ARCH_X86
    case ZX_PROP_REGISTER_FS: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_REGISTER_GS: {
      return ZX_ERR_NOT_SUPPORTED;
    }
#endif

    default:
      return ZX_ERR_NOT_SUPPORTED;
  }

  __UNREACHABLE;
}

zx_status_t object_base::set_property(uint32_t property, const void* _value, size_t size) const {
  if (!_value)
    return ZX_ERR_INVALID_ARGS;

  if (!value_)
    return ZX_ERR_BAD_HANDLE;

  auto current_handle = value_->get();
  if (!current_handle)
    return ZX_ERR_BAD_HANDLE;

  zx_status_t status =
      current_handle->HasRights(ZX_RIGHT_SET_PROPERTY) ? ZX_OK : ZX_ERR_ACCESS_DENIED;
  if (status != ZX_OK)
    return status;
  fbl::RefPtr<Dispatcher> dispatcher = current_handle->dispatcher();

  switch (property) {
    case ZX_PROP_NAME: {
      if (size >= ZX_MAX_NAME_LEN)
        size = ZX_MAX_NAME_LEN - 1;
      char name[ZX_MAX_NAME_LEN - 1];
      memcpy(name, _value, size);
      return dispatcher->set_name(name, size);
    }
#if ARCH_X86
    case ZX_PROP_REGISTER_FS: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_REGISTER_GS: {
      return ZX_ERR_NOT_SUPPORTED;
    }
#endif
    case ZX_PROP_PROCESS_DEBUG_ADDR: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_PROCESS_BREAK_ON_LOAD: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_SOCKET_RX_THRESHOLD: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_SOCKET_TX_THRESHOLD: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_JOB_KILL_ON_OOM: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_EXCEPTION_STATE: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_EXCEPTION_STRATEGY: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    case ZX_PROP_VMO_CONTENT_SIZE: {
      if (size < sizeof(uint64_t)) {
        return ZX_ERR_BUFFER_TOO_SMALL;
      }
      auto vmo = DownCastDispatcher<VmObjectDispatcher>(&dispatcher);
      if (!vmo) {
        return ZX_ERR_WRONG_TYPE;
      }
      uint64_t dst_value = 0;
      memcpy(&dst_value, _value, sizeof(uint64_t));
      return vmo->SetContentSize(dst_value);
    }
    case ZX_PROP_STREAM_MODE_APPEND: {
      return ZX_ERR_NOT_SUPPORTED;
    }
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }

  __UNREACHABLE;
}

bool operator<(const fbl::RefPtr<Value>& a, const fbl::RefPtr<Value>& b) {
  return (a->get() < b->get());
}

}  // namespace zx
