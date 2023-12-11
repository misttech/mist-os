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

void AddAndOfferConfig(component_testing::RealmBuilder& builder, const char* name,
                       fuchsia::component::decl::ConfigValue value) {
  fuchsia::component::decl::Component realm = builder.GetRealmDecl();

  fuchsia::component::decl::Configuration config;
  config.set_name(name).set_value(std::move(value));
  realm.mutable_capabilities()->push_back(
      fuchsia::component::decl::Capability::WithConfig(std::move(config)));

  fuchsia::component::decl::OfferConfiguration config_offer;
  config_offer
      .set_source(::fuchsia::component::decl::Ref::WithSelf(::fuchsia::component::decl::SelfRef()))
      .set_source_name(name)
      .set_target_name(name)
      .set_target(::fuchsia::component::decl::Ref::WithChild(
          ::fuchsia::component::decl::ChildRef{.name = "child"}));
  realm.mutable_offers()->push_back(
      fuchsia::component::decl::Offer::WithConfig(std::move(config_offer)));

  builder.ReplaceRealmDecl(std::move(realm));
}

void OfferConfigFromVoid(component_testing::RealmBuilder& builder, const char* name) {
  fuchsia::component::decl::Component realm = builder.GetRealmDecl();

  fuchsia::component::decl::OfferConfiguration config_offer;
  config_offer
      .set_source(
          ::fuchsia::component::decl::Ref::WithVoidType(::fuchsia::component::decl::VoidRef()))
      .set_availability(::fuchsia::component::decl::Availability::OPTIONAL)
      .set_source_name(name)
      .set_target_name(name)
      .set_target(::fuchsia::component::decl::Ref::WithChild(
          ::fuchsia::component::decl::ChildRef{.name = "child"}));
  realm.mutable_offers()->push_back(
      fuchsia::component::decl::Offer::WithConfig(std::move(config_offer)));

  builder.ReplaceRealmDecl(std::move(realm));
}

TEST(ScTest, CheckValuesVoidOptional) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  component_testing::RealmBuilder builder = component_testing::RealmBuilder::Create();
  AddChildComponent(builder);
  AddAndOfferConfig(builder, "fuchsia.config.MyFlag",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithBool_(true)));
  AddAndOfferConfig(builder, "fuchsia.config.MyTransitional",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithUint8(5)));
  OfferConfigFromVoid(builder, "fuchsia.config.MyInt");

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
  AddAndOfferConfig(builder, "fuchsia.config.MyFlag",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithBool_(true)));
  AddAndOfferConfig(builder, "fuchsia.config.MyTransitional",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithUint8(5)));

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
  AddAndOfferConfig(builder, "fuchsia.config.MyFlag",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithBool_(false)));
  AddAndOfferConfig(builder, "fuchsia.config.MyInt",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithUint8(10)));
  AddAndOfferConfig(builder, "fuchsia.config.MyTransitional",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithUint8(10)));

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
  AddAndOfferConfig(builder, "fuchsia.config.MyFlag",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithBool_(false)));
  AddAndOfferConfig(builder, "fuchsia.config.MyInt",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithUint8(10)));
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
  AddAndOfferConfig(builder, "fuchsia.config.MyFlag",
                    fuchsia::component::decl::ConfigValue::WithSingle(
                        fuchsia::component::decl::ConfigSingleValue::WithInt8(7)));

  component_testing::RealmRoot root = builder.Build(loop.dispatcher());
  zx::result client_channel = root.component().Connect<test_config::Config>();
  ASSERT_OK(client_channel);

  fidl::SyncClient client(std::move(client_channel.value()));
  fidl::Result result = client->Get();

  // This call should fail because the component cannot start with the wrong type.
  ASSERT_TRUE(result.is_error());
}

}  // namespace
