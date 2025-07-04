// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_iovec.h>
#include <lib/user_copy/user_ptr.h>
#include <platform.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <object/handle.h>
#include <object/stream_dispatcher.h>
#include <vm/vm_aspace.h>

#define LOCAL_TRACE 0

// zx_status_t zx_stream_create
zx_status_t sys_stream_create(uint32_t options, zx_handle_t vmo_handle, zx_off_t seek,
                              zx_handle_t* out_stream) {
  if ((options & ~ZX_STREAM_CREATE_MASK) != 0)
    return ZX_ERR_INVALID_ARGS;

  uint32_t stream_options = 0;
  zx_rights_t desired_vmo_rights = ZX_RIGHT_NONE;
  zx_status_t status =
      StreamDispatcher::parse_create_syscall_flags(options, &stream_options, &desired_vmo_rights);
  if (status != ZX_OK) {
    return status;
  }

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<VmObjectDispatcher> disp;
  zx_rights_t actual_vmo_rights = ZX_RIGHT_NONE;
  status = up->handle_table().GetDispatcherWithRights(*up, vmo_handle, desired_vmo_rights, &disp,
                                                      &actual_vmo_rights);
  if (status != ZX_OK)
    return status;

  // Cannot create a stream from a physical or contiguous VMO.
  fbl::RefPtr<VmObjectPaged> vmo = DownCastVmObject<VmObjectPaged>(disp->vmo());
  if (!vmo || vmo->is_contiguous()) {
    return ZX_ERR_WRONG_TYPE;
  }

  // Remember whether this stream can resize the underlying VMO when required. Note that it might be
  // possible for stream writes / appends to proceed by manipulating only the content size, without
  // having to change the VMO size. This flag will be checked only for stream operations that would
  // require manipulating the VMO size, more specifically when expanding the VMO size if the
  // requested content size needs to surpass the current VMO size.
  if (actual_vmo_rights & ZX_RIGHT_RESIZE) {
    stream_options |= StreamDispatcher::kCanResizeVmo;
  }

  auto result = disp->content_size_manager();
  if (result.is_error()) {
    return result.status_value();
  }

  KernelHandle<StreamDispatcher> new_handle;
  zx_rights_t rights;
  status = StreamDispatcher::Create(stream_options, ktl::move(vmo), ktl::move(*result), seek,
                                    &new_handle, &rights);
  if (status != ZX_OK)
    return status;
  return up->MakeAndAddHandle(ktl::move(new_handle), rights, out_stream);
}

// zx_status_t zx_stream_writev
zx_status_t sys_stream_writev(zx_handle_t handle, uint32_t options,
                              user_in_ptr<const zx_iovec_t> vector, size_t vector_count,
                              user_out_ptr<size_t> out_actual) {
  LTRACEF("handle %x\n", handle);

  if (options & ~ZX_STREAM_APPEND) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!vector) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<StreamDispatcher> stream;
  {
    zx_status_t status =
        up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &stream);
    if (status != ZX_OK) {
      return status;
    }
  }

  auto [status, actual] = options & ZX_STREAM_APPEND
                              ? stream->AppendVector(make_user_in_iovec(vector, vector_count))
                              : stream->WriteVector(make_user_in_iovec(vector, vector_count));

  if (status == ZX_OK && out_actual) {
    status = out_actual.copy_to_user(actual);
  }

  return status;
}

// zx_status_t zx_stream_writev_at
zx_status_t sys_stream_writev_at(zx_handle_t handle, uint32_t options, zx_off_t offset,
                                 user_in_ptr<const zx_iovec_t> vector, size_t vector_count,
                                 user_out_ptr<size_t> out_actual) {
  LTRACEF("handle %x\n", handle);

  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!vector) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<StreamDispatcher> stream;
  {
    zx_status_t status =
        up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_WRITE, &stream);
    if (status != ZX_OK) {
      return status;
    }
  }

  auto [status, actual] = stream->WriteVectorAt(make_user_in_iovec(vector, vector_count), offset);

  if (status == ZX_OK && out_actual) {
    status = out_actual.copy_to_user(actual);
  }

  return status;
}

// zx_status_t zx_stream_readv
zx_status_t sys_stream_readv(zx_handle_t handle, uint32_t options, user_out_ptr<zx_iovec_t> vector,
                             size_t vector_count, user_out_ptr<size_t> out_actual) {
  LTRACEF("handle %x\n", handle);

  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!vector) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<StreamDispatcher> stream;
  {
    zx_status_t status =
        up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_READ, &stream);
    if (status != ZX_OK) {
      return status;
    }
  }

  auto [status, actual] = stream->ReadVector(make_user_out_iovec(vector, vector_count));

  if (status == ZX_OK && out_actual) {
    status = out_actual.copy_to_user(actual);
  }

  return status;
}

// zx_status_t zx_stream_readv_at
zx_status_t sys_stream_readv_at(zx_handle_t handle, uint32_t options, zx_off_t offset,
                                user_out_ptr<zx_iovec_t> vector, size_t vector_count,
                                user_out_ptr<size_t> out_actual) {
  LTRACEF("handle %x\n", handle);

  if (options != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!vector) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<StreamDispatcher> stream;
  {
    zx_status_t status =
        up->handle_table().GetDispatcherWithRights(*up, handle, ZX_RIGHT_READ, &stream);
    if (status != ZX_OK) {
      return status;
    }
  }

  auto [status, actual] = stream->ReadVectorAt(make_user_out_iovec(vector, vector_count), offset);

  if (status == ZX_OK && out_actual) {
    status = out_actual.copy_to_user(actual);
  }

  return status;
}

// zx_status_t zx_stream_seek
zx_status_t sys_stream_seek(zx_handle_t handle, zx_stream_seek_origin_t whence, int64_t offset,
                            user_out_ptr<zx_off_t> out_seek) {
  LTRACEF("handle %x\n", handle);

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<StreamDispatcher> stream;
  zx_rights_t rights;
  zx_status_t status = up->handle_table().GetDispatcherAndRights(*up, handle, &stream, &rights);
  if (status != ZX_OK) {
    return status;
  }
  if ((rights & (ZX_RIGHT_READ | ZX_RIGHT_WRITE)) == 0) {
    return ZX_ERR_ACCESS_DENIED;
  }
  zx_off_t seek = 0u;
  status = stream->Seek(whence, offset, &seek);

  if (status == ZX_OK && out_seek) {
    status = out_seek.copy_to_user(seek);
  }
  return status;
}
