// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.devfs.test/cpp/wire.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/exporter.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace ft = fuchsia_devfs_test;

namespace {

class RootDriver : public fdf::DriverBase, public fidl::WireServer<ft::Device> {
  static constexpr std::string_view name = "root";

 public:
  RootDriver(fdf::DriverStartArgs start_args, fdf::UnownedDispatcher driver_dispatcher)
      : fdf::DriverBase(name, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    // Setup the outgoing directory.
    auto status = context().outgoing()->component().AddUnmanagedProtocol<ft::Device>(
        bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure), name);
    if (status.is_error()) {
      return status.take_error();
    }

    // Create the devfs exporter.
    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (endpoints.is_error()) {
      return zx::error(endpoints.status_value());
    }
    status = context().outgoing()->Serve(std::move(endpoints->server));
    if (status.is_error()) {
      return status.take_error();
    }
    auto exporter = fdf::DevfsExporter::Create(
        *context().incoming(), dispatcher(),
        fidl::WireSharedClient(std::move(endpoints->client), dispatcher()));
    if (exporter.is_error()) {
      return exporter.take_error();
    }
    exporter_ = std::move(*exporter);

    // Export "root-device" to devfs.
    exporter_.Export(std::string("svc/").append(name), "root-device", {}, 0,
                     [this](zx_status_t status) {
                       if (status != ZX_OK) {
                         UnbindNode(status);
                       }
                     });
    return zx::ok();
  }

 private:
  void UnbindNode(zx_status_t status) {
    FDF_LOG(ERROR, "Failed to start root driver: %s", zx_status_get_string(status));
    node().reset();
  }

  // fidl::WireServer<ft::Device>
  void Ping(PingCompleter::Sync& completer) override { completer.Reply(); }

  fidl::ServerBindingGroup<ft::Device> bindings_;
  fdf::DevfsExporter exporter_;
};

}  // namespace

FUCHSIA_DRIVER_RECORD_CPP_V3(fdf::Record<RootDriver>);
