// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_METADATA_CPP_TESTS_METADATA_RETRIEVER_TEST_DRIVER_METADATA_RETRIEVER_TEST_DRIVER_H_
#define LIB_DRIVER_METADATA_CPP_TESTS_METADATA_RETRIEVER_TEST_DRIVER_METADATA_RETRIEVER_TEST_DRIVER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.test/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/node/cpp/add_child.h>

namespace fdf_metadata::test {

// This driver's purpose is to try to retrieve metadata from its parent node using
// `fdf::GetMetadata()`.
class MetadataRetrieverTestDriver : public fdf::DriverBase,
                                    public fidl::Server<fuchsia_hardware_test::MetadataRetriever> {
 public:
  static constexpr std::string_view kDriverName = "metadata_retriever";
  static constexpr std::string_view kControllerNodeName = "controller";

  MetadataRetrieverTestDriver(fdf::DriverStartArgs start_args,
                              fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  // fuchsia.hardware.test/MetadataRetriever implementation.
  void GetMetadata(GetMetadataCompleter::Sync& completer) override;
  void GetMetadataIfExists(GetMetadataIfExistsCompleter::Sync& completer) override;

 private:
  void Serve(fidl::ServerEnd<fuchsia_hardware_test::MetadataRetriever> request);

  // Create a non-bindable child node whose purpose is to expose the
  // fuchsia.hardware.test/MetadataRetriever FIDL protocol served by this to devfs.
  zx_status_t InitControllerNode();

  driver_devfs::Connector<fuchsia_hardware_test::MetadataRetriever> devfs_connector_{
      fit::bind_member<&MetadataRetrieverTestDriver::Serve>(this)};
  fidl::ServerBindingGroup<fuchsia_hardware_test::MetadataRetriever> bindings_;

  std::optional<fdf::OwnedChildNode> controller_node_;
};

}  // namespace fdf_metadata::test

#endif  // LIB_DRIVER_METADATA_CPP_TESTS_METADATA_RETRIEVER_TEST_DRIVER_METADATA_RETRIEVER_TEST_DRIVER_H_
