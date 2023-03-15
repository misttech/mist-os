// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/driver_loader.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <tuple>

#include <zxtest/zxtest.h>

#include "fbl/ref_ptr.h"
#include "src/devices/bin/driver_manager/driver.h"

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

struct FakeDriver {
  std::string driver_url;
  fdi::wire::DriverPackageType package_type;
  bool is_fallback = false;
  bool is_dfv2 = false;

  FakeDriver(std::string driver_url, fdi::wire::DriverPackageType package_type,
             bool is_fallback = false, bool is_dfv2 = false)
      : driver_url(std::move(driver_url)),
        package_type(std::move(package_type)),
        is_fallback(is_fallback),
        is_dfv2(is_dfv2) {}
};

class FakeDriverLoaderIndex final : public fidl::WireServer<fdi::DriverIndex> {
 public:
  void MatchDriver(MatchDriverRequestView request, MatchDriverCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void WaitForBaseDrivers(WaitForBaseDriversCompleter::Sync& completer) override {
    completer.Reply();
  }

  void MatchDriversV1(MatchDriversV1RequestView request,
                      MatchDriversV1Completer::Sync& completer) override {
    fidl::Arena allocator;
    fidl::VectorView<fdi::wire::MatchedDriver> drivers(allocator,
                                                       fake_drivers.size() + specs.size());
    size_t index = 0;
    for (auto& driver : fake_drivers) {
      auto driver_info = fdi::wire::MatchedDriverInfo::Builder(allocator);
      if (driver.is_dfv2) {
        driver_info.url(
            fidl::ObjectView<fidl::StringView>(allocator, allocator, driver.driver_url));
      } else {
        driver_info.driver_url(
            fidl::ObjectView<fidl::StringView>(allocator, allocator, driver.driver_url));
        driver_info.url(
            fidl::ObjectView<fidl::StringView>(allocator, allocator, driver.driver_url));
      }
      driver_info.package_type(driver.package_type);
      driver_info.is_fallback(driver.is_fallback);

      drivers[index] = fdi::wire::MatchedDriver::WithDriver(allocator, driver_info.Build());
      index++;
    }

    for (auto& spec : specs) {
      drivers[index] = fdi::wire::MatchedDriver::WithParentSpec(allocator, spec);
      index++;
    }

    completer.ReplySuccess(drivers);
  }

  void AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request,
                            AddCompositeNodeSpecCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  std::vector<FakeDriver> fake_drivers;
  std::vector<fdi::wire::MatchedCompositeNodeParentInfo> specs;
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

TEST_F(DriverLoaderTest, TestFallbackGetsRemoved) {
  std::string not_fallback_libname = "fuchsia_boot:///#not_fallback.cm";
  std::string fallback_libname = "fuchsia_boot:///#fallback.cm";

  driver_index_server.fake_drivers.emplace_back(not_fallback_libname,
                                                fdi::wire::DriverPackageType::kBoot);
  driver_index_server.fake_drivers.emplace_back(fallback_libname,
                                                fdi::wire::DriverPackageType::kBoot, true);

  auto not_fallback = std::make_unique<Driver>();
  not_fallback->libname = not_fallback_libname;
  resolver.map[not_fallback_libname] = std::move(not_fallback);

  auto fallback = std::make_unique<Driver>();
  fallback->libname = fallback_libname;
  fallback->fallback = true;
  resolver.map[fallback_libname] = std::move(fallback);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             nullptr);

  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);
  ASSERT_EQ(drivers.size(), 1);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).v1()->libname, not_fallback_libname);
}

TEST_F(DriverLoaderTest, TestFallbackAcceptedAfterBaseLoaded) {
  std::string not_fallback_libname = "fuchsia_boot:///#not_fallback.cm";
  std::string fallback_libname = "fuchsia_boot:///#fallback.cm";

  driver_index_server.fake_drivers.emplace_back(not_fallback_libname,
                                                fdi::wire::DriverPackageType::kBoot);
  driver_index_server.fake_drivers.emplace_back(fallback_libname,
                                                fdi::wire::DriverPackageType::kBoot, true);

  auto not_fallback = std::make_unique<Driver>();
  not_fallback->libname = not_fallback_libname;
  resolver.map[not_fallback_libname] = std::move(not_fallback);

  auto fallback = std::make_unique<Driver>();
  fallback->libname = fallback_libname;
  fallback->fallback = true;
  resolver.map[fallback_libname] = std::move(fallback);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             nullptr);
  loop.StartThread("fidl-thread");

  // Wait for base drivers, which is when we load fallback drivers.
  sync_completion_t base_drivers;
  driver_loader.WaitForBaseDrivers([&base_drivers]() { sync_completion_signal(&base_drivers); });
  sync_completion_wait(&base_drivers, ZX_TIME_INFINITE);

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 2);
  // The non-fallback should always be first.
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).v1()->libname, not_fallback_libname);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[1]).v1()->libname, fallback_libname);
}

