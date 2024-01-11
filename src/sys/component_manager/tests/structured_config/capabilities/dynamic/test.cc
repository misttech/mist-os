// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/test.config/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sys/component/cpp/testing/realm_builder.h>

#include <zxtest/zxtest.h>

#include "src/sys/component_manager/tests/structured_config/capabilities/dynamic/config.h"

namespace {

void AddChildComponent(component_testing::RealmBuilder& builder) {
  builder.AddChild("child", "#meta/child.cm");
  builder.AddRoute(component_testing::Route{
      .capabilities = {component_testing::Capability{
          component_testing::Protocol{.name = "test.config.Config"}}},
      .source = component_testing::Ref{component_testing::ChildRef{.name = "child"}},
      .targets = {component_testing::ParentRef{}},
  });
}

TEST(ScTest, CheckValuesVoidOptional) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Bool(true),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyTransitional",
      .value = component_testing::ConfigValue::Uint8(5),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
              component_testing::Config{.name = "fuchsia.config.MyTransitional"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });
  builder.AddRoute(component_testing::Route{
      .capabilities = {component_testing::Config{.name = "fuchsia.config.MyInt"}},
      .source = component_testing::VoidRef(),
      .targets = {component_testing::ChildRef{"child"}},
  });

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  fidl::Result result = client->Get();
  ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());

  config::Config my_config = config::Config::CreateFromVmo(std::move(result->config()));
  ASSERT_EQ(my_config.my_flag(), true);
  ASSERT_EQ(my_config.transitional(), 5);
  // This value is coming from the CVF file since there is a void optional.
  ASSERT_EQ(my_config.my_int(), 0);
}

TEST(ScTest, CheckValuesNoOptional) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Bool(false),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyTransitional",
      .value = component_testing::ConfigValue::Uint8(5),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
              component_testing::Config{.name = "fuchsia.config.MyTransitional"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  fidl::Result result = client->Get();
  // This call should fail because 'fuchsia.config.MyInt' is not being routed.
  ASSERT_TRUE(result.is_error());
}

TEST(ScTest, CheckValues) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Bool(false),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyInt",
      .value = component_testing::ConfigValue::Uint8(10),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyTransitional",
      .value = component_testing::ConfigValue::Uint8(10),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
              component_testing::Config{.name = "fuchsia.config.MyInt"},
              component_testing::Config{.name = "fuchsia.config.MyTransitional"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  fidl::Result result = client->Get();
  ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());

  config::Config my_config = config::Config::CreateFromVmo(std::move(result->config()));
  ASSERT_EQ(my_config.my_flag(), false);
  ASSERT_EQ(my_config.my_int(), 10);
  ASSERT_EQ(my_config.transitional(), 10);
}

TEST(ScTest, NoTransitionalValue) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Bool(false),
  });
  configurations.push_back({
      .name = "fuchsia.config.MyInt",
      .value = component_testing::ConfigValue::Uint8(10),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
              component_testing::Config{.name = "fuchsia.config.MyInt"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });
  // We are specifically not routing fuchsia.config.MyTransitional.

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  fidl::Result result = client->Get();
  ASSERT_TRUE(result.is_ok(), "%s", result.error_value().FormatDescription().c_str());

  config::Config my_config = config::Config::CreateFromVmo(std::move(result->config()));
  ASSERT_EQ(my_config.my_flag(), false);
  ASSERT_EQ(my_config.my_int(), 10);
  // This value is coming from the CVF file.
  ASSERT_EQ(my_config.transitional(), 5);
}

TEST(ScTest, BadValueType) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  std::vector<component_testing::ConfigCapability> configurations;
  configurations.push_back({
      .name = "fuchsia.config.MyFlag",
      .value = component_testing::ConfigValue::Int8(7),
  });
  builder.AddConfiguration(std::move(configurations));
  builder.AddRoute({
      .capabilities =
          {
              component_testing::Config{.name = "fuchsia.config.MyFlag"},
          },
      .source = component_testing::SelfRef{},
      .targets = {component_testing::ChildRef{"child"}},
  });

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  fidl::Result result = client->Get();

  // This call should fail because the component cannot start with the wrong type.
  ASSERT_TRUE(result.is_error());
}

}  // namespace
