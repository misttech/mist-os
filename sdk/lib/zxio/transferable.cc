// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.unknown/cpp/wire.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fit/defer.h>
#include <lib/zx/channel.h>
#include <lib/zxio/null.h>
#include <lib/zxio/ops.h>

#include "sdk/lib/zxio/private.h"

namespace funknown = fuchsia_unknown;

namespace {

class Transferable : public HasIo {
 public:
  Transferable(zx::channel channel) : HasIo(kOps), channel_(std::move(channel)) {}

 private:
  static const zxio_ops_t kOps;

  zx_status_t Close(bool should_wait);
  zx_status_t Clone(zx_handle_t* out_handle);
  zx_status_t Release(zx_handle_t* out_handle);
  zx_status_t AttrGet(zxio_node_attributes_t* inout_attr);
  zx_status_t FlagsGet(uint32_t* out_flags);
  zx_status_t FlagsSet(uint32_t flags);

  zx::channel channel_;
};

constexpr zxio_ops_t Transferable::kOps = []() {
  using Adaptor = Adaptor<Transferable>;
  zxio_ops_t ops = zxio_default_ops;
  ops.close = Adaptor::From<&Transferable::Close>;
  ops.clone = Adaptor::From<&Transferable::Clone>;
  ops.release = Adaptor::From<&Transferable::Release>;
  ops.attr_get = Adaptor::From<&Transferable::AttrGet>;
  ops.flags_get = Adaptor::From<&Transferable::FlagsGet>;
  ops.flags_set = Adaptor::From<&Transferable::FlagsSet>;
  return ops;
}();

zx_status_t Transferable::Close(bool should_wait) {
  auto cleanup = fit::defer([this] { this->~Transferable(); });

  if (channel_.is_valid() && should_wait) {
    auto client = fidl::UnownedClientEnd<funknown::Closeable>(channel_.get());

    const fidl::WireResult result = fidl::WireCall(client)->Close();
    if (!result.ok()) {
      return result.status();
    }
    const auto& response = result.value();
    if (response.is_error()) {
      return response.error_value();
    }
  }
  return ZX_OK;
}

zx_status_t Transferable::Clone(zx_handle_t* out_handle) {
  if (!channel_.is_valid()) {
    return ZX_ERR_INVALID_ARGS;
  }
  auto client = fidl::UnownedClientEnd<funknown::Cloneable>(channel_.get());
  auto [client_end, server_end] = fidl::Endpoints<funknown::Cloneable>::Create();
#if FUCHSIA_API_LEVEL_AT_LEAST(NEXT)
  const fidl::Status result = fidl::WireCall(client)->Clone(std::move(server_end));
#else
  const fidl::Status result = fidl::WireCall(client)->Clone2(std::move(server_end));
#endif
  if (!result.ok()) {
    return result.status();
  }
  *out_handle = client_end.TakeChannel().release();
  return ZX_OK;
}

zx_status_t Transferable::Release(zx_handle_t* out_handle) {
  *out_handle = channel_.release();
  return ZX_OK;
}

zx_status_t Transferable::AttrGet(zxio_node_attributes_t* inout_attr) {
  if (inout_attr->has.abilities) {
    ZXIO_NODE_ATTR_SET(*inout_attr, abilities, ZXIO_OPERATION_GET_ATTRIBUTES);
  }
  if (inout_attr->has.object_type) {
    ZXIO_NODE_ATTR_SET(*inout_attr, object_type, ZXIO_OBJECT_TYPE_TRANSFERABLE);
  }
  return ZX_OK;
}

zx_status_t Transferable::FlagsGet(uint32_t* out_flags) {
  // By default a transferable is readable and writeable - zxio doesn't know.
  fuchsia_io::wire::OpenFlags flags{};
  flags |= fuchsia_io::wire::OpenFlags::kRightReadable;
  flags |= fuchsia_io::wire::OpenFlags::kRightWritable;
  *out_flags = static_cast<uint32_t>(flags);
  return ZX_OK;
}

zx_status_t Transferable::FlagsSet(uint32_t flags) { return ZX_OK; }

}  // namespace

zx_status_t zxio_transferable_init(zxio_storage_t* storage, zx::channel channel) {
  new (storage) Transferable(std::move(channel));
  return ZX_OK;
}