TEST_F(DriverLoaderTest, TestFallbackAcceptedWhenSystemNotRequired) {
  std::string not_fallback_libname = "fuchsia_boot:///#not_fallback.cm";
  std::string fallback_libname = "fuchsia_boot:///#fallback.cm";

  driver_index_server.fake_drivers.emplace_back(not_fallback_libname,
                                                fdi::wire::DriverPackageType::kBoot);
  driver_index_server.fake_drivers.emplace_back(fallback_libname,
                                                fdi::wire::DriverPackageType::kBoot, true);

  auto not_fallback = std::make_unique<Driver>();
  not_fallback->libname = not_fallback_libname;
  resolver.map[not_fallback_libname] = std::move(not_fallback);

  auto fallback = std::make_unique<Driver>();
  fallback->libname = fallback_libname;
  fallback->fallback = true;
  resolver.map[fallback_libname] = std::move(fallback);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), false,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 2);
  // The non-fallback should always be first.
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).v1()->libname, not_fallback_libname);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[1]).v1()->libname, fallback_libname);
}

TEST_F(DriverLoaderTest, TestLibname) {
  std::string name1 = "fuchsia-boot:///#driver1.cm";
  std::string name2 = "fuchsia-boot:///#driver2.cm";

  driver_index_server.fake_drivers.emplace_back(name1, fdi::wire::DriverPackageType::kBoot);
  driver_index_server.fake_drivers.emplace_back(name2, fdi::wire::DriverPackageType::kBoot);

  auto driver1 = std::make_unique<Driver>();
  driver1->libname = name1;
  resolver.map[name1] = std::move(driver1);

  auto driver2 = std::make_unique<Driver>();
  driver2->libname = name2;
  resolver.map[name2] = std::move(driver2);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  config.libname = name2;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 1);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).v1()->libname, name2);
}

TEST_F(DriverLoaderTest, TestRelativeLibname) {
  std::string name1 = "fuchsia-boot:///#driver1.cm";
  std::string name2 = "fuchsia-pkg://fuchsia.com/my-package#meta/#driver2.cm";

  driver_index_server.fake_drivers.emplace_back(name1, fdi::wire::DriverPackageType::kBoot);
  driver_index_server.fake_drivers.emplace_back(name2, fdi::wire::DriverPackageType::kBase);

  auto driver1 = std::make_unique<Driver>();
  driver1->libname = name1;
  resolver.map[name1] = std::move(driver1);

  auto driver2 = std::make_unique<Driver>();
  driver2->libname = name2;
  resolver.map[name2] = std::move(driver2);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             nullptr);
  loop.StartThread("fidl-thread");

  {
    DriverLoader::MatchDeviceConfig config;
    config.libname = "driver1.cm";
    fidl::VectorView<fdf::wire::NodeProperty> props{};
    auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

    ASSERT_EQ(1, drivers.size());
    ASSERT_EQ(name1, std::get<MatchedDriverInfo>(drivers[0]).v1()->libname);
  }

  {
    DriverLoader::MatchDeviceConfig config;
    config.libname = "driver2.cm";
    fidl::VectorView<fdf::wire::NodeProperty> props{};
    auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

    ASSERT_EQ(1, drivers.size());
    ASSERT_EQ(name2, std::get<MatchedDriverInfo>(drivers[0]).v1()->libname);
  }
}

