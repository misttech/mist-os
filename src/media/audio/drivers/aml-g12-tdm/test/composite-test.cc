// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdio/directory.h>

#include <zxtest/zxtest.h>

namespace audio::aml_g12 {

struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};
};

class AmlG12CompositeTest : public zxtest::Test {
 public:
  AmlG12CompositeTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        driver_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(env_dispatcher(), std::in_place) {}

  void SetUp() override {
    fuchsia_driver_framework::DriverStartArgs driver_start_args;
    incoming_.SyncCall([&driver_start_args](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      driver_start_args = std::move(start_args_result->start_args);
    });

    zx::result result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::DriverUnderTest<Driver>::Start, std::move(driver_start_args)));
    ASSERT_EQ(ZX_OK, result.status_value());

    incoming_.SyncCall([this](IncomingNamespace* incoming) {
      auto client_channel = incoming->node_.children().at(kDriverName).ConnectToDevice();
      client_.Bind(
          fidl::ClientEnd<fuchsia_hardware_audio::Composite>(std::move(client_channel.value())));
      ASSERT_TRUE(client_.is_valid());
    });
  }

  void TearDown() override {
    zx::result result =
        runtime_.RunToCompletion(dut_.SyncCall(&fdf_testing::DriverUnderTest<Driver>::PrepareStop));
    ASSERT_EQ(ZX_OK, result.status_value());
  }

 protected:
  fidl::SyncClient<fuchsia_hardware_audio::Composite> client_;

 private:
  async_dispatcher_t* driver_dispatcher() { return driver_dispatcher_->async_dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  // Use dut_ instead of driver_ because driver_ is used by zxtest.
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<Driver>> dut_{
      driver_dispatcher(), std::in_place};
};

TEST_F(AmlG12CompositeTest, CompositeProperties) {
  fidl::Result result = client_->GetProperties();
  ASSERT_TRUE(result.is_ok());
  // TODO(fxbug.dev/132252): Implement audio-composite tests.
  ASSERT_FALSE(result->properties().clock_domain().has_value());
}

}  // namespace audio::aml_g12
