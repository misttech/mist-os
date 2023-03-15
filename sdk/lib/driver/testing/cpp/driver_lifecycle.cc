// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/driver_lifecycle.h>

#include "lib/fdf/testing.h"

namespace fdf_testing {

namespace internal {

zx::result<> PrepareStopSync(OpaqueDriverPtr driver, const DriverLifecycle& driver_lifecycle_symbol,
                             fdf::TestSynchronizedDispatcher& driver_dispatcher) {
  libsync::Completion prepare_stop_completion;

  zx::result result = fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(), [&]() {
    driver_lifecycle_symbol.v2.prepare_stop(
        driver,
        [](void* cookie, zx_status_t status) {
          static_cast<libsync::Completion*>(cookie)->Signal();
        },
        &prepare_stop_completion);
  });

  if (result.is_error()) {
    return result.take_error();
  }

  return fdf::WaitFor(prepare_stop_completion);
}

}  // namespace internal

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
  OpaqueDriverPtr opaque = nullptr;

  zx_status_t status = ZX_ERR_INTERNAL;
  zx::result result = fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(), [&]() {
    status = driver_lifecycle_symbol.v1.start({&c_msg, wire_format_metadata},
                                              driver_dispatcher.driver_dispatcher().get(), &opaque);
  });

  if (result.is_error()) {
    return result.take_error();
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
    zx::result result =
        internal::PrepareStopSync(driver, driver_lifecycle_symbol, driver_dispatcher);
    if (result.is_error()) {
      return result.take_error();
    }
  }

  zx::result result = fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(),
                                               [&]() { driver_lifecycle_symbol.v1.stop(driver); });

  if (result.is_error()) {
    return result.take_error();
  }

  result = driver_dispatcher.Stop();
  return result;
}

}  // namespace fdf_testing
