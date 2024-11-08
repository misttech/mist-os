// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_TESTS_UTILS_SCENIC_CTF_TEST_BASE_H_
#define SRC_UI_SCENIC_TESTS_UTILS_SCENIC_CTF_TEST_BASE_H_

#include <fuchsia/testing/harness/cpp/fidl.h>
#include <fuchsia/ui/test/context/cpp/fidl.h>
#include <lib/fidl/cpp/interface_handle.h>
#include <lib/fidl/cpp/synchronous_interface_ptr.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/zx/channel.h>

#include <cstdint>
#include <cstdlib>
#include <iostream>

#include <zxtest/zxtest.h>

#include "src/ui/testing/util/logging_event_loop.h"

namespace integration_tests {

/// ScenicCtfTest use realm_proxy to connect scenic test realm.
/// The scenic test realm consists of three components:
///   * Scenic
///   * Fake Cobalt
///   * Fake Display Provider
///
/// topology as follows:
///       test_manager
///            |
///     <test component>
///            |                  <- Test realm
/// ----------------------------  <- realm_proxy
///     /      |     \            <- Scenic realm
///  Scenic  Cobalt  Hdcp
class ScenicCtfTest : public zxtest::Test, public ui_testing::LoggingEventLoop {
 public:
  ScenicCtfTest() = default;
  ~ScenicCtfTest() override = default;

  /// SetUp connect test realm so test can use realm_proxy_ to access.
  void SetUp() override;

  const std::shared_ptr<sys::ServiceDirectory>& LocalServiceDirectory() const;

  /// Override DisplayRotation() to provide fuchsia.scenic.DisplayRotation to test realm. By
  /// default, it returns 0.
  virtual uint64_t DisplayRotation() const;

  /// Override Renderer() to provide fuchsia.scenic.Renderer to test realm. By default, it returns
  /// "vulkan".
  virtual fuchsia::ui::test::context::RendererType Renderer() const;

  /// Override DisplayComposition() to provide fuchsia.scenic.DisplayComposition to test realm. True
  /// by default.
  virtual bool DisplayComposition() const;

  /// Connect to the FIDL protocol which served from the realm proxy use default served path if no
  /// name passed in.
  template <typename Interface>
  fidl::SynchronousInterfacePtr<Interface> ConnectSyncIntoRealm(
      const std::string& service_path = Interface::Name_) {
    fidl::SynchronousInterfacePtr<Interface> ptr;

    fuchsia::testing::harness::RealmProxy_ConnectToNamedProtocol_Result result;
    if (realm_proxy_->ConnectToNamedProtocol(service_path, ptr.NewRequest().TakeChannel(),
                                             &result) != ZX_OK) {
      std::cerr << "ConnectToNamedProtocol(" << service_path << ", " << Interface::Name_
                << ") failed." << std::endl;
      std::abort();
    }
    return std::move(ptr);
  }

  /// Connect to the FIDL protocol which served from the realm proxy use default served path if no
  /// name passed in.
  template <typename Interface>
  fidl::InterfacePtr<Interface> ConnectAsyncIntoRealm(
      const std::string& service_path = Interface::Name_) {
    fidl::InterfacePtr<Interface> ptr;

    fuchsia::testing::harness::RealmProxy_ConnectToNamedProtocol_Result result;
    if (realm_proxy_->ConnectToNamedProtocol(service_path, ptr.NewRequest().TakeChannel(),
                                             &result) != ZX_OK) {
      std::cerr << "ConnectToNamedProtocol(" << service_path << ", " << Interface::Name_
                << ") failed." << std::endl;
      std::abort();
    }
    return std::move(ptr);
  }

 private:
  fuchsia::ui::test::context::ScenicRealmFactorySyncPtr realm_factory_;
  fuchsia::testing::harness::RealmProxySyncPtr realm_proxy_;
  std::unique_ptr<sys::ComponentContext> context_;
};

}  // namespace integration_tests

#endif  // SRC_UI_SCENIC_TESTS_UTILS_SCENIC_CTF_TEST_BASE_H_
