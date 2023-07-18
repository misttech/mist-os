// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v1/driver_loader.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <tuple>

#include <zxtest/zxtest.h>

#include "fbl/ref_ptr.h"
#include "src/devices/bin/driver_manager/v1/driver.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf
namespace fdi = fuchsia_driver_index;

class FakeResolver : public internal::PackageResolverInterface {
 public:
  zx::result<std::unique_ptr<Driver>> FetchDriver(const std::string& package_url) override {
    if (map.count(package_url) != 0) {
      auto driver = std::move(map[package_url]);
      map.erase(package_url);
      return zx::ok(std::move(driver));
    }
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  std::map<std::string, std::unique_ptr<Driver>> map;
};

class FakeDriverLoaderIndex final : public fidl::WireServer<fdi::DriverIndex> {
 public:
  void MatchDriver(MatchDriverRequestView request, MatchDriverCompleter::Sync& completer) override {
    if (!driver.has_value()) {
      completer.ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }
    completer.ReplySuccess(driver.value());
  }

  void WaitForBaseDrivers(WaitForBaseDriversCompleter::Sync& completer) override {
    completer.Reply();
  }

  void AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request,
                            AddCompositeNodeSpecCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  std::optional<fdi::wire::MatchedDriver> driver;
};

class DriverLoaderTest : public zxtest::Test {
 public:
  void SetUp() override {
    auto endpoints = fidl::CreateEndpoints<fdi::DriverIndex>();
    ASSERT_FALSE(endpoints.is_error());
    fidl::BindServer(loop.dispatcher(), std::move(endpoints->server), &driver_index_server);
    driver_index =
        fidl::WireSharedClient<fdi::DriverIndex>(std::move(endpoints->client), loop.dispatcher());
  }

  void TearDown() override {}

  async::Loop loop = async::Loop(&kAsyncLoopConfigNeverAttachToThread);
  FakeDriverLoaderIndex driver_index_server;
  FakeResolver resolver;
  FakeResolver universe_resolver;
  fidl::WireSharedClient<fdi::DriverIndex> driver_index;
};

TEST_F(DriverLoaderTest, TestUrl) {
  std::string name = "fuchsia-boot:///#driver1.cm";

  fidl::Arena arena;
  auto driver_info = fdi::wire::MatchedDriverInfo::Builder(arena).url(name).is_fallback(false);
  driver_index_server.driver = fdi::wire::MatchedDriver::WithDriver(arena, driver_info.Build());

  std::unique_ptr driver = std::make_unique<Driver>();
  driver->url = name;
  resolver.map[name] = std::move(driver);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 1);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).component_url, name);
}

TEST_F(DriverLoaderTest, TestRelativeUrl) {
  std::string name = "fuchsia-boot:///#driver.cm";

  fidl::Arena arena;
  auto driver_info = fdi::wire::MatchedDriverInfo::Builder(arena).url(name).is_fallback(false);
  driver_index_server.driver = fdi::wire::MatchedDriver::WithDriver(arena, driver_info.Build());

  std::unique_ptr driver = std::make_unique<Driver>();
  driver->url = name;
  resolver.map[name] = std::move(driver);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             nullptr);
  loop.StartThread("fidl-thread");

  {
    DriverLoader::MatchDeviceConfig config;
    config.driver_url_suffix = "driver.cm";
    fidl::VectorView<fdf::wire::NodeProperty> props{};
    auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

    ASSERT_EQ(1, drivers.size());
    ASSERT_EQ(name, std::get<MatchedDriverInfo>(drivers[0]).component_url);
  }

  {
    DriverLoader::MatchDeviceConfig config;
    config.driver_url_suffix = "driver2.cm";
    fidl::VectorView<fdf::wire::NodeProperty> props{};
    auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

    ASSERT_EQ(0, drivers.size());
  }
}

TEST_F(DriverLoaderTest, TestTooLongRelativeUrl) {
  std::string name = "fuchsia-boot:///#driver1.cm";
  // The characters of `url` do not matter so long as the size of `url`
  // is longer than `name1`.
  std::string long_name = std::string(name.length() + 1, 'a');

  fidl::Arena arena;
  auto driver_info = fdi::wire::MatchedDriverInfo::Builder(arena).url(name).is_fallback(false);
  driver_index_server.driver = fdi::wire::MatchedDriver::WithDriver(arena, driver_info.Build());

  auto driver = std::make_unique<Driver>();
  driver->url = name;
  resolver.map[name] = std::move(driver);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  config.driver_url_suffix = long_name;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 0);
}

TEST_F(DriverLoaderTest, TestOnlyReturnBaseAndFallback) {
  std::string name = "fuchsia-boot:///#driver1.cm";

  fidl::Arena arena;
  auto driver_info =
      fdi::wire::MatchedDriverInfo::Builder(arena).url(name).is_fallback(false).package_type(
          fdi::wire::DriverPackageType::kBoot);
  driver_index_server.driver = fdi::wire::MatchedDriver::WithDriver(arena, driver_info.Build());

  auto driver = std::make_unique<Driver>();
  driver->url = name;
  resolver.map[name] = std::move(driver);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), false,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  config.only_return_base_and_fallback_drivers = true;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 0);
}