TEST_F(DriverLoaderTest, TestTooLongRelativeLibname) {
  std::string name1 = "fuchsia-boot:///#driver1.cm";
  // The characters of `libname` do not matter so long as the size of `libname`
  // is longer than `name1`.
  std::string long_name = std::string(name1.length() + 1, 'a');

  driver_index_server.fake_drivers.emplace_back(name1, fdi::wire::DriverPackageType::kBoot);

  auto driver1 = std::make_unique<Driver>();
  driver1->libname = name1;
  resolver.map[name1] = std::move(driver1);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  config.libname = long_name;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 0);
}

TEST_F(DriverLoaderTest, TestOnlyReturnBaseAndFallback) {
  std::string name1 = "fuchsia-pkg://fuchsia.com/my-package#meta/#driver1.cm";
  std::string name2 = "fuchsia-boot:///#meta/driver2.cm";
  std::string name3 = "fuchsia-boot:///#meta/driver3.cm";

  driver_index_server.fake_drivers.emplace_back(name1, fdi::wire::DriverPackageType::kBase);
  driver_index_server.fake_drivers.emplace_back(name2, fdi::wire::DriverPackageType::kBoot);
  driver_index_server.fake_drivers.emplace_back(name3, fdi::wire::DriverPackageType::kBoot, true);

  auto driver1 = std::make_unique<Driver>();
  driver1->libname = name1;
  resolver.map[name1] = std::move(driver1);

  auto driver2 = std::make_unique<Driver>();
  driver2->libname = name2;
  resolver.map[name2] = std::move(driver2);

  auto driver3 = std::make_unique<Driver>();
  driver3->libname = name3;
  driver3->fallback = true;
  resolver.map[name3] = std::move(driver3);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), false,
                             nullptr);
  loop.StartThread("fidl-thread");

  // We can also match libname by the path that the URL turns into.
  DriverLoader::MatchDeviceConfig config;
  config.only_return_base_and_fallback_drivers = true;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 2);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).v1()->libname, name1);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[1]).v1()->libname, name3);
}

TEST_F(DriverLoaderTest, TestReturnOnlyNodeGroups) {
  fidl::Arena allocator;

  // Add first composite node spec.
  auto spec_1 = fdi::wire::MatchedCompositeNodeSpecInfo::Builder(allocator);
  spec_1.node_index(1);
  spec_1.name(fidl::ObjectView<fidl::StringView>(allocator, allocator, "spec_1"));

  fidl::VectorView<fdi::wire::MatchedCompositeNodeSpecInfo> specs_1(allocator, 1);
  specs_1[0] = spec_1.Build();

  auto parent_spec_1 = fdi::wire::MatchedCompositeNodeParentInfo::Builder(allocator);
  parent_spec_1.specs(fidl::ObjectView<fidl::VectorView<fdi::wire::MatchedCompositeNodeSpecInfo>>(
      allocator, specs_1));
  driver_index_server.specs.push_back(parent_spec_1.Build());

  // Add second composite node spec.
  auto spec_2 = fdi::wire::MatchedCompositeNodeSpecInfo::Builder(allocator);
  spec_2.node_index(0);
  spec_2.name(fidl::ObjectView<fidl::StringView>(allocator, allocator, "spec_2"));

  fidl::VectorView<fdi::wire::MatchedCompositeNodeSpecInfo> specs_2(allocator, 1);
  specs_2[0] = spec_2.Build();

  auto parent_spec_2 = fdi::wire::MatchedCompositeNodeParentInfo::Builder(allocator);
  parent_spec_2.specs(specs_2);
  driver_index_server.specs.push_back(parent_spec_2.Build());

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), false,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 2);

  auto spec_result_1 = std::get<fdi::MatchedCompositeNodeParentInfo>(drivers[0]);
  ASSERT_EQ(1, spec_result_1.specs().value().size());
  ASSERT_STREQ("spec_1", spec_result_1.specs().value().at(0).name().value());
  ASSERT_EQ(1, spec_result_1.specs().value().at(0).node_index());

  auto spec_result_2 = std::get<fdi::MatchedCompositeNodeParentInfo>(drivers[1]);
  ASSERT_EQ(1, spec_result_2.specs().value().size());
  ASSERT_STREQ("spec_2", spec_result_2.specs().value().at(0).name().value());
  ASSERT_EQ(0, spec_result_2.specs().value().at(0).node_index());
}

