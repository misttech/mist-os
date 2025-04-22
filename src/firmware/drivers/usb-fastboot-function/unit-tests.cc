// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.fastboot/cpp/wire_test_base.h>
#include <fuchsia/hardware/usb/function/cpp/banjo-mock.h>
#include <lib/driver/compat/cpp/banjo_server.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <memory>

#include <sdk/lib/inspect/testing/cpp/zxtest/inspect.h>
#include <zxtest/zxtest.h>

#include "src/devices/usb/lib/usb-endpoint/testing/fake-usb-endpoint-server.h"
#include "src/firmware/drivers/usb-fastboot-function/usb_fastboot_function.h"

// Some test utilities in "fuchsia/hardware/usb/function/cpp/banjo-mock.h" expect the following
// operators to be implemented.

bool operator==(const usb_request_complete_callback_t& lhs,
                const usb_request_complete_callback_t& rhs) {
  // Comparison of these struct is not useful. Return true always.
  return true;
}

bool operator==(const usb_ss_ep_comp_descriptor_t& lhs, const usb_ss_ep_comp_descriptor_t& rhs) {
  // Comparison of these struct is not useful. Return true always.
  return true;
}

bool operator==(const usb_endpoint_descriptor_t& lhs, const usb_endpoint_descriptor_t& rhs) {
  // Comparison of these struct is not useful. Return true always.
  return true;
}

bool operator==(const usb_request_t& lhs, const usb_request_t& rhs) {
  // Only comparing endpoint address. Use ExpectCallWithMatcher for more specific
  // comparisons.
  return lhs.header.ep_address == rhs.header.ep_address;
}

bool operator==(const usb_function_interface_protocol_t& lhs,
                const usb_function_interface_protocol_t& rhs) {
  // Comparison of these struct is not useful. Return true always.
  return true;
}

namespace usb_fastboot_function {
namespace {

static constexpr uint32_t kBulkOutEp = 1;
static constexpr uint32_t kBulkInEp = 2;

class MockFastbootUsbFunction : public ddk::MockUsbFunction {
 public:
  zx_status_t UsbFunctionSetInterface(const usb_function_interface_protocol_t* interface) override {
    // Overriding method to store the interface passed.
    function_ = *interface;
    return ddk::MockUsbFunction::UsbFunctionSetInterface(interface);
  }

  zx_status_t UsbFunctionConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                                  const usb_ss_ep_comp_descriptor_t* ss_comp_desc) override {
    // Overriding method to handle valid cases where nullptr is passed. The generated mock tries to
    // dereference it without checking.
    usb_endpoint_descriptor_t ep{};
    usb_ss_ep_comp_descriptor_t ss{};
    const usb_endpoint_descriptor_t* arg1 = ep_desc ? ep_desc : &ep;
    const usb_ss_ep_comp_descriptor_t* arg2 = ss_comp_desc ? ss_comp_desc : &ss;
    return ddk::MockUsbFunction::UsbFunctionConfigEp(arg1, arg2);
  }

  compat::DeviceServer::BanjoConfig GetBanjoConfig() {
    compat::DeviceServer::BanjoConfig config{.default_proto_id = ZX_PROTOCOL_USB_FUNCTION};
    config.callbacks[ZX_PROTOCOL_USB_FUNCTION] = banjo_server_.callback();
    return config;
  }

  compat::BanjoServer banjo_server_{ZX_PROTOCOL_USB_FUNCTION, this, GetProto()->ops};
  usb_function_interface_protocol_t function_;
};

using FakeUsb = fake_usb_endpoint::FakeUsbFidlProvider<fuchsia_hardware_usb_function::UsbFunction,
                                                       fake_usb_endpoint::FakeEndpoint>;
class UsbFastbootEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    device_server_.Initialize("default", std::nullopt, mock_usb_.GetBanjoConfig());
    EXPECT_EQ(ZX_OK, device_server_.Serve(dispatcher, &to_driver_vfs));
    fuchsia_hardware_usb_function::UsbFunctionService::InstanceHandler handler({
        .device = usb_function_bindings_.CreateHandler(&fake_dev_, dispatcher,
                                                       fidl::kIgnoreBindingClosure),
    });
    EXPECT_OK(to_driver_vfs.AddService<fuchsia_hardware_usb_function::UsbFunctionService>(
        std::move(handler)));

