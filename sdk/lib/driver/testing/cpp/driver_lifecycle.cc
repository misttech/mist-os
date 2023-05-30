// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/driver_lifecycle.h>

#include "lib/fdf/testing.h"

namespace fdf_testing {

namespace internal {

void StartDriverV3(const DriverLifecycle& lifecycle, EncodedDriverStartArgs encoded_start_args,
                   fdf_dispatcher* dispatcher, std::shared_ptr<libsync::Completion> completion,
                   fit::callback<void(zx::result<OpaqueDriverPtr>)> callback) {
  struct Cookie {
    std::shared_ptr<libsync::Completion> completion;
    fit::callback<void(zx::result<OpaqueDriverPtr>)> callback;
    async_dispatcher_t* dispatcher;
  };
  Cookie* cookie = new Cookie({
      .completion = std::move(completion),
      .callback = std::move(callback),
      .dispatcher = fdf_dispatcher_get_async_dispatcher(dispatcher),
  });

  lifecycle.v3.start(
      encoded_start_args, dispatcher,
      [](void* cookie, zx_status_t status, void* opaque) {
        auto* ctx = static_cast<Cookie*>(cookie);
        async::PostTask(ctx->dispatcher, [ctx, status, opaque]() {
          ctx->callback(zx::make_result(status, opaque));
          ctx->completion->Signal();
          delete ctx;
        });
      },
      cookie);
}

void PrepareStopV2(const DriverLifecycle& lifecycle, void* driver, fdf_dispatcher* dispatcher,
                   std::shared_ptr<libsync::Completion> completion,
                   fit::callback<void(zx::result<>)> callback) {
  struct Cookie {
    std::shared_ptr<libsync::Completion> completion;
    fit::callback<void(zx::result<>)> callback;
    async_dispatcher_t* dispatcher;
  };
  Cookie* cookie = new Cookie({
      .completion = std::move(completion),
      .callback = std::move(callback),
      .dispatcher = fdf_dispatcher_get_async_dispatcher(dispatcher),
  });

  lifecycle.v2.prepare_stop(
      driver,
      [](void* cookie, zx_status_t status) {
        auto* ctx = static_cast<Cookie*>(cookie);
        async::PostTask(ctx->dispatcher, [ctx, status]() {
          ctx->callback(zx::make_result(status));
          ctx->completion->Signal();
          delete ctx;
        });
      },
      cookie);
}

}  // namespace internal

DriverUnderTestBase::DriverUnderTestBase(fdf_dispatcher_t* driver_dispatcher,
                                         const DriverLifecycle& driver_lifecycle_symbol)
    : driver_dispatcher_(driver_dispatcher ? driver_dispatcher
                                           : fdf::Dispatcher::GetCurrent()->get()),
      driver_lifecycle_symbol_(driver_lifecycle_symbol),
      checker_(fdf_dispatcher_get_async_dispatcher(driver_dispatcher_)),
      prepare_stop_completer_(nullptr) {}

DriverUnderTestBase::~DriverUnderTestBase() {
  if (driver_.has_value()) {
    zx::result result = Stop();
    ZX_ASSERT_MSG(ZX_OK == result.status_value(), "Stop failed.");
  }
}

zx::result<std::shared_ptr<libsync::Completion>> DriverUnderTestBase::Start(
    fdf::DriverStartArgs start_args) {
  return StartWithErrorHandler(std::move(start_args), [](zx_status_t error) {
    ZX_ASSERT_MSG(false, "Driver start error: %s", zx_status_get_string(error));
  });
}

zx::result<std::shared_ptr<libsync::Completion>> DriverUnderTestBase::StartWithErrorHandler(
    fdf::DriverStartArgs start_args, fit::callback<void(zx_status_t error)> error_handler) {
  std::lock_guard guard(checker_);
  ZX_ASSERT_MSG(!driver_.has_value(), "Cannot start driver more than once.");

  fidl::OwnedEncodeResult encoded = fidl::StandaloneEncode(std::move(start_args));
  if (!encoded.message().ok()) {
    return zx::error(encoded.message().error().status());
  }

  fidl_opaque_wire_format_metadata_t wire_format_metadata =
      encoded.wire_format_metadata().ToOpaque();

  // We convert the outgoing message into an incoming message to provide to the
  // driver on start.
  fidl::OutgoingToEncodedMessage converted_message{encoded.message()};
  if (!converted_message.ok()) {
    return zx::error(converted_message.error().status());
  }

  // After calling |record_->start|, we assume it has taken ownership of
  // the handles from |start_args|, and can therefore relinquish ownership.
  fidl_incoming_msg_t c_msg = std::move(converted_message.message()).ReleaseToEncodedCMessage();
  EncodedFidlMessage msg{
      .bytes = static_cast<uint8_t*>(c_msg.bytes),
      .handles = c_msg.handles,
      .num_bytes = c_msg.num_bytes,
      .num_handles = c_msg.num_handles,
  };

  EncodedDriverStartArgs encoded_start_args{msg, wire_format_metadata};

  std::shared_ptr<libsync::Completion> completion = std::make_shared<libsync::Completion>();
  if (driver_lifecycle_symbol_.version >= 3 && driver_lifecycle_symbol_.v3.start != nullptr) {
    internal::StartDriverV3(
        driver_lifecycle_symbol_, encoded_start_args, driver_dispatcher_, completion,
        [this, error_handler =
                   std::move(error_handler)](zx::result<OpaqueDriverPtr> driver_result) mutable {
          std::lock_guard guard(checker_);
          if (driver_result.is_ok()) {
            driver_.emplace(driver_result.value());
          } else {
            error_handler(driver_result.status_value());
          }
        });
  } else {
    OpaqueDriverPtr out_driver = nullptr;
    zx_status_t status =
        driver_lifecycle_symbol_.v1.start(encoded_start_args, driver_dispatcher_, &out_driver);

    if (status == ZX_OK) {
      driver_.emplace(out_driver);
    } else {
      error_handler(status);
    }

    completion->Signal();
  }

  return zx::ok(completion);
}

std::shared_ptr<libsync::Completion> DriverUnderTestBase::PrepareStop() {
  return PrepareStopWithErrorHandler([](zx_status_t error) {
    ZX_ASSERT_MSG(false, "Driver prepare stop error: %s", zx_status_get_string(error));
  });
}

std::shared_ptr<libsync::Completion> DriverUnderTestBase::PrepareStopWithErrorHandler(
    fit::callback<void(zx_status_t error)> error_handler) {
  std::lock_guard guard(checker_);
  ZX_ASSERT_MSG(driver_.has_value(), "Driver does not exist.");

  std::shared_ptr<libsync::Completion> completion = std::make_shared<libsync::Completion>();

  // Store the completer so we can make sure it is done before calling stop.
  prepare_stop_completer_ = completion;

  if (driver_lifecycle_symbol_.version >= 2 &&
      driver_lifecycle_symbol_.v2.prepare_stop != nullptr) {
    internal::PrepareStopV2(
        driver_lifecycle_symbol_, driver_.value(), driver_dispatcher_, completion,
        [error_handler = std::move(error_handler)](zx::result<> result) mutable {
          if (result.is_error()) {
            error_handler(result.status_value());
          }
        });
  } else {
    completion->Signal();
  }

  return completion;
}

zx::result<> DriverUnderTestBase::Stop() {
  std::lock_guard guard(checker_);
  ZX_ASSERT_MSG(driver_.has_value(), "Driver does not exist.");
  ZX_ASSERT_MSG(prepare_stop_completer_, "PrepareStop must have been called before Stop.");
  ZX_ASSERT_MSG(prepare_stop_completer_->signaled(),
                "PrepareStop completion has not been signaled.");

  zx_status_t status = driver_lifecycle_symbol_.v1.stop(driver_.value());
  driver_.reset();
  return zx::make_result(status);
}

zx::result<OpaqueDriverPtr> StartDriver(fdf::DriverStartArgs start_args,
                                        fdf::TestSynchronizedDispatcher& driver_dispatcher,
                                        const DriverLifecycle& driver_lifecycle_symbol) {
  fidl::OwnedEncodeResult encoded = fidl::StandaloneEncode(std::move(start_args));
  if (!encoded.message().ok()) {
    return zx::error(encoded.message().error().status());
  }

  fidl_opaque_wire_format_metadata_t wire_format_metadata =
      encoded.wire_format_metadata().ToOpaque();

  // We convert the outgoing message into an incoming message to provide to the
  // driver on start.
  fidl::OutgoingToEncodedMessage converted_message{encoded.message()};
  if (!converted_message.ok()) {
    return zx::error(converted_message.error().status());
  }

  // After calling |record_->start|, we assume it has taken ownership of
  // the handles from |start_args|, and can therefore relinquish ownership.
  fidl_incoming_msg_t c_msg = std::move(converted_message.message()).ReleaseToEncodedCMessage();
  EncodedFidlMessage msg{
      .bytes = static_cast<uint8_t*>(c_msg.bytes),
      .handles = c_msg.handles,
      .num_bytes = c_msg.num_bytes,
      .num_handles = c_msg.num_handles,
  };
  EncodedDriverStartArgs encoded_start_args{msg, wire_format_metadata};

  OpaqueDriverPtr opaque;
  zx_status_t status = ZX_ERR_INTERNAL;

  std::shared_ptr<libsync::Completion> completion = std::make_shared<libsync::Completion>();
  zx::result result = fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(), [&]() {
    if (driver_lifecycle_symbol.version >= 3 && driver_lifecycle_symbol.v3.start != nullptr) {
      internal::StartDriverV3(driver_lifecycle_symbol, encoded_start_args,
                              driver_dispatcher.driver_dispatcher().get(), completion,
                              [&](zx::result<OpaqueDriverPtr> driver_result) {
                                if (driver_result.is_ok()) {
                                  opaque = driver_result.value();
                                  status = ZX_OK;
                                } else {
                                  status = driver_result.status_value();
                                }
                              });
    } else {
      status = driver_lifecycle_symbol.v1.start(
          encoded_start_args, driver_dispatcher.driver_dispatcher().get(), &opaque);
      completion->Signal();
    }
  });

  if (result.is_error()) {
    return result.take_error();
  }

  zx::result wait_result = fdf::WaitFor(*completion);
  if (wait_result.is_error()) {
    return wait_result.take_error();
  }

  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(opaque);
}

zx::result<> TeardownDriver(OpaqueDriverPtr driver,
                            fdf::TestSynchronizedDispatcher& driver_dispatcher,
                            const DriverLifecycle& driver_lifecycle_symbol) {
  if (driver_lifecycle_symbol.version >= 2) {
    std::shared_ptr<libsync::Completion> completion = std::make_shared<libsync::Completion>();

    zx::result result = fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(), [&]() {
      internal::PrepareStopV2(driver_lifecycle_symbol, driver,
                              driver_dispatcher.driver_dispatcher().get(), completion,
                              [](zx::result<>) {});
    });

    if (result.is_error()) {
      return result.take_error();
    }
    zx::result wait_result = fdf::WaitFor(*completion);
    if (wait_result.is_error()) {
      return wait_result.take_error();
    }
  }

  return fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(),
                                  [&]() { driver_lifecycle_symbol.v1.stop(driver); });
}

}  // namespace fdf_testing
