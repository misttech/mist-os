// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/cobalt/bin/app/system_data_updater_impl.h"

#include <lib/fidl/cpp/binding_set.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/testing/cpp/inspect.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <lib/syslog/cpp/macros.h>

#include "fuchsia/cobalt/cpp/fidl.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace cobalt {

using fuchsia::cobalt::SoftwareDistributionInfo;
using FuchsiaStatus = fuchsia::cobalt::Status;
using fuchsia::cobalt::SystemDataUpdaterPtr;

class CobaltAppForTest {
 public:
  explicit CobaltAppForTest(std::unique_ptr<sys::ComponentContext> context)
      : context_(std::move(context)) {
    context_->outgoing()->AddPublicService(
        system_data_updater_bindings_.GetHandler(&system_data_updater_impl_));
  }

  const inspect::Inspector& inspector() { return inspector_; }

 private:
  inspect::Inspector inspector_;

  std::unique_ptr<sys::ComponentContext> context_;

  SystemDataUpdaterImpl system_data_updater_impl_;
  fidl::BindingSet<fuchsia::cobalt::SystemDataUpdater> system_data_updater_bindings_;
};

class SystemDataUpdaterImplTests : public gtest::TestLoopFixture {
 public:
  void SetUp() override {
    TestLoopFixture::SetUp();
    cobalt_app_.reset(new CobaltAppForTest(context_provider_.TakeContext()));
  }

  void TearDown() override {
    cobalt_app_.reset();
    TestLoopFixture::TearDown();
  }

 protected:
  SystemDataUpdaterPtr GetSystemDataUpdater() {
    SystemDataUpdaterPtr system_data_updater;
    context_provider_.ConnectToPublicService(system_data_updater.NewRequest());
    return system_data_updater;
  }

 private:
  sys::testing::ComponentContextProvider context_provider_;
  std::unique_ptr<CobaltAppForTest> cobalt_app_;
};

TEST_F(SystemDataUpdaterImplTests, ReturnsOk) {
  SystemDataUpdaterPtr system_data_updater = GetSystemDataUpdater();
  SoftwareDistributionInfo info = SoftwareDistributionInfo();

  bool called = false;
  system_data_updater->SetSoftwareDistributionInfo(std::move(info), [&called](FuchsiaStatus s) {
    called = true;
    EXPECT_EQ(s, FuchsiaStatus::OK);
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(called);
}

}  // namespace cobalt
