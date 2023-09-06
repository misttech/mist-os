// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host2/legacy_lifecycle_shim.h"

#include <lib/async/cpp/task.h>
#include <lib/fdf/dispatcher.h>

#include "src/devices/lib/log/log.h"

namespace dfv2 {

LegacyLifecycleShim::LegacyLifecycleShim(const DriverLifecycle* lifecycle, std::string_view name)
    : lifecycle_(lifecycle), name_(name) {}

void LegacyLifecycleShim::Initialize(fdf_dispatcher_t* dispatcher,
                                     fdf::ServerEnd<fuchsia_driver_framework::Driver> server_end) {
  dispatcher_ = dispatcher;
  async::PostTask(fdf_dispatcher_get_async_dispatcher(dispatcher_),
                  [this, server = std::move(server_end)]() mutable {
                    binding_.emplace(fdf_dispatcher_get_current_dispatcher(), std::move(server),
                                     this, fidl::kIgnoreBindingClosure);
                  });
}

zx_status_t LegacyLifecycleShim::Destroy() {
  void* opaque = nullptr;
  {
    fbl::AutoLock al(&lock_);
    if (opaque_.has_value()) {
      opaque = opaque_.value();
    }
  }

  if (opaque) {
    zx_status_t status = lifecycle_->v1.stop(opaque);
    if (status != ZX_OK) {
      LOGF(ERROR, "Failed to stop driver '%s': %s", name_.c_str(), zx_status_get_string(status));
    }

    return status;
  }

  return ZX_ERR_NOT_FOUND;
}

void LegacyLifecycleShim::Start(StartRequestView request, fdf::Arena& arena,
                                StartCompleter::Sync& completer) {
  fidl::OwnedEncodeResult encoded = fidl::StandaloneEncode(request->start_args);
  if (!encoded.message().ok()) {
    LOGF(ERROR, "Failed to start driver '%s', could not encode start args: %s", name_.c_str(),
         encoded.message().FormatDescription().data());
    completer.buffer(arena).ReplyError(encoded.message().status());
    return;
  }
  fidl_opaque_wire_format_metadata_t wire_format_metadata =
      encoded.wire_format_metadata().ToOpaque();

  // We convert the outgoing message into an incoming message to provide to the
  // driver on start.
  fidl::OutgoingToEncodedMessage converted_message{encoded.message()};
  if (!converted_message.ok()) {
    LOGF(ERROR, "Failed to start driver '%s', could not convert start args: %s", name_.c_str(),
         converted_message.FormatDescription().data());
    completer.buffer(arena).ReplyError(converted_message.status());
    return;
  }

  // After calling |lifecycle_->start|, we assume it has taken ownership of
  // the handles from |start_args|, and can therefore relinquish ownership.
  auto [bytes, handles] = std::move(converted_message.message()).Release();
  EncodedFidlMessage msg{
      .bytes = bytes.data(),
      .handles = handles.data(),
      .num_bytes = static_cast<uint32_t>(bytes.size()),
      .num_handles = static_cast<uint32_t>(handles.size()),
  };

  // Async start was added in version 3.
  if (lifecycle_->version >= 3 && lifecycle_->v3.start != nullptr) {
    start_completer_.emplace(completer.ToAsync());
    start_arena_.emplace(std::move(arena));
    lifecycle_->v3.start(
        {msg, wire_format_metadata}, dispatcher_,
        [](void* cookie, zx_status_t status, void* opaque) {
          LegacyLifecycleShim* self = static_cast<LegacyLifecycleShim*>(cookie);
          async::PostTask(
              fdf_dispatcher_get_async_dispatcher(self->dispatcher_), [self, status, opaque]() {
                ZX_ASSERT(self->start_completer_.has_value());
                ZX_ASSERT(self->start_arena_.has_value());
                self->start_completer_->buffer(*self->start_arena_).Reply(zx::make_result(status));
                self->start_completer_.reset();
                self->start_arena_.reset();

                fbl::AutoLock al(&self->lock_);
                self->opaque_.emplace(opaque);
              });
        },
        this);

  } else {
    void* opaque;
    zx_status_t status = lifecycle_->v1.start({msg, wire_format_metadata}, dispatcher_, &opaque);
    completer.buffer(arena).Reply(zx::make_result(status));

    fbl::AutoLock al(&lock_);
    opaque_.emplace(opaque);
  }
}

void LegacyLifecycleShim::Stop(fdf::Arena& arena, StopCompleter::Sync& completer) {
  if (lifecycle_->version >= 2 && lifecycle_->v2.prepare_stop != nullptr) {
    void* opaque = nullptr;
    {
      fbl::AutoLock al(&lock_);
      ZX_ASSERT(opaque_.has_value());
      opaque = opaque_.value();
    }
    lifecycle_->v2.prepare_stop(
        opaque,
        [](void* cookie, zx_status_t status) {
          LegacyLifecycleShim* self = static_cast<LegacyLifecycleShim*>(cookie);
          async::PostTask(fdf_dispatcher_get_async_dispatcher(self->dispatcher_),
                          [self]() { self->binding_.reset(); });
        },
        this);
  } else {
    binding_.reset();
  }
}

void LegacyLifecycleShim::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_driver_framework::Driver> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  LOGF(
      WARNING,
      "Driver binding for '%s' received unknown method request: unknown_method_type(%d), method_ordinal(%lu)",
      name_.c_str(), metadata.unknown_method_type, metadata.method_ordinal);
  completer.Close(ZX_ERR_NOT_SUPPORTED);
}

}  // namespace dfv2
