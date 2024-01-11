// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.test/cpp/wire.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fuchsia/driver/development/cpp/fidl.h>
#include <fuchsia/driver/test/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/ddk/driver.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/driver_test_realm/realm_builder/cpp/lib.h>
#include <lib/fdio/directory.h>
#include <lib/sys/cpp/component_context.h>

#include <bind/bindlib/codegen/testlib/cpp/bind.h>
#include <bind/bindlibparent/codegen/testlib/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

namespace lib = bind_bindlib_codegen_testlib;
namespace parent = bind_bindlibparent_codegen_testlib;

const std::string kChildDevicePath = "/dev/sys/test/parent";

class BindLibToFidlCodeGenTest : public testing::Test {
 protected:
  void SetUp() override {
    // Create and build the realm.
    realm_builder_ = component_testing::RealmBuilder::Create();
    driver_test_realm::Setup(*realm_builder_);
    realm_ = realm_builder_->Build(loop_.dispatcher());

    // Start DriverTestRealm.
    fidl::SynchronousInterfacePtr<fuchsia::driver::test::Realm> driver_test_realm;
    ASSERT_EQ(ZX_OK, realm_->component().Connect(driver_test_realm.NewRequest()));

    auto args = fuchsia::driver::test::RealmArgs();
    args.set_use_driver_framework_v2(false);

    fuchsia::driver::test::Realm_Start_Result realm_result;
    ASSERT_EQ(ZX_OK, driver_test_realm->Start(std::move(args), &realm_result));
    ASSERT_FALSE(realm_result.is_err());

    // Connect to dev.
    fidl::InterfaceHandle<fuchsia::io::Node> dev;
    zx_status_t status =
        realm_->component().Connect("dev-topological", dev.NewRequest().TakeChannel());
    ASSERT_EQ(status, ZX_OK);

    // Turn it into a file descriptor.
    fbl::unique_fd dev_fd;
    ASSERT_EQ(fdio_fd_create(dev.TakeChannel().release(), dev_fd.reset_and_get_address()), ZX_OK);

    // Wait for the child device to bind and appear. The child device should bind with its string
    // properties.
    zx::result channel =
        device_watcher::RecursiveWaitForFile(dev_fd.get(), "sys/test/parent/child");
    ASSERT_EQ(channel.status_value(), ZX_OK);

    // Connect to the DriverDevelopment service.
    status = realm_->component().Connect(driver_dev_.NewRequest());
    ASSERT_EQ(status, ZX_OK);
  }

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::optional<component_testing::RealmBuilder> realm_builder_;
  std::optional<component_testing::RealmRoot> realm_;
  fuchsia::driver::development::ManagerSyncPtr driver_dev_;
};

// TODO(b/316176095): Re-enable test after ensuring it works with DFv2.
TEST_F(BindLibToFidlCodeGenTest, DISABLED_DeviceProperties) {
  fuchsia::driver::development::NodeInfoIteratorSyncPtr iterator;
  ASSERT_EQ(ZX_OK, driver_dev_->GetNodeInfo({kChildDevicePath}, iterator.NewRequest(),
                                            /* exact_match= */ true));

  std::vector<fuchsia::driver::development::NodeInfo> devices;
  ASSERT_EQ(iterator->GetNext(&devices), ZX_OK);

  auto& str_props = devices[0].versioned_info().v1().property_list().str_props;
  ASSERT_EQ(static_cast<size_t>(9), str_props.size());

  ASSERT_EQ(bind_fuchsia::PROTOCOL, str_props[0].key);
  ASSERT_TRUE(str_props[0].value.is_int_value());
  ASSERT_EQ(3u, str_props[0].value.int_value());

  ASSERT_EQ(bind_fuchsia::PCI_VID, str_props[1].key);
  ASSERT_TRUE(str_props[1].value.is_int_value());
  ASSERT_EQ(lib::BIND_PCI_VID_PIE, str_props[1].value.int_value());

  ASSERT_EQ(bind_fuchsia::PCI_DID, str_props[2].key);
  ASSERT_TRUE(str_props[2].value.is_int_value());
  ASSERT_EQ(1234u, str_props[2].value.int_value());

  ASSERT_EQ("bindlib.codegen.testlib.kinglet", str_props[3].key);
  ASSERT_EQ(lib::KINGLET, str_props[3].key);
  ASSERT_TRUE(str_props[3].value.is_str_value());
  ASSERT_EQ("firecrest", str_props[3].value.str_value());

  ASSERT_EQ("bindlib.codegen.testlib.Moon", str_props[4].key);
  ASSERT_EQ(lib::MOON, str_props[4].key);
  ASSERT_TRUE(str_props[4].value.is_enum_value());
  ASSERT_EQ("bindlib.codegen.testlib.Moon.Half", str_props[4].value.enum_value());
  ASSERT_EQ(lib::MOON_HALF, str_props[4].value.enum_value());

  ASSERT_EQ("bindlib.codegen.testlib.bobolink", str_props[5].key);
  ASSERT_EQ(lib::BOBOLINK, str_props[5].key);
  ASSERT_TRUE(str_props[5].value.is_int_value());
  ASSERT_EQ(static_cast<uint32_t>(10), str_props[5].value.int_value());

  ASSERT_EQ("bindlib.codegen.testlib.flag", str_props[6].key);
  ASSERT_EQ(lib::FLAG, str_props[6].key);
  ASSERT_TRUE(str_props[6].value.is_bool_value());
  ASSERT_TRUE(str_props[6].value.bool_value());
  ASSERT_EQ(lib::FLAG_ENABLE, str_props[6].value.bool_value());

  ASSERT_EQ("bindlibparent.codegen.testlib.Pizza", str_props[7].key);
  ASSERT_EQ(parent::PIZZA, str_props[7].key);
  ASSERT_TRUE(str_props[7].value.is_str_value());
  ASSERT_EQ("pepperoni pizza", str_props[7].value.str_value());
  ASSERT_EQ(parent::PIZZA_PEPPERONI, str_props[7].value.str_value());

  ASSERT_EQ("bindlibparent.codegen.testlib.Grit", str_props[8].key);
  ASSERT_EQ(parent::GRIT, str_props[8].key);
  ASSERT_TRUE(str_props[8].value.is_int_value());
  ASSERT_EQ(static_cast<uint32_t>(100), str_props[8].value.int_value());
  ASSERT_EQ(parent::GRIT_COARSE, str_props[8].value.int_value());
}
