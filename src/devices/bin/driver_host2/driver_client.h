// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_HOST2_DRIVER_CLIENT_H_
#define SRC_DEVICES_BIN_DRIVER_HOST2_DRIVER_CLIENT_H_

#include <fidl/fuchsia.driver.framework/cpp/driver/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/natural_types.h>

#include <fbl/ref_ptr.h>

namespace dfv2 {

// Forward declare the Driver class.
class Driver;

// This class lives on the |client_dispatcher_| inside of the |Driver| class.
class DriverClient : public fdf::WireAsyncEventHandler<fuchsia_driver_framework::Driver> {
 public:
  DriverClient(fbl::RefPtr<Driver> driver, std::string_view url);

  void Bind(fdf::ClientEnd<fuchsia_driver_framework::Driver> client);

  void Start(fuchsia_driver_framework::DriverStartArgs start_args,
             fit::callback<void(zx::result<>)> callback);
  void Stop();

  // fdf::WireAsyncEventHandler<fuchsia_driver_framework::Driver>
  void on_fidl_error(fidl::UnbindInfo error) override;

  // fdf::WireAsyncEventHandler<fuchsia_driver_framework::Driver>
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::Driver> metadata) override;

 private:
  fbl::RefPtr<Driver> driver_;
  std::string url_;
  fdf::WireClient<fuchsia_driver_framework::Driver> driver_client_;
};

}  // namespace dfv2

#endif  // SRC_DEVICES_BIN_DRIVER_HOST2_DRIVER_CLIENT_H_