TEST_F(DriverLoaderTest, TestReturnDriversAndNodeGroups) {
  fidl::Arena allocator;

  auto spec = fdi::wire::MatchedCompositeNodeSpecInfo::Builder(allocator);
  spec.node_index(1);
  spec.name(fidl::ObjectView<fidl::StringView>(allocator, allocator, "spec"));

  fidl::VectorView<fdi::wire::MatchedCompositeNodeSpecInfo> specs(allocator, 1);
  specs[0] = spec.Build();

  auto parent_spec = fdi::wire::MatchedCompositeNodeParentInfo::Builder(allocator);
  parent_spec.specs(specs);

  auto driver_name = "fuchsia_boot:///#driver.cm";
  driver_index_server.specs.push_back(parent_spec.Build());
  driver_index_server.fake_drivers.emplace_back(driver_name, fdi::wire::DriverPackageType::kBoot);

  auto driver = std::make_unique<Driver>();
  driver->libname = driver_name;
  resolver.map[driver_name] = std::move(driver);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), false,
                             nullptr);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 2);

  // Check driver.
  ASSERT_EQ(driver_name, std::get<MatchedDriverInfo>(drivers[0]).v1()->libname);

  // Check composite node spec.
  auto spec_result = std::get<fdi::MatchedCompositeNodeParentInfo>(drivers[1]);
  ASSERT_EQ(1, spec_result.specs().value().size());
  ASSERT_STREQ("spec", spec_result.specs().value().at(0).name().value());
  ASSERT_EQ(1, spec_result.specs().value().at(0).node_index().value());
}

TEST_F(DriverLoaderTest, TestReturnNodeGroupNoTopologicalPath) {
  fidl::Arena allocator;

  auto spec = fdi::wire::MatchedCompositeNodeSpecInfo::Builder(allocator);
  spec.node_index(1);

  fidl::VectorView<fdi::wire::MatchedCompositeNodeSpecInfo> specs(allocator, 1);
  specs[0] = spec.Build();

  auto parent_spec = fdi::wire::MatchedCompositeNodeParentInfo::Builder(allocator);
  parent_spec.specs(specs);
  driver_index_server.specs.push_back(parent_spec.Build());

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
  driver_index_server.specs.push_back(parent_spec.Build());

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

  auto parent_spec = fdi::wire::MatchedCompositeNodeParentInfo::Builder(allocator);
  parent_spec.specs(specs);
  driver_index_server.specs.push_back(parent_spec.Build());

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
  std::string name1 = "fuchsia-pkg://fuchsia.com/my-package#meta/#driver1.cm";
  std::string name2 = "fuchsia-boot:///#meta/driver2.cm";

  driver_index_server.fake_drivers.emplace_back(name1, fdi::wire::DriverPackageType::kUniverse);
  driver_index_server.fake_drivers.emplace_back(name2, fdi::wire::DriverPackageType::kBoot);

  // Add driver 1 to universe resolver since it is a universe driver.
  auto driver1 = std::make_unique<Driver>();
  driver1->libname = name1;
  universe_resolver.map[name1] = std::move(driver1);

  // Add driver 2 to the regular resolver.
  auto driver2 = std::make_unique<Driver>();
  driver2->libname = name2;
  resolver.map[name2] = std::move(driver2);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             &universe_resolver);
  loop.StartThread("fidl-thread");

  // We should find driver1 from the universe resolver.
  DriverLoader::MatchDeviceConfig config;
  config.libname = name1;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 1);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).v1()->libname, name1);
}

TEST_F(DriverLoaderTest, TestV2Driver) {
  std::string name = "fuchsia-boot:///#meta/driver.cm";

  driver_index_server.fake_drivers.emplace_back(name, fdi::wire::DriverPackageType::kBoot,
                                                /* is_fallback */ false, /* is_dfv2 */ true);

  DriverLoader driver_loader(nullptr, std::move(driver_index), &resolver, loop.dispatcher(), true,
                             &universe_resolver);
  loop.StartThread("fidl-thread");

  DriverLoader::MatchDeviceConfig config;
  config.libname = name;
  fidl::VectorView<fdf::wire::NodeProperty> props{};
  auto drivers = driver_loader.MatchPropertiesDriverIndex("test_device", props, config);

  ASSERT_EQ(drivers.size(), 1);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).is_v1(), false);
  ASSERT_EQ(std::get<MatchedDriverInfo>(drivers[0]).v2().url, name);
}