    return zx::ok();
  }

  // Call set_configured of usb fastboot to bring the interface online.
  void EnableUsb() {
    mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
    mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
    mock_usb_.function_.ops->set_configured(mock_usb_.function_.ctx, true, USB_SPEED_FULL);
  }

  compat::DeviceServer device_server_;
  MockFastbootUsbFunction mock_usb_;
  FakeUsb fake_dev_ = FakeUsb(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  fidl::ServerBindingGroup<fuchsia_hardware_usb_function::UsbFunction> usb_function_bindings_;
};

class UsbFastbootTestConfig final {
 public:
  using DriverType = UsbFastbootFunction;
  using EnvironmentType = UsbFastbootEnvironment;
};

using inspect::InspectTestHelper;

class UsbFastbootFunctionTest : public zxtest::Test {
 public:
  void SetUp() override {
    driver_test_.RunInEnvironmentTypeContext([](UsbFastbootEnvironment& env) {
      // Expect calls from UsbFastbootFunction initialization
      env.mock_usb_.ExpectAllocInterface(ZX_OK, 1);
      env.mock_usb_.ExpectAllocInterface(ZX_OK, 1);
      env.mock_usb_.ExpectAllocEp(ZX_OK, USB_DIR_OUT, kBulkOutEp);
      env.mock_usb_.ExpectAllocEp(ZX_OK, USB_DIR_IN, kBulkInEp);
      env.mock_usb_.ExpectSetInterface(ZX_OK, {});
      env.fake_dev_.ExpectConnectToEndpoint(kBulkOutEp);
      env.fake_dev_.ExpectConnectToEndpoint(kBulkInEp);
    });
    ASSERT_OK(driver_test_.StartDriver().status_value());
    auto device = driver_test_.Connect<fuchsia_hardware_fastboot::Service::Fastboot>();
    EXPECT_OK(device.status_value());
    client_.Bind(std::move(device.value()));
  }

  void TearDown() override {
    driver_test_.RunInEnvironmentTypeContext(
        [](UsbFastbootEnvironment& env) { env.mock_usb_.VerifyAndClear(); });
  }

  fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl>& client() { return client_; }

 protected:
  fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl> client_;
  fdf_testing::BackgroundDriverTest<UsbFastbootTestConfig> driver_test_;
};

TEST_F(UsbFastbootFunctionTest, LifetimeTest) {
  // Lifetime tested in test Setup() and TearDown()
  EXPECT_OK(driver_test_.StopDriver());
}

void ValidateVmo(const zx::vmo& vmo, std::string_view payload) {
  fzl::VmoMapper mapper;
  ASSERT_OK(mapper.Map(vmo));
  size_t content_size = 0;
  ASSERT_OK(vmo.get_prop_content_size(&content_size));
  ASSERT_EQ(content_size, payload.size());
}

TEST_F(UsbFastbootFunctionTest, ReceiveTestSinglePacket) {
  const std::string_view test_data = "getvar:all";
  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) { env.EnableUsb(); });

  std::thread t([&]() {
    auto res = client()->Receive(0);
    ValidateVmo(res.value()->data, test_data);
  });

  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) {
    env.fake_dev_.fake_endpoint(kBulkOutEp).RequestComplete(ZX_OK, test_data.size());
  });

  t.join();
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbFastbootFunctionTest, ReceiveStateReset) {
  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) { env.EnableUsb(); });

  {
    const std::string_view test_data = "getvar:all";
    std::thread t1([&]() {
      auto res = client()->Receive(0);
      ValidateVmo(res.value()->data, test_data);
    });
    driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) {
      env.fake_dev_.fake_endpoint(kBulkOutEp).RequestComplete(ZX_OK, test_data.size());
    });
    t1.join();
  }

  {
    const std::string_view test_data = "getvar:max-download-size";
    std::thread t1([&]() {
      auto res = client()->Receive(0);
      ValidateVmo(res.value()->data, test_data);
    });
    driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) {
      env.fake_dev_.fake_endpoint(kBulkOutEp).RequestComplete(ZX_OK, test_data.size());
    });
    t1.join();
  }
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbFastbootFunctionTest, ReceiveFailsOnError) {
  const std::string_view test_data = "getvar:all";
  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) { env.EnableUsb(); });

  std::thread t([&]() { ASSERT_FALSE(client()->Receive(0)->is_ok()); });

  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) {
    env.fake_dev_.fake_endpoint(kBulkOutEp)
        .RequestComplete(ZX_ERR_IO_NOT_PRESENT, test_data.size());
  });

  t.join();
  EXPECT_OK(driver_test_.StopDriver());
}

