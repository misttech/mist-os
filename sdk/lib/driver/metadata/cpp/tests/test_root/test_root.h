// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_METADATA_CPP_TESTS_TEST_ROOT_TEST_ROOT_H_
#define LIB_DRIVER_METADATA_CPP_TESTS_TEST_ROOT_TEST_ROOT_H_

#include <lib/driver/component/cpp/driver_base.h>

namespace fdf_metadata::test {

// This driver's purpose is to create two child nodes: one for the "test_parent_expose" driver to
// bind to and one for the "test_parent_no_expose" driver to bind to.
class TestRoot : public fdf::DriverBase {
 public:
  static constexpr std::string_view kDriverName = "test_root";
  static constexpr std::string_view kTestParentExposeNodeName = "test_parent_expose";
  static constexpr std::string_view kTestParentNoExposeNodeName = "test_parent_no_expose";

  TestRoot(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

 private:
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> AddTestParentNode(
      std::string node_name, std::string test_parent_property_value);

  fidl::SyncClient<fuchsia_driver_framework::Node> node_;
  std::optional<fidl::ClientEnd<fuchsia_driver_framework::NodeController>>
      test_parent_expose_controller_;
  std::optional<fidl::ClientEnd<fuchsia_driver_framework::NodeController>>
      test_parent_no_expose_controller_;
};

}  // namespace fdf_metadata::test

#endif  // LIB_DRIVER_METADATA_CPP_TESTS_TEST_ROOT_TEST_ROOT_H_
