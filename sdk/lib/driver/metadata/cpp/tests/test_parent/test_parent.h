// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_METADATA_CPP_TESTS_TEST_PARENT_TEST_PARENT_H_
#define LIB_DRIVER_METADATA_CPP_TESTS_TEST_PARENT_TEST_PARENT_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/metadata/cpp/tests/fuchsia.hardware.test/metadata.h>

namespace fdf_metadata::test {

// This driver's purpose is to serve metadata to its two child nodes using `fdf::MetadataServer`.
class TestParent : public fdf::DriverBase, public fidl::Server<fuchsia_hardware_test::Parent> {
 public:
  static constexpr std::string_view kDriverName = "test_parent";
  static constexpr std::string_view kTestChildUseNodeName = "test_child_use";
  static constexpr std::string_view kTestChildNoUseNodeName = "test_child_no_use";

  TestParent(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  // fuchsia.hardware.test/Parent implementation.
  void SetMetadata(SetMetadataRequest& request, SetMetadataCompleter::Sync& completer) override;

 private:
  class TestChild {
   public:
    zx_status_t Init(
        std::string node_name, std::string test_child_property_value,
        fuchsia_hardware_test::MetadataServer& metadata_server,
        fidl::SyncClient<fuchsia_driver_framework::Node>& node, async_dispatcher_t* dispatcher,
        fit::function<void(fidl::ServerEnd<fuchsia_hardware_test::Parent>)> connect_callback);

   private:
    std::optional<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> controller_;
    std::optional<driver_devfs::Connector<fuchsia_hardware_test::Parent>> devfs_connector_;
  };

  void Serve(fidl::ServerEnd<fuchsia_hardware_test::Parent> request);

  fuchsia_hardware_test::MetadataServer metadata_server_;
  fidl::SyncClient<fuchsia_driver_framework::Node> node_;
  fidl::ServerBindingGroup<fuchsia_hardware_test::Parent> bindings_;
  TestChild test_child_no_use_;
  TestChild test_child_use_;
};

}  // namespace fdf_metadata::test

#endif  // LIB_DRIVER_METADATA_CPP_TESTS_TEST_PARENT_TEST_PARENT_H_
