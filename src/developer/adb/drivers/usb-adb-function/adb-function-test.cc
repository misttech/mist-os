// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adb-function.h"

#include <fidl/fuchsia.hardware.adb/cpp/fidl.h>
#include <fuchsia/hardware/usb/function/cpp/banjo-mock.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/sync/completion.h>

#include <map>
#include <vector>

#include <usb/usb-request.h>
#include <zxtest/zxtest.h>

#include "lib/driver/compat/cpp/banjo_server.h"
#include "src/devices/usb/lib/usb-endpoint/testing/fake-usb-endpoint-server.h"

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

namespace usb_adb_function {

static constexpr uint32_t kBulkOutEp = 1;
static constexpr uint32_t kBulkInEp = 2;

typedef struct {
  usb_request_t* usb_request;
  const usb_request_complete_callback_t* complete_cb;
} mock_usb_request_t;

class MockUsbFunction : public ddk::MockUsbFunction {
 public:
  zx_status_t UsbFunctionCancelAll(uint8_t ep_address) override {
    while (!usb_request_queues[ep_address].empty()) {
      const mock_usb_request_t r = usb_request_queues[ep_address].back();
      r.complete_cb->callback(r.complete_cb->ctx, r.usb_request);
      usb_request_queues[ep_address].pop_back();
    }
    return ddk::MockUsbFunction::UsbFunctionCancelAll(ep_address);
  }

  zx_status_t UsbFunctionSetInterface(const usb_function_interface_protocol_t* interface) override {
    // Overriding method to store the interface passed.
    function = *interface;
    if (!on_set_interface_.empty()) {
      on_set_interface_.front()(*this);
      on_set_interface_.pop();
    }
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

  void UsbFunctionRequestQueue(usb_request_t* usb_request,
                               const usb_request_complete_callback_t* complete_cb) override {
    // Override to store requests.
    const uint8_t ep = usb_request->header.ep_address;
    auto queue = usb_request_queues.find(ep);
    if (queue == usb_request_queues.end()) {
      usb_request_queues[ep] = {};
    }
    usb_request_queues[ep].push_back({usb_request, complete_cb});
    mock_request_queue_.Call(*usb_request, *complete_cb);
  }

  compat::DeviceServer::BanjoConfig GetBanjoConfig() {
    compat::DeviceServer::BanjoConfig config{.default_proto_id = ZX_PROTOCOL_USB_FUNCTION};
    config.callbacks[ZX_PROTOCOL_USB_FUNCTION] = banjo_server.callback();
    return config;
  }

  compat::BanjoServer banjo_server{ZX_PROTOCOL_USB_FUNCTION, this, GetProto()->ops};
  usb_function_interface_protocol_t function;
  // Store request queues for each endpoint.
  std::map<uint8_t, std::vector<mock_usb_request_t>> usb_request_queues;

  std::queue<fit::callback<void(MockUsbFunction&)>> on_set_interface_;
};

using FakeUsb = fake_usb_endpoint::FakeUsbFidlProvider<fuchsia_hardware_usb_function::UsbFunction,
                                                       fake_usb_endpoint::FakeEndpoint>;
class UsbAdbEnvironment : public fdf_testing::Environment {
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

  // Call set_configured of usb adb to bring the interface online.
  void EnableUsb() {
    mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
    mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
    mock_usb_.function.ops->set_configured(mock_usb_.function.ctx, true, USB_SPEED_FULL);
  }

  void CancelAllUsbRxRequests() {
    for (size_t i = 0; i < kBulkRxCount; i++) {
      fake_dev_.fake_endpoint(kBulkOutEp).RequestComplete(ZX_ERR_CANCELED, 0);
    }
  }

  // Expect that the driver will call SetInterface, and when it does so, call
  // CancelAllUsbRxRequests.
  //
  // We call CancelAllUsbRxRequests only _after_ the driver calls SetInterface
  // in order to avoid a race condition where we cancel a request, only to have
  // the driver process the cancellation and send it back out again before
  // `StopAdb()` gets processed.
  void ExpectSetInterfaceAndCancelAllRxRequests() {
    mock_usb_.ExpectSetInterface(ZX_OK, {});
    mock_usb_.on_set_interface_.push(
        [this](MockUsbFunction& mock_usb) { CancelAllUsbRxRequests(); });
  }

