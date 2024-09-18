// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "build_info.h"

#include <fuchsia/buildinfo/cpp/fidl.h>
#include <fuchsia/buildinfo/test/cpp/fidl.h>
#include <fuchsia/component/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>
#include <lib/sys/component/cpp/testing/realm_builder_types.h>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

namespace {
using component_testing::RealmBuilder;
using component_testing::RealmRoot;

using fuchsia::buildinfo::BuildInfo;
using fuchsia::buildinfo::Provider;
using fuchsia::buildinfo::test::BuildInfoTestController;

using component_testing::ChildRef;
using component_testing::ParentRef;
using component_testing::Protocol;
using component_testing::Route;
}  // namespace

class FakeBuildInfoTestFixture : public gtest::RealLoopFixture {
 public:
  static constexpr char fake_provider_url[] =
      "fuchsia-pkg://fuchsia.com/fake_build_info_test#meta/fake_build_info.cm";
  static constexpr char fake_provider_name[] = "fake_provider";

  static constexpr auto kProductName = "workstation";
  static constexpr auto kBoardName = "x64";
  static constexpr auto kVersion = "2022-03-28T15:42:20+00:00";
  static constexpr auto kPlatformVersion = "2024-03-28T15:42:20+00:00";
  static constexpr auto kProductVersion = "2024-04-28T15:42:20+00:00";
  static constexpr auto kLastCommitDate = "2022-03-28T15:42:20+00:00";

  FakeBuildInfoTestFixture()
      : realm_builder_(std::make_unique<RealmBuilder>(RealmBuilder::Create())) {}

  void SetUp() override {
    SetUpRealm(realm_builder_.get());

    realm_ = std::make_unique<RealmRoot>(realm_builder_->Build(dispatcher()));
  }

 protected:
  void SetUpRealm(RealmBuilder* builder) {
    realm_builder_->AddChild(fake_provider_name, fake_provider_url);

    realm_builder_->AddRoute(
        Route{.capabilities = {Protocol{Provider::Name_}, Protocol{BuildInfoTestController::Name_}},
              .source = ChildRef{fake_provider_name},
              .targets = {ParentRef()}});
  }

  RealmRoot* realm() { return realm_.get(); }

 private:
  std::unique_ptr<RealmRoot> realm_;
  std::unique_ptr<RealmBuilder> realm_builder_;
};

TEST_F(FakeBuildInfoTestFixture, SetBuildInfo) {
  auto provider = realm()->component().ConnectSync<Provider>();
  auto test_controller = realm()->component().ConnectSync<BuildInfoTestController>();

  BuildInfo result;
  provider->GetBuildInfo(&result);

  EXPECT_TRUE(result.has_product_config());
  EXPECT_EQ(result.product_config(), FakeProviderImpl::kProductNameDefault);
  EXPECT_TRUE(result.has_board_config());
  EXPECT_EQ(result.board_config(), FakeProviderImpl::kBoardNameDefault);
  EXPECT_TRUE(result.has_version());
  EXPECT_EQ(result.version(), FakeProviderImpl::kVersionDefault);
  EXPECT_TRUE(result.has_latest_commit_date());
  EXPECT_EQ(result.latest_commit_date(), FakeProviderImpl::kLastCommitDateDefault);

  auto build_info = BuildInfo();
  build_info.set_board_config(FakeBuildInfoTestFixture::kBoardName);
  build_info.set_product_config(FakeBuildInfoTestFixture::kProductName);
  build_info.set_version(FakeBuildInfoTestFixture::kVersion);
  build_info.set_platform_version(FakeBuildInfoTestFixture::kPlatformVersion);
  build_info.set_product_version(FakeBuildInfoTestFixture::kProductVersion);
  build_info.set_latest_commit_date(FakeBuildInfoTestFixture::kLastCommitDate);

  test_controller->SetBuildInfo(std::move(build_info));
  provider->GetBuildInfo(&result);

  EXPECT_TRUE(result.has_product_config());
  EXPECT_EQ(result.product_config(), FakeBuildInfoTestFixture::kProductName);
  EXPECT_TRUE(result.has_board_config());
  EXPECT_EQ(result.board_config(), FakeBuildInfoTestFixture::kBoardName);
  EXPECT_TRUE(result.has_version());
  EXPECT_EQ(result.version(), FakeBuildInfoTestFixture::kVersion);
  EXPECT_TRUE(result.has_platform_version());
  EXPECT_EQ(result.platform_version(), FakeBuildInfoTestFixture::kPlatformVersion);
  EXPECT_TRUE(result.has_product_version());
  EXPECT_EQ(result.product_version(), FakeBuildInfoTestFixture::kProductVersion);
  EXPECT_TRUE(result.has_latest_commit_date());
  EXPECT_EQ(result.latest_commit_date(), FakeBuildInfoTestFixture::kLastCommitDate);
}
