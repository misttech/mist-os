// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/driver_lifecycle.h>

#include <chrono>

#include "lib/fdf/testing.h"

namespace fdf_testing {

namespace internal {

void StartDriverV3(const DriverLifecycle& lifecycle, EncodedDriverStartArgs encoded_start_args,
                   fdf_dispatcher* dispatcher,
                   fit::callback<void(zx::result<OpaqueDriverPtr>)> callback) {
  struct Cookie {
    fit::callback<void(zx::result<OpaqueDriverPtr>)> callback;
    async_dispatcher_t* dispatcher;
  };
  Cookie* cookie = new Cookie({
      .callback = std::move(callback),
      .dispatcher = fdf_dispatcher_get_async_dispatcher(dispatcher),
  });

  lifecycle.v3.start(
      encoded_start_args, dispatcher,
      [](void* cookie, zx_status_t status, void* opaque) {
        auto* ctx = static_cast<Cookie*>(cookie);
        async::PostTask(ctx->dispatcher, [ctx, status, opaque]() {
          ctx->callback(zx::make_result(status, opaque));
          delete ctx;
        });
      },
      cookie);
}

void PrepareStopV2(const DriverLifecycle& lifecycle, void* driver, fdf_dispatcher* dispatcher,
                   fit::callback<void(zx::result<>)> callback) {
  struct Cookie {
    fit::callback<void(zx::result<>)> callback;
    async_dispatcher_t* dispatcher;
  };
  Cookie* cookie = new Cookie({
      .callback = std::move(callback),
      .dispatcher = fdf_dispatcher_get_async_dispatcher(dispatcher),
  });

  lifecycle.v2.prepare_stop(
      driver,
      [](void* cookie, zx_status_t status) {
        auto* ctx = static_cast<Cookie*>(cookie);
        async::PostTask(ctx->dispatcher, [ctx, status]() {
          ctx->callback(zx::make_result(status));
          delete ctx;
        });
      },
      cookie);
}

}  // namespace internal

DriverUnderTestBase::DriverUnderTestBase(DriverLifecycle driver_lifecycle_symbol)
    : driver_dispatcher_(fdf::Dispatcher::GetCurrent()->get()),
      driver_lifecycle_symbol_(driver_lifecycle_symbol),
      checker_(fdf_dispatcher_get_async_dispatcher(driver_dispatcher_)) {}

DriverUnderTestBase::~DriverUnderTestBase() {
  if (driver_.has_value()) {
    zx::result result = Stop();
    ZX_ASSERT_MSG(ZX_OK == result.status_value(), "Stop failed.");
  }
}

AsyncTask<zx::result<>> DriverUnderTestBase::Start(fdf::DriverStartArgs start_args) {
  std::lock_guard guard(checker_);
  ZX_ASSERT_MSG(!driver_.has_value(), "Cannot start driver more than once.");

  fidl::OwnedEncodeResult encoded = fidl::StandaloneEncode(std::move(start_args));
  ZX_ASSERT_MSG(ZX_OK == encoded.message().status(), "Failed to encode start_args: %s.",
                encoded.message().status_string());

  fidl_opaque_wire_format_metadata_t wire_format_metadata =
      encoded.wire_format_metadata().ToOpaque();

  // We convert the outgoing message into an incoming message to provide to the
  // driver on start.
  fidl::OutgoingToEncodedMessage converted_message{encoded.message()};
  ZX_ASSERT_MSG(ZX_OK == converted_message.status(), "Failed to convert to outgoing msg: %s.",
                converted_message.FormatDescription().c_str());

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

  ZX_ASSERT_MSG(!start_promise_.has_value(), "Start can only be called once.");
  start_promise_.emplace();

  if (driver_lifecycle_symbol_.version >= 3 && driver_lifecycle_symbol_.v3.start != nullptr) {
    internal::StartDriverV3(
        driver_lifecycle_symbol_, encoded_start_args, driver_dispatcher_,
        [this](zx::result<OpaqueDriverPtr> driver_result) mutable {
          std::lock_guard guard(checker_);
          driver_.emplace(driver_result);
          start_promise_->set_value(zx::make_result(driver_result.status_value()));
        });
  } else {
    OpaqueDriverPtr out_driver = nullptr;
    zx_status_t status =
        driver_lifecycle_symbol_.v1.start(encoded_start_args, driver_dispatcher_, &out_driver);

    driver_.emplace(zx::make_result(status, out_driver));
    start_promise_->set_value(zx::make_result(status));
  }

  return AsyncTask<zx::result<>>(start_promise_->get_future().share());
}

AsyncTask<zx::result<>> DriverUnderTestBase::PrepareStop() {
  std::lock_guard guard(checker_);
  ZX_ASSERT_MSG(driver_.has_value(), "Driver does not exist.");
  ZX_ASSERT_MSG(driver_.value().is_ok(), "Driver start did not succeed: %s.",
                driver_.value().status_string());
  ZX_ASSERT_MSG(!prepare_stop_promise_.has_value(), "PrepareStop can only be called once.");
  prepare_stop_promise_.emplace();
  prepare_stop_promise_future_ = prepare_stop_promise_->get_future().share();

  if (driver_lifecycle_symbol_.version >= 2 &&
      driver_lifecycle_symbol_.v2.prepare_stop != nullptr) {
    internal::PrepareStopV2(driver_lifecycle_symbol_, driver_.value().value(), driver_dispatcher_,
                            [this](zx::result<> result) mutable {
                              std::lock_guard guard(checker_);
                              prepare_stop_promise_->set_value(result);
                            });
  } else {
    prepare_stop_promise_->set_value(zx::ok());
  }

  return AsyncTask<zx::result<>>(prepare_stop_promise_future_);
}

zx::result<> DriverUnderTestBase::Stop() {
  std::lock_guard guard(checker_);
  ZX_ASSERT_MSG(driver_.has_value(), "Driver does not exist.");
  ZX_ASSERT_MSG(driver_.value().is_ok(), "Driver start did not succeed: %s.",
                driver_.value().status_string());
  ZX_ASSERT_MSG(prepare_stop_promise_future_.valid(),
                "PrepareStop must have been called before Stop.");
  ZX_ASSERT_MSG(
      prepare_stop_promise_future_.wait_for(std::chrono::seconds(0)) == std::future_status::ready,
      "PrepareStop has not completed.");

  zx_status_t status = driver_lifecycle_symbol_.v1.stop(driver_.value().value());
  driver_.reset();
  return zx::make_result(status);
}

void* DriverUnderTestBase::GetDriver() {
  std::lock_guard guard(checker_);
  ZX_ASSERT_MSG(driver_.has_value(), "Driver does not exist.");
  ZX_ASSERT_MSG(driver_.value().is_ok(), "Driver start did not succeed: %s.",
                driver_.value().status_string());
  return driver_.value().value();
}

}  // namespace fdf_testing