  compat::DeviceServer device_server_;
  MockUsbFunction mock_usb_;
  FakeUsb fake_dev_ = FakeUsb(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  fidl::ServerBindingGroup<fuchsia_hardware_usb_function::UsbFunction> usb_function_bindings_;
};

class UsbAdbTestConfig final {
 public:
  using DriverType = UsbAdbDevice;
  using EnvironmentType = UsbAdbEnvironment;
};

class UsbAdbTest : public zxtest::Test {
 public:
  fidl::WireSyncClient<fadb::UsbAdbImpl> NormalStartAdb() {
    auto [client_end, server_end] = fidl::Endpoints<fadb::UsbAdbImpl>::Create();
    EXPECT_OK(client_->StartAdb(std::move(server_end)));

    driver_test_.RunInEnvironmentTypeContext([](UsbAdbEnvironment& env) { env.EnableUsb(); });

    return fidl::WireSyncClient<fadb::UsbAdbImpl>(std::move(client_end));
  }

  void NormalStopDriver() {
    driver_test_.RunInEnvironmentTypeContext(
        [](UsbAdbEnvironment& env) { env.ExpectSetInterfaceAndCancelAllRxRequests(); });
    EXPECT_OK(driver_test_.StopDriver());
  }

  void SetUp() override {
    // Expect calls from UsbAdbDevice initialization
    driver_test_.RunInEnvironmentTypeContext([](UsbAdbEnvironment& env) {
      env.mock_usb_.ExpectAllocInterface(ZX_OK, 1);
      env.mock_usb_.ExpectAllocEp(ZX_OK, USB_DIR_OUT, kBulkOutEp);
      env.mock_usb_.ExpectAllocEp(ZX_OK, USB_DIR_IN, kBulkInEp);
      env.mock_usb_.ExpectSetInterface(ZX_OK, {});
      env.fake_dev_.ExpectConnectToEndpoint(kBulkOutEp);
      env.fake_dev_.ExpectConnectToEndpoint(kBulkInEp);
    });

    ASSERT_OK(driver_test_.StartDriver().status_value());
    auto device = driver_test_.Connect<fadb::Service::Adb>();
    EXPECT_OK(device.status_value());
    client_.Bind(std::move(device.value()));
  }

  void TearDown() override {
    driver_test_.RunInEnvironmentTypeContext(
        [](UsbAdbEnvironment& env) { env.mock_usb_.VerifyAndClear(); });
  }

  void SendTestData(fidl::WireSyncClient<fadb::UsbAdbImpl>& usb_impl, size_t size) {
    uint8_t test_data[size];

    driver_test_.RunInEnvironmentTypeContext([&](UsbAdbEnvironment& env) {
      for (uint32_t i = 0; i < sizeof(test_data) / kVmoDataSize; i++) {
        env.fake_dev_.fake_endpoint(kBulkInEp).RequestComplete(ZX_OK, kVmoDataSize);
      }
      if (sizeof(test_data) % kVmoDataSize) {
        env.fake_dev_.fake_endpoint(kBulkInEp).RequestComplete(ZX_OK,
                                                               sizeof(test_data) % kVmoDataSize);
      }
    });

    auto result =
        usb_impl->QueueTx(fidl::VectorView<uint8_t>::FromExternal(test_data, sizeof(test_data)));
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());

    driver_test_.RunInEnvironmentTypeContext([&](UsbAdbEnvironment& env) {
      EXPECT_EQ(env.fake_dev_.fake_endpoint(kBulkInEp).pending_request_count(), 0);
    });
  }

  void ExpectReceiveData(size_t size) {
    // Invoke request completion on bulk out endpoint.
    driver_test_.RunInEnvironmentTypeContext([&](UsbAdbEnvironment& env) {
      env.fake_dev_.fake_endpoint(kBulkOutEp).RequestComplete(ZX_OK, size);
    });
  }

  fdf_testing::BackgroundDriverTest<UsbAdbTestConfig> driver_test_;
  fidl::WireSyncClient<fadb::Device> client_;
};

class EventHandler : public fidl::WireSyncEventHandler<fadb::UsbAdbImpl> {
 public:
  ~EventHandler() { EXPECT_TRUE(expected_statuses_.empty()); }

  void OnStatusChanged(fidl::WireEvent<fadb::UsbAdbImpl::OnStatusChanged>* event) override {
    ASSERT_FALSE(expected_statuses_.empty());
    EXPECT_EQ(event->status, expected_statuses_.front());
    expected_statuses_.pop();
  }

