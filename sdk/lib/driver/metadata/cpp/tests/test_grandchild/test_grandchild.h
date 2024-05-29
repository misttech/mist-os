// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_METADATA_CPP_TESTS_TEST_GRANDCHILD_TEST_GRANDCHILD_H_
#define LIB_DRIVER_METADATA_CPP_TESTS_TEST_GRANDCHILD_TEST_GRANDCHILD_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.test/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/metadata/cpp/tests/fuchsia.hardware.test/metadata.h>

namespace fdf_metadata::test {

// This driver's purpose is to try to retrieve metadata from its parent node using
// `fdf::GetMetadata()`.
class TestGrandchild : public fdf::DriverBase,
                       public fidl::Server<fuchsia_hardware_test::Grandchild> {
 public:
  static constexpr std::string_view kDriverName = "test_grandchild";
  static constexpr std::string_view kChildNodeName = "grandchild";

  TestGrandchild(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  // fuchsia.hardware.test/Grandchild implementation.
  void GetMetadata(GetMetadataCompleter::Sync& completer) override;

 private:
  zx_status_t AddChild();

  void Serve(fidl::ServerEnd<fuchsia_hardware_test::Grandchild> request);

  fidl::SyncClient<fuchsia_driver_framework::Node> node_;
  fidl::ServerBindingGroup<fuchsia_hardware_test::Grandchild> bindings_;
  driver_devfs::Connector<fuchsia_hardware_test::Grandchild> devfs_connector_{
      fit::bind_member<&TestGrandchild::Serve>(this)};
  std::optional<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> child_node_controller_;
  std::optional<fidl::ClientEnd<fuchsia_driver_framework::Node>> child_node_;
};

}  // namespace fdf_metadata::test

#endif  // LIB_DRIVER_METADATA_CPP_TESTS_TEST_GRANDCHILD_TEST_GRANDCHILD_H_
