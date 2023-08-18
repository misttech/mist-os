// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <gtest/gtest.h>

#include "driver_logger_harness.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_logger_dfv2.h"

DriverLoggerHarness::~DriverLoggerHarness() {}

class DriverLoggerHarnessDFv2 : public DriverLoggerHarness {
 public:
  void Initialize();

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher test_env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment{
      test_env_dispatcher_->async_dispatcher(), std::in_place};

  std::unique_ptr<fdf::Logger> logger_;

  fit::deferred_callback logger_callback_;
};

void DriverLoggerHarnessDFv2::Initialize() {
  zx::result incoming_directory_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();

  ASSERT_TRUE(test_environment
                  .SyncCall(&fdf_testing::TestEnvironment::Initialize,
                            std::move(incoming_directory_endpoints->server))
                  .is_ok());

  auto entry_incoming = fuchsia_component_runner::ComponentNamespaceEntry(
      {.path = std::string("/"), .directory = std::move(incoming_directory_endpoints->client)});
  std::vector<fuchsia_component_runner::ComponentNamespaceEntry> incoming_namespace;
  incoming_namespace.push_back(std::move(entry_incoming));

  auto incoming = fdf::Namespace::Create(incoming_namespace);
  ASSERT_TRUE(incoming.is_ok()) << incoming.status_string();

  zx::result logger =
      fdf::Logger::Create(*incoming, fdf::Dispatcher::GetCurrent()->async_dispatcher(), "testing",
                          FUCHSIA_LOG_INFO, true);
  ASSERT_TRUE(logger.is_ok()) << logger.status_string();
  logger_ = std::move(*logger);
  logger_callback_ = magma::InitializePlatformLoggerForDFv2(logger_.get(), "mali");
}

// static
std::unique_ptr<DriverLoggerHarness> DriverLoggerHarness::Create() {
  auto harness = std::make_unique<DriverLoggerHarnessDFv2>();
  harness->Initialize();
  return std::move(harness);
}
