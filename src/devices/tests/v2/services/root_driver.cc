// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.services.test/cpp/wire.h>
#include <lib/driver/component/cpp/driver_cpp.h>

namespace ft = fuchsia_services_test;

namespace {

class RootDriver : public fdf::DriverBase,
                   public fidl::WireServer<ft::ControlPlane>,
                   public fidl::WireServer<ft::DataPlane> {
 public:
  RootDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("root", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    auto control = [this](fidl::ServerEnd<ft::ControlPlane> server_end) -> void {
      fidl::BindServer(dispatcher(), std::move(server_end), this);
    };
    auto data = [this](fidl::ServerEnd<ft::DataPlane> server_end) -> void {
      fidl::BindServer(dispatcher(), std::move(server_end), this);
    };
    ft::Device::InstanceHandler handler({.control = std::move(control), .data = std::move(data)});

    auto result = outgoing()->AddService<ft::Device>(std::move(handler));
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add Device service: %s", result.status_string());
      return result.take_error();
    }
    return zx::ok();
  }

 private:
  // fidl::WireServer<ft::ControlPlane>
  void ControlDo(ControlDoCompleter::Sync& completer) override { completer.Reply(); }

  // fidl::WireServer<ft::DataPlane>
  void DataDo(DataDoCompleter::Sync& completer) override { completer.Reply(); }
};

}  // namespace

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<RootDriver>);
