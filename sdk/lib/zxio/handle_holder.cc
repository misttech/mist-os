// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/zxio/handle_holder.h"

#include <lib/zxio/null.h>
#include <lib/zxio/ops.h>

#include <new>
#include <utility>

namespace zxio {

namespace {

// A zxio_handle_holder is a zxio object instance that holds on to a handle and
// allows it to be closed or released via zxio_close() / zxio_release(). It is
// useful for wrapping objects that zxio does not understand.
struct zxio_handle_holder {
  zxio_t io;
  zx::handle handle;
};

static_assert(sizeof(zxio_handle_holder) <= sizeof(zxio_storage_t),
              "zxio_handle_holder must fit inside zxio_storage_t.");

zxio_handle_holder& zxio_get_handle_holder(zxio_t* io) {
  return *reinterpret_cast<zxio_handle_holder*>(io);
}

constexpr zxio_ops_t zxio_handle_holder_ops = []() {
  zxio_ops_t ops = zxio_default_ops;
  ops.close = [](zxio_t* io, const bool should_wait) {
    zxio_get_handle_holder(io).~zxio_handle_holder();
    return ZX_OK;
  };

  ops.release = [](zxio_t* io, zx_handle_t* out_handle) {
    const zx_handle_t handle = zxio_get_handle_holder(io).handle.release();
    if (handle == ZX_HANDLE_INVALID) {
      return ZX_ERR_BAD_HANDLE;
    }
    *out_handle = handle;
    return ZX_OK;
  };
  return ops;
}();

}  // namespace

void handle_holder_init(zxio_storage_t* storage, zx::handle handle) {
  auto holder = new (storage) zxio_handle_holder{
      .handle = std::move(handle),
  };
  zxio_init(&holder->io, &zxio_handle_holder_ops);
}

}  // namespace zxio
