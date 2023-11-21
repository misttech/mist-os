// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/settings/cpp/fidl.h>
#include <fuchsia/settings/test/cpp/fidl.h>
#include <fuchsia/testing/harness/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/sys/cpp/service_directory.h>

#include <zxtest/zxtest.h>

namespace {

class PrivacyTest : public zxtest::Test {
 protected:
  void SetUp() override {
    EXPECT_EQ(ZX_OK, service->Connect(realm_factory.NewRequest()));

    fuchsia::settings::test::RealmOptions options;
    fuchsia::settings::test::RealmFactory_CreateRealm_Result result1;
    EXPECT_OK(realm_factory->CreateRealm(std::move(options), realm_proxy.NewRequest(), &result1));

    fuchsia::testing::harness::RealmProxy_ConnectToNamedProtocol_Result result2;
    EXPECT_OK(realm_proxy->ConnectToNamedProtocol("fuchsia.settings.Privacy",
                                                  privacy.NewRequest().TakeChannel(), &result2));
  }

  void TearDown() override {
    fuchsia::settings::PrivacySettings settings;
    settings.set_user_data_sharing_consent(GetInitValue());
    fuchsia::settings::Privacy_Set_Result result;
    fuchsia::settings::PrivacySettings got_settings;
    // Set back to the initial settings and verify.
    EXPECT_EQ(ZX_OK, privacy->Set(std::move(settings), &result));
    EXPECT_EQ(ZX_OK, privacy->Watch(&got_settings));
    EXPECT_TRUE(got_settings.user_data_sharing_consent());
  }

  bool GetInitValue() const { return this->init_user_data_sharing_consent; }

  fuchsia::settings::PrivacySyncPtr privacy;

 private:
  fuchsia::testing::harness::RealmProxySyncPtr realm_proxy;
  fuchsia::settings::test::RealmFactorySyncPtr realm_factory;
  std::shared_ptr<sys::ServiceDirectory> service = sys::ServiceDirectory::CreateFromNamespace();
  bool init_user_data_sharing_consent = true;
};

// Tests that Set() results in an update to privacy settings.
TEST_F(PrivacyTest, Set) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  ASSERT_EQ(loop.StartThread(), ZX_OK);

  // Setup initial PrivacySettings.
  fuchsia::settings::PrivacySettings settings;
  fuchsia::settings::Privacy_Set_Result result;
  settings.set_user_data_sharing_consent(GetInitValue());
  EXPECT_EQ(ZX_OK, privacy->Set(std::move(settings), &result));

  // Verify initial settings.
  fuchsia::settings::PrivacySettings got_settings;
  EXPECT_EQ(ZX_OK, privacy->Watch(&got_settings));
  EXPECT_TRUE(got_settings.user_data_sharing_consent());

  // Flip the settings.
  fuchsia::settings::PrivacySettings new_settings;
  new_settings.set_user_data_sharing_consent(false);
  EXPECT_EQ(ZX_OK, privacy->Set(std::move(new_settings), &result));

  // Verify the new settings.
  EXPECT_EQ(ZX_OK, privacy->Watch(&got_settings));
  EXPECT_FALSE(got_settings.user_data_sharing_consent());
}

}  // namespace