  std::queue<fadb::StatusFlags> expected_statuses_;
};

TEST_F(UsbAdbTest, StopBeforeUsbStartsUp) {
  // Expect disconnect.
  driver_test_.RunInEnvironmentTypeContext(
      [](UsbAdbEnvironment& env) { env.mock_usb_.ExpectSetInterface(ZX_OK, {}); });
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbAdbTest, StartStop) {
  auto [client_end, server_end] = fidl::Endpoints<fadb::UsbAdbImpl>::Create();
  EXPECT_OK(client_->StartAdb(std::move(server_end)));
  auto usb_impl = fidl::WireSyncClient<fadb::UsbAdbImpl>(std::move(client_end));

  EventHandler handler;

  // TODO(https://fxbug.dev/398918059): Enable this assertion when
  // HandleOneEvent supports a deadline.
  //
  // We don't expect an "online" event until after USB comes up.
  // EXPECT_EQ(usb_impl.HandleOneEvent(handler, zx::deadline_after(zx::msec(1))).status(),
  //           ZX_ERR_TIMED_OUT);

  driver_test_.RunInEnvironmentTypeContext([](UsbAdbEnvironment& env) { env.EnableUsb(); });

  // Now we should get the event.
  handler.expected_statuses_.push(fadb::StatusFlags::kOnline);
  EXPECT_OK(usb_impl.HandleOneEvent(handler));

  libsync::Completion stop_requested;
  driver_test_.RunInEnvironmentTypeContext([&](UsbAdbEnvironment& env) {
    env.mock_usb_.ExpectSetInterface(ZX_OK, {});
    env.mock_usb_.on_set_interface_.emplace(
        [&stop_requested](MockUsbFunction& mock_usb) { stop_requested.Signal(); });
  });

  // Request a USB reset.
  libsync::Completion stop_finished;
  std::thread t([&]() {
    EXPECT_OK(client_->StopAdb());
    stop_finished.Signal();
  });

  // TODO(https://fxbug.dev/398918059): Enable this assertion when
  // HandleOneEvent supports a deadline.
  //
  // We don't expect an "offline" event or for StopAdb to complete until USB is shut down.
  // EXPECT_EQ(usb_impl.HandleOneEvent(handler, zx::deadline_after(zx::msec(1))).status(),
  //           ZX_ERR_TIMED_OUT);
  EXPECT_EQ(stop_finished.Wait(zx::deadline_after(zx::msec(1))), ZX_ERR_TIMED_OUT);

  // We call CancelAllUsbRxRequests only _after_ the driver calls SetInterface
  // in order to avoid a race condition where we cancel a request, only to have
  // the driver process the cancellation and send it back out again before
  // `StopAdb()` gets processed.
  stop_requested.Wait();
  driver_test_.RunInEnvironmentTypeContext([&](UsbAdbEnvironment& env) {
    env.CancelAllUsbRxRequests();
    env.mock_usb_.ExpectSetInterface(ZX_OK, {});
  });

  handler.expected_statuses_.emplace(0);
  EXPECT_OK(usb_impl.HandleOneEvent(handler));
  EXPECT_EQ(usb_impl.HandleOneEvent(handler).status(), ZX_ERR_PEER_CLOSED);

  stop_finished.Wait();
  t.join();

  driver_test_.RunInEnvironmentTypeContext(
      [](UsbAdbEnvironment& env) { env.mock_usb_.ExpectSetInterface(ZX_OK, {}); });

  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbAdbTest, StopDriverWhileConnected) {
  auto usb_impl = NormalStartAdb();

  EventHandler handler;
  handler.expected_statuses_.emplace(fadb::StatusFlags::kOnline);
  EXPECT_OK(usb_impl.HandleOneEvent(handler));

  driver_test_.RunInEnvironmentTypeContext(
      [&](UsbAdbEnvironment& env) { env.ExpectSetInterfaceAndCancelAllRxRequests(); });

  EXPECT_OK(driver_test_.StopDriver());

  handler.expected_statuses_.emplace(0);
  EXPECT_OK(usb_impl.HandleOneEvent(handler));
}

TEST_F(UsbAdbTest, UsbStackRequestsStop) {
  auto usb_impl = NormalStartAdb();

  EventHandler handler;
  handler.expected_statuses_.emplace(fadb::StatusFlags::kOnline);
  EXPECT_OK(usb_impl.HandleOneEvent(handler));

  driver_test_.RunInEnvironmentTypeContext([](UsbAdbEnvironment& env) {
    // After we call SetConfigured(), the driver will request USB stop and then
    // start.
    env.mock_usb_.ExpectSetInterface(ZX_OK, {});
    env.mock_usb_.ExpectSetInterface(ZX_OK, {});

    env.mock_usb_.function.ops->set_configured(env.mock_usb_.function.ctx, false, 0);
    env.CancelAllUsbRxRequests();
  });

  handler.expected_statuses_.emplace(0);
  EXPECT_OK(usb_impl.HandleOneEvent(handler));

  driver_test_.RunInEnvironmentTypeContext(
      [](UsbAdbEnvironment& env) { env.mock_usb_.ExpectSetInterface(ZX_OK, {}); });
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbAdbTest, StartStopStartStop) {
  {
    EventHandler handler;
    auto usb_impl = NormalStartAdb();
    handler.expected_statuses_.push(fadb::StatusFlags::kOnline);
    EXPECT_OK(usb_impl.HandleOneEvent(handler));

    driver_test_.RunInEnvironmentTypeContext([](UsbAdbEnvironment& env) {
      env.ExpectSetInterfaceAndCancelAllRxRequests();
      env.mock_usb_.ExpectSetInterface(ZX_OK, {});
    });
    EXPECT_OK(client_->StopAdb());

    handler.expected_statuses_.emplace(0);
    EXPECT_OK(usb_impl.HandleOneEvent(handler));
    EXPECT_EQ(usb_impl.HandleOneEvent(handler).status(), ZX_ERR_PEER_CLOSED);
  }

  {
    EventHandler handler;
    auto usb_impl = NormalStartAdb();
    handler.expected_statuses_.push(fadb::StatusFlags::kOnline);
    EXPECT_OK(usb_impl.HandleOneEvent(handler));

    driver_test_.RunInEnvironmentTypeContext([](UsbAdbEnvironment& env) {
      env.ExpectSetInterfaceAndCancelAllRxRequests();
      env.mock_usb_.ExpectSetInterface(ZX_OK, {});
    });
    EXPECT_OK(client_->StopAdb());

    handler.expected_statuses_.emplace(0);
    EXPECT_OK(usb_impl.HandleOneEvent(handler));
    EXPECT_EQ(usb_impl.HandleOneEvent(handler).status(), ZX_ERR_PEER_CLOSED);
  }

  driver_test_.RunInEnvironmentTypeContext(
      [](UsbAdbEnvironment& env) { env.mock_usb_.ExpectSetInterface(ZX_OK, {}); });
  EXPECT_OK(driver_test_.StopDriver());
}

TEST_F(UsbAdbTest, StartAdbAfterUsbConnectionEstablished) {
  driver_test_.RunInEnvironmentTypeContext([](UsbAdbEnvironment& env) { env.EnableUsb(); });

  auto [client_end, server_end] = fidl::Endpoints<fadb::UsbAdbImpl>::Create();
  EXPECT_OK(client_->StartAdb(std::move(server_end)));

  auto usb_impl = fidl::WireSyncClient<fadb::UsbAdbImpl>(std::move(client_end));

  // We should get kOnline immediately, because we're already connected.
  EventHandler handler;
  handler.expected_statuses_.push(fadb::StatusFlags::kOnline);
  EXPECT_OK(usb_impl.HandleOneEvent(handler));

  ASSERT_NO_FATAL_FAILURE(NormalStopDriver());
}

TEST_F(UsbAdbTest, SendAdbMessage) {
  auto usb_impl = NormalStartAdb();

  // Sending data that fits within a single VMO request
  ASSERT_NO_FATAL_FAILURE(SendTestData(usb_impl, kVmoDataSize - 2));
  // Sending data that is exactly fills up a single VMO request
  ASSERT_NO_FATAL_FAILURE(SendTestData(usb_impl, kVmoDataSize));
  // Sending data that exceeds a single VMO request
  ASSERT_NO_FATAL_FAILURE(SendTestData(usb_impl, kVmoDataSize + 2));
  // Sending data that exceeds kBulkTxRxCount VMO requests (the last packet should be stored in
  // queue)
  ASSERT_NO_FATAL_FAILURE(SendTestData(usb_impl, kVmoDataSize * kBulkTxCount + 2));
  // Sending data that exceeds kBulkTxRxCount + 1 VMO requests (probably unneeded test, but added
  // for good measure.)
  ASSERT_NO_FATAL_FAILURE(SendTestData(usb_impl, kVmoDataSize * (kBulkTxCount + 1) + 2));

  ASSERT_NO_FATAL_FAILURE(NormalStopDriver());
}

TEST_F(UsbAdbTest, RecvAdbMessage) {
  constexpr uint32_t kReceiveSize = kVmoDataSize - 2;
  auto usb_impl = NormalStartAdb();

  // Queue a receive request before the data is available. The request will not get an immediate
  // reply. Data fits within a single VMO request.

  std::thread t([&]() {
    auto response = usb_impl->Receive();
    ASSERT_OK(response.status());
    ASSERT_EQ(response.value().value()->data.count(), kReceiveSize);
  });

  // Wait to make it so (most likely) the `Receive` request arrives first. This is
  // just a test coverage thing - it won't flake if the `ExpectReceiveData`
  // happens first.
  zx::nanosleep(zx::deadline_after(zx::msec(1)));

  ASSERT_NO_FATAL_FAILURE(ExpectReceiveData(kReceiveSize));
  t.join();

  ASSERT_NO_FATAL_FAILURE(NormalStopDriver());
}

}  // namespace usb_adb_function