TEST_F(DriverLoaderTest, TestReturnOnlyNodeGroups) {
  fidl::Arena allocator;

  std::vector<fdi::wire::MatchedCompositeNodeSpecInfo> specs;

  // Add first composite node spec.
  auto spec_1 = fdi::wire::MatchedCompositeNodeSpecInfo::Builder(allocator);
  spec_1.node_index(1);
  spec_1.name(fidl::ObjectView<fidl::StringView>(allocator, allocator, "spec_1"));
  specs.push_back(spec_1.Build());

  // Add second composite node spec.
  auto spec_2 = fdi::wire::MatchedCompositeNodeSpecInfo::Builder(allocator);
  spec_2.node_index(0);
  spec_2.name(fidl::ObjectView<fidl::StringView>(allocator, allocator, "spec_2"));
  specs.push_back(spec_2.Build());

  driver_index_server.driver = fdi::wire::MatchedDriver::WithParentSpec(
      allocator,
      fdi::wire::MatchedCompositeNodeParentInfo::Builder(allocator).specs(specs).Build());

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), false,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 1);

  auto spec_result = std::get<fdi::MatchedCompositeNodeParentInfo>(drivers[0]);
  ASSERT_EQ(2, spec_result.specs().value().size());
  ASSERT_STREQ("spec_1", spec_result.specs().value().at(0).name().value());
  ASSERT_EQ(1, spec_result.specs().value().at(0).node_index());
  ASSERT_STREQ("spec_2", spec_result.specs().value().at(1).name().value());
  ASSERT_EQ(0, spec_result.specs().value().at(1).node_index());
}

TEST_F(DriverLoaderTest, TestReturnNodeGroupNoTopologicalPath) {
  fidl::Arena allocator;

  auto spec = fdi::wire::MatchedCompositeNodeSpecInfo::Builder(allocator);
  spec.node_index(1);

  fidl::VectorView<fdi::wire::MatchedCompositeNodeSpecInfo> specs(allocator, 1);
  specs[0] = spec.Build();

  auto parent_spec = fdi::wire::MatchedCompositeNodeParentInfo::Builder(allocator).specs(specs);
  driver_index_server.driver =
      fdi::wire::MatchedDriver::WithParentSpec(allocator, parent_spec.Build());

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), false,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);
  ASSERT_EQ(drivers.size(), 0);
}

TEST_F(DriverLoaderTest, TestReturnNodeGroupNoNodes) {
  fidl::Arena allocator;

  fidl::VectorView<fdi::wire::MatchedCompositeNodeSpecInfo> specs(allocator, 0);
  auto parent_spec = fdi::wire::MatchedCompositeNodeParentInfo::Builder(allocator);
  parent_spec.specs(fidl::ObjectView<fidl::VectorView<fdi::wire::MatchedCompositeNodeSpecInfo>>(
      allocator, specs));
  driver_index_server.driver =
      fdi::wire::MatchedDriver::WithParentSpec(allocator, parent_spec.Build());

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), false,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);
  ASSERT_EQ(drivers.size(), 0);
}

TEST_F(DriverLoaderTest, TestReturnNodeGroupMultipleNodes) {
  fidl::Arena allocator;

  auto spec_1 = fdi::wire::MatchedCompositeNodeSpecInfo::Builder(allocator);
  spec_1.node_index(1);
  spec_1.name(fidl::ObjectView<fidl::StringView>(allocator, allocator, "spec_1"));

  auto spec_2 = fdi::wire::MatchedCompositeNodeSpecInfo::Builder(allocator);
  spec_2.node_index(3);
  spec_2.name(fidl::ObjectView<fidl::StringView>(allocator, allocator, "spec_2"));

  fidl::VectorView<fdi::wire::MatchedCompositeNodeSpecInfo> specs(allocator, 2);
  specs[0] = spec_1.Build();
  specs[1] = spec_2.Build();

  auto parent_spec = fdi::wire::MatchedCompositeNodeParentInfo::Builder(allocator).specs(specs);
  driver_index_server.driver =
      fdi::wire::MatchedDriver::WithParentSpec(allocator, parent_spec.Build());

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), false,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 1);

  auto spec_result = std::get<fdi::MatchedCompositeNodeParentInfo>(drivers[0]);
  ASSERT_EQ(2, spec_result.specs().value().size());
  ASSERT_STREQ("spec_1", spec_result.specs().value().at(0).name().value());
  ASSERT_EQ(1, spec_result.specs().value().at(0).node_index().value());
  ASSERT_STREQ("spec_2", spec_result.specs().value().at(1).name().value());
  ASSERT_EQ(3, spec_result.specs().value().at(1).node_index().value());
}

TEST_F(DriverLoaderTest, TestEphemeralDriver) {
  std::string name = "fuchsia-pkg://fuchsia.com/my-package#meta/#driver1.cm";

  fidl::Arena arena;
  auto driver_info =
      fdi::wire::MatchedDriverInfo::Builder(arena).url(name).is_fallback(false).package_type(
          fdi::wire::DriverPackageType::kUniverse);
  driver_index_server.driver = fdi::wire::MatchedDriver::WithDriver(arena, driver_info.Build());

  // Add driver 1 to universe resolver since it is a universe driver.
  auto driver1 = std::make_unique<Driver>();
  driver1->url = name;
  universe_resolver.map[name] = std::move(driver1);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             &universe_resolver);
  loop.StartThread("fidl-thread");

  // We should find driver1 from the universe resolver.
  DriverLoader::MatchDeviceConfig config;
  config.driver_url_suffix = name;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 1);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).component_url, name);
}

TEST_F(DriverLoaderTest, TestV2Driver) {
  std::string name = "fuchsia-boot:///#driver1.cm";

  fidl::Arena arena;
  auto driver_info = fdi::wire::MatchedDriverInfo::Builder(arena).url(name).is_fallback(false);
  driver_index_server.driver = fdi::wire::MatchedDriver::WithDriver(arena, driver_info.Build());

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             &universe_resolver);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  config.driver_url_suffix = name;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 1);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).is_dfv2, true);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).component_url, name);
}
