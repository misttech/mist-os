// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_TESTS_TEST_DRIVER_H_
#define LIB_DRIVER_COMPONENT_CPP_TESTS_TEST_DRIVER_H_

#include <fidl/fuchsia.driver.component.test/cpp/driver/wire.h>
#include <fidl/fuchsia.driver.component.test/cpp/wire.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/connector.h>

class TestDriver : public fdf::DriverBase,
                   public fidl::WireServer<fuchsia_driver_component_test::ZirconProtocol>,
                   public fdf::WireServer<fuchsia_driver_component_test::DriverProtocol> {
 public:
  TestDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("test_driver", std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(fit::bind_member<&TestDriver::Connect>(this)) {}

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  zx::result<> ExportDevfsNodeSync();
  zx::result<> ServeDriverService();
  zx::result<> ServeZirconService();

  zx::result<> ValidateIncomingDriverService();
  zx::result<> ValidateIncomingZirconService();

  void CreateChildNodeSync();

  void CreateChildNodeAsync();

  bool async_added_child() const { return async_added_child_; }
  bool sync_added_child() const { return sync_added_child_; }

 private:
  // fidl::WireServer<fuchsia_driver_component_test::ZirconProtocol>
  void ZirconMethod(ZirconMethodCompleter::Sync& completer) override { completer.ReplySuccess(); }

  // fdf::WireServer<fuchsia_driver_component_test::DriverProtocol>
  void DriverMethod(fdf::Arena& arena, DriverMethodCompleter::Sync& completer) override {
    fdf::Arena reply_arena('DRVR');
    completer.buffer(reply_arena).ReplySuccess();
  }

  // driver_devfs::Connector<fuchsia_driver_component_test::ZirconProtocol>
  void Connect(fidl::ServerEnd<fuchsia_driver_component_test::ZirconProtocol> request) {
    zircon_bindings_.AddBinding(dispatcher(), std::move(request), this,
                                fidl::kIgnoreBindingClosure);
  }

  fuchsia_driver_component_test::ZirconService::InstanceHandler GetInstanceHandlerZircon() {
    return fuchsia_driver_component_test::ZirconService::InstanceHandler({
        .device = zircon_bindings_.CreateHandler(
            this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
    });
  }

  fuchsia_driver_component_test::DriverService::InstanceHandler GetInstanceHandlerDriver() {
    return fuchsia_driver_component_test::DriverService::InstanceHandler({
        .device = driver_bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                                 fidl::kIgnoreBindingClosure),
    });
  }

  fidl::WireClient<fuchsia_driver_framework::Node> node_client_;
  bool async_added_child_ = false;
  bool sync_added_child_ = false;

  fidl::ServerBindingGroup<fuchsia_driver_component_test::ZirconProtocol> zircon_bindings_;
  driver_devfs::Connector<fuchsia_driver_component_test::ZirconProtocol> devfs_connector_;

  fdf::ServerBindingGroup<fuchsia_driver_component_test::DriverProtocol> driver_bindings_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> devfs_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> devfs_node_controller_;
};

#endif  // LIB_DRIVER_COMPONENT_CPP_TESTS_TEST_DRIVER_H_
