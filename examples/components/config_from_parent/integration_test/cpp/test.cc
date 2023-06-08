// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.diagnostics/cpp/fidl.h>
#include <fuchsia/diagnostics/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/inspect/contrib/cpp/archive_reader.h>
#include <lib/syslog/cpp/macros.h>

#include <string>

#include <rapidjson/document.h>
#include <rapidjson/pointer.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

using inspect::contrib::InspectData;

constexpr char kChildUrl[] = "#meta/config_example.cm";
constexpr char kCollectionName[] = "realm_api_collection";

class IntegrationTest : public gtest::RealLoopFixture {
 protected:
  InspectData GetInspect(const std::string& child_name) {
    zx::result client_end = component::Connect<fuchsia_diagnostics::ArchiveAccessor>();
    ZX_ASSERT(client_end.is_ok());
    fuchsia::diagnostics::ArchiveAccessorPtr archive;
    archive.Bind(client_end->TakeChannel(), dispatcher());

    std::stringstream selector;
    selector << kCollectionName << "\\:" << child_name;
    std::string child_with_collection = selector.str();
    selector << ":root";

    inspect::contrib::ArchiveReader reader(std::move(archive), {selector.str()});
    fpromise::result<std::vector<InspectData>, std::string> result;
    async::Executor executor(dispatcher());
    executor.schedule_task(
        reader.SnapshotInspectUntilPresent({child_with_collection})
            .then([&](fpromise::result<std::vector<InspectData>, std::string>& rest) {
              result = std::move(rest);
            }));
    RunLoopUntil([&] { return result.is_ok() || result.is_error(); });

    EXPECT_EQ(result.is_error(), false) << "Error was " << result.error();
    EXPECT_EQ(result.value().size(), 1ul) << "Expected only one component";

    return std::move(result.value()[0]);
  }
};

TEST_F(IntegrationTest, ParentValuesObserved) {
  std::string child_name = "dynamic_child_realm_api_parent_values";
  std::string expected_greeting = "parent component";
  zx::result client_end = component::Connect<fuchsia_component::Realm>();
  ZX_ASSERT(client_end.is_ok());
  fidl::Client realm(std::move(*client_end), dispatcher());
  ZX_ASSERT(realm.is_valid());

  fuchsia_component_decl::Child child_decl;
  child_decl.name(child_name);
  child_decl.url(kChildUrl);
  child_decl.startup(fuchsia_component_decl::StartupMode::kLazy);

  fuchsia_component_decl::ConfigOverride greeting_override;
  greeting_override.key("greeting");
  greeting_override.value(fuchsia_component_decl::ConfigValue::WithSingle(
      fuchsia_component_decl::ConfigSingleValue::WithString(expected_greeting)));
  child_decl.config_overrides({{greeting_override}});

  fuchsia_component_decl::CollectionRef collection;
  collection.name(kCollectionName);

  fuchsia_component::CreateChildArgs child_args;

  realm->CreateChild({collection, child_decl, std::move(child_args)})
      .ThenExactlyOnce([this](fidl::Result<fuchsia_component::Realm::CreateChild>& result) {
        if (!result.is_ok()) {
          FX_LOGS(ERROR) << "CreateChild failed: " << result.error_value();
          ZX_PANIC("%s", result.error_value().FormatDescription().c_str());
        }
        QuitLoop();
      });
  RunLoop();

  fuchsia_component_decl::ChildRef child_ref;
  child_ref.collection(kCollectionName);
  child_ref.name(child_name);

  auto exposed_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ZX_ASSERT(!exposed_endpoints.is_error());
  auto exposed_client = std::move(exposed_endpoints->client);

  realm->OpenExposedDir({child_ref, std::move(exposed_endpoints->server)})
      .ThenExactlyOnce([this](fidl::Result<fuchsia_component::Realm::OpenExposedDir>& result) {
        if (!result.is_ok()) {
          FX_LOGS(ERROR) << "OpenExposedDir failed: " << result.error_value();
          ZX_PANIC("%s", result.error_value().FormatDescription().c_str());
        }
        QuitLoop();
      });
  RunLoop();

  zx::result bind_result = component::ConnectAt<fuchsia_component::Binder>(exposed_client);
  if (!bind_result.is_ok()) {
    FX_LOGS(ERROR) << "Opening fuchsia.component.Binder failed: " << bind_result.error_value();
  }

  auto data = GetInspect(child_name);

  EXPECT_EQ(expected_greeting, data.GetByPath({"root", "config", "greeting"}).GetString());
}

// This test ensures that the test above is passing because parent overrides work rather than
// lucking into the same value as the packaged config.
TEST_F(IntegrationTest, DefaultValuesObserved) {
  std::string child_name = "dynamic_child_realm_api_default_values";
  std::string expected_greeting = "World!";
  zx::result client_end = component::Connect<fuchsia_component::Realm>();
  ZX_ASSERT(client_end.is_ok());
  fidl::Client realm(std::move(*client_end), dispatcher());
  ZX_ASSERT(realm.is_valid());

  fuchsia_component_decl::Child child_decl;
  child_decl.name(child_name);
  child_decl.url(kChildUrl);
  child_decl.startup(fuchsia_component_decl::StartupMode::kLazy);

  fuchsia_component_decl::CollectionRef collection;
  collection.name(kCollectionName);

  fuchsia_component::CreateChildArgs child_args;

  realm->CreateChild({collection, child_decl, std::move(child_args)})
      .ThenExactlyOnce([this](fidl::Result<fuchsia_component::Realm::CreateChild>& result) {
        if (!result.is_ok()) {
          FX_LOGS(ERROR) << "CreateChild failed: " << result.error_value();
          ZX_PANIC("%s", result.error_value().FormatDescription().c_str());
        }
        QuitLoop();
      });
  RunLoop();

  fuchsia_component_decl::ChildRef child_ref;
  child_ref.collection(kCollectionName);
  child_ref.name(child_name);

  auto exposed_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ZX_ASSERT(!exposed_endpoints.is_error());
  auto exposed_client = std::move(exposed_endpoints->client);

  realm->OpenExposedDir({child_ref, std::move(exposed_endpoints->server)})
      .ThenExactlyOnce([this](fidl::Result<fuchsia_component::Realm::OpenExposedDir>& result) {
        if (!result.is_ok()) {
          FX_LOGS(ERROR) << "OpenExposedDir failed: " << result.error_value();
          ZX_PANIC("%s", result.error_value().FormatDescription().c_str());
        }
        QuitLoop();
      });
  RunLoop();

  zx::result bind_result = component::ConnectAt<fuchsia_component::Binder>(exposed_client);
  if (!bind_result.is_ok()) {
    FX_LOGS(ERROR) << "Opening fuchsia.component.Binder failed: " << bind_result.error_value();
  }

  auto data = GetInspect(child_name);

  EXPECT_EQ(expected_greeting, data.GetByPath({"root", "config", "greeting"}).GetString());
}