void InitializeSendVmo(fzl::OwnedVmoMapper& vmo, std::string_view data) {
  ASSERT_OK(vmo.CreateAndMap(data.size(), "test"));
  ASSERT_OK(vmo.vmo().set_prop_content_size(data.size()));
  memcpy(vmo.start(), data.data(), data.size());
}

TEST_F(UsbFastbootFunctionTest, ReceiveFailsOnNonConfiguredInterface) {
  ASSERT_FALSE(client()->Receive(0)->is_ok());
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbFastbootFunctionTest, Send) {
  const std::string_view send_data = "OKAY0.4";
  fzl::OwnedVmoMapper send_vmo;
  InitializeSendVmo(send_vmo, send_data);

  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) { env.EnableUsb(); });

  std::thread t([&]() {
    auto result = client()->Send(send_vmo.Release());
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  });

  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) {
    env.fake_dev_.fake_endpoint(kBulkInEp).RequestComplete(ZX_OK, send_data.size());
  });

  t.join();
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbFastbootFunctionTest, SendStatesReset) {
  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) { env.EnableUsb(); });

  {
    const std::string_view send_data = "OKAY0.4";
    fzl::OwnedVmoMapper send_vmo;
    InitializeSendVmo(send_vmo, send_data);
    std::thread t([&]() {
      auto result = client()->Send(send_vmo.Release());
      ASSERT_TRUE(result.ok());
      ASSERT_TRUE(result->is_ok());
    });
    driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) {
      env.fake_dev_.fake_endpoint(kBulkInEp).RequestComplete(ZX_OK, send_data.size());
    });
    t.join();
  }

  {
    const std::string_view send_data = "OKAY0.6";
    fzl::OwnedVmoMapper send_vmo;
    InitializeSendVmo(send_vmo, send_data);
    std::thread t([&]() {
      auto result = client()->Send(send_vmo.Release());
      ASSERT_TRUE(result.ok());
      ASSERT_TRUE(result->is_ok());
    });
    driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) {
      env.fake_dev_.fake_endpoint(kBulkInEp).RequestComplete(ZX_OK, send_data.size());
    });
    t.join();
  }
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbFastbootFunctionTest, SendFailsOnNonConfiguredInterface) {
  const std::string_view send_data = "OKAY0.4";
  fzl::OwnedVmoMapper send_vmo;
  InitializeSendVmo(send_vmo, send_data);
  ASSERT_FALSE(client()->Send(send_vmo.Release())->is_ok());
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbFastbootFunctionTest, SendFailOnError) {
  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) { env.EnableUsb(); });

  const std::string_view send_data = "OKAY0.6";
  fzl::OwnedVmoMapper send_vmo;
  InitializeSendVmo(send_vmo, send_data);

  std::thread t([&]() { ASSERT_FALSE(client()->Send(send_vmo.Release())->is_ok()); });

  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) {
    env.fake_dev_.fake_endpoint(kBulkInEp).RequestComplete(ZX_ERR_IO_NOT_PRESENT, send_data.size());
  });

  t.join();
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbFastbootFunctionTest, SendFailsOnZeroContentSize) {
  driver_test_.RunInEnvironmentTypeContext([&](UsbFastbootEnvironment& env) { env.EnableUsb(); });
  const std::string_view send_data = "OKAY0.4";
  fzl::OwnedVmoMapper send_vmo;
  InitializeSendVmo(send_vmo, send_data);
  ASSERT_OK(send_vmo.vmo().set_prop_content_size(0));
  ASSERT_FALSE(client()->Send(send_vmo.Release())->is_ok());
  EXPECT_OK(driver_test_.StopDriver());
}

}  // namespace
}  // namespace usb_fastboot_function
