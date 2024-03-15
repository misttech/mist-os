// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adb-function.h"

#include <fidl/fuchsia.hardware.adb/cpp/fidl.h>
#include <fuchsia/hardware/usb/function/cpp/banjo-mock.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/sync/completion.h>

#include <map>
#include <vector>

#include <usb/usb-request.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
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

  usb_function_interface_protocol_t function;
  // Store request queues for each endpoint.
  std::map<uint8_t, std::vector<mock_usb_request_t>> usb_request_queues;
};

struct IncomingNamespace {
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
  fake_usb_endpoint::FakeUsbFidlProvider<fuchsia_hardware_usb_function::UsbFunction> fake_dev{
      async_get_default_dispatcher()};
  fidl::ServerBindingGroup<fuchsia_hardware_usb_function::UsbFunction> usb_function_bindings_;
};

class UsbAdbTest : public zxtest::Test {
 private:
  class FakeAdbDaemon;

 public:
  static constexpr uint32_t kBulkOutEp = 1;
  static constexpr uint32_t kBulkInEp = 2;
  static constexpr uint32_t kBulkTxRxCount = 2;
  static constexpr uint32_t kVmoDataSize = 10;

  std::unique_ptr<FakeAdbDaemon> CreateFakeAdbDaemon() {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_adb::UsbAdbImpl>();
    EXPECT_TRUE(endpoints.is_ok());

    {
      auto adb_endpoints = fidl::CreateEndpoints<fuchsia_hardware_adb::Device>();
      EXPECT_TRUE(endpoints.is_ok());
      std::optional<fidl::ServerBinding<fuchsia_hardware_adb::Device>> binding;
      EXPECT_OK(fdf::RunOnDispatcherSync(
          adb_dispatcher_->async_dispatcher(), [&adb_endpoints, &binding, this]() {
            binding.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                            std::move(adb_endpoints->server),
                            dev_->GetDeviceContext<UsbAdbDevice>(), fidl::kIgnoreBindingClosure);
          }));
      EXPECT_OK(fidl::WireCall(adb_endpoints->client)->Start(std::move(endpoints->server)));
      EXPECT_OK(fdf::RunOnDispatcherSync(adb_dispatcher_->async_dispatcher(),
                                         [&binding]() { binding.reset(); }));
    }

    return std::make_unique<FakeAdbDaemon>(std::move(endpoints->client));
  }

  void Configure() {
    // Call set_configured of usb adb to bring the interface online.
    mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
    mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
    mock_usb_.function.ops->set_configured(mock_usb_.function.ctx, true, USB_SPEED_FULL);
    configured_ = true;
  }

  void SendTestData(std::unique_ptr<FakeAdbDaemon>& fake_adb, size_t size);

 private:
  void SetUp() override {
    ASSERT_EQ(ZX_OK, incoming_loop_.StartThread("incoming-ns-thread"));

    parent_->AddProtocol(ZX_PROTOCOL_USB_FUNCTION, mock_usb_.GetProto()->ops,
                         mock_usb_.GetProto()->ctx);
    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);
    incoming_.SyncCall([server = std::move(endpoints->server)](IncomingNamespace* infra) mutable {
      ASSERT_OK(
          infra->outgoing.template AddService<fuchsia_hardware_usb_function::UsbFunctionService>(
              fuchsia_hardware_usb_function::UsbFunctionService::InstanceHandler({
                  .device = infra->usb_function_bindings_.CreateHandler(
                      &infra->fake_dev, async_get_default_dispatcher(),
                      fidl::kIgnoreBindingClosure),
              })));

      ASSERT_OK(infra->outgoing.Serve(std::move(server)));
    });
    parent_->AddFidlService(fuchsia_hardware_usb_function::UsbFunctionService::Name,
                            std::move(endpoints->client));

    // Expect calls from UsbAdbDevice initialization
    mock_usb_.ExpectAllocInterface(ZX_OK, 1);
    mock_usb_.ExpectAllocEp(ZX_OK, USB_DIR_OUT, kBulkOutEp);
    mock_usb_.ExpectAllocEp(ZX_OK, USB_DIR_IN, kBulkInEp);
    mock_usb_.ExpectSetInterface(ZX_OK, {});
    incoming_.SyncCall([](IncomingNamespace* infra) {
      infra->fake_dev.ExpectConnectToEndpoint(kBulkOutEp);
      infra->fake_dev.ExpectConnectToEndpoint(kBulkInEp);
    });
    UsbAdbDevice* dev;
    ASSERT_OK(fdf::RunOnDispatcherSync(adb_dispatcher_->async_dispatcher(), [&]() {
      auto adb = std::make_unique<UsbAdbDevice>(parent_.get(), kBulkTxRxCount, kBulkTxRxCount,
                                                kVmoDataSize);
      dev = adb.get();
      stop_sync_ = &dev->test_stop_sync_;
      ASSERT_OK(dev->Init());

      // The DDK now owns this reference.
      [[maybe_unused]] auto released = adb.release();
    }));

    dev_ = parent_->GetLatestChild();
    ASSERT_EQ(dev, dev_->GetDeviceContext<UsbAdbDevice>());
  }

  void TearDown() override {
    mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
    mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
    ASSERT_OK(fdf::RunOnDispatcherSync(adb_dispatcher_->async_dispatcher(),
                                       [this]() { dev_->SuspendNewOp(0, false, 0); }));
    if (configured_) {
      incoming_.SyncCall([&](IncomingNamespace* infra) {
        for (size_t i = 0; i < kBulkTxRxCount; i++) {
          infra->fake_dev.fake_endpoint(kBulkOutEp).RequestComplete(ZX_ERR_CANCELED, 0);
        }
      });
    }
    dev_->WaitUntilSuspendReplyCalled();
    ASSERT_OK(fdf::RunOnDispatcherSync(adb_dispatcher_->async_dispatcher(),
                                       [this]() { dev_->ReleaseOp(); }));
    parent_ = nullptr;
    mock_usb_.VerifyAndClear();
  }

  std::shared_ptr<MockDevice> parent_ = MockDevice::FakeRootParent();
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  fdf::UnownedSynchronizedDispatcher adb_dispatcher_ =
      mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher();
  zx_device_t* dev_;
  bool configured_ = false;

 protected:
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  MockUsbFunction mock_usb_;
  libsync::Completion* stop_sync_;
};

// Fake Adb protocol service.
class UsbAdbTest::FakeAdbDaemon {
 private:
  class EventHandler : public fidl::WireAsyncEventHandler<fuchsia_hardware_adb::UsbAdbImpl> {
   public:
    explicit EventHandler(FakeAdbDaemon* dev) : dev_(dev) {}

   private:
    void OnStatusChanged(
        fidl::WireEvent<fuchsia_hardware_adb::UsbAdbImpl::OnStatusChanged>* event) override {
      dev_->status_ = event->status;
    }

    FakeAdbDaemon* dev_;
  };
  EventHandler event_handler_{this};
  fuchsia_hardware_adb::StatusFlags status_;

 public:
  explicit FakeAdbDaemon(fidl::ClientEnd<fuchsia_hardware_adb::UsbAdbImpl> client)
      : client_(std::move(client), loop_.dispatcher(), &event_handler_) {}

  void CheckStatus(fuchsia_hardware_adb::StatusFlags expected_status) {
    loop_.RunUntilIdle();
    EXPECT_EQ(status_, expected_status);
  }

  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  fidl::WireClient<fuchsia_hardware_adb::UsbAdbImpl> client_;
};

void UsbAdbTest::SendTestData(std::unique_ptr<FakeAdbDaemon>& fake_adb, size_t size) {
  uint8_t test_data[size];
  incoming_.SyncCall([&](IncomingNamespace* infra) {
    for (uint32_t i = 0; i < sizeof(test_data) / kVmoDataSize; i++) {
      infra->fake_dev.fake_endpoint(kBulkInEp).RequestComplete(ZX_OK, kVmoDataSize);
    }
    if (sizeof(test_data) % kVmoDataSize) {
      infra->fake_dev.fake_endpoint(kBulkInEp).RequestComplete(ZX_OK,
                                                               sizeof(test_data) % kVmoDataSize);
    }
  });

  auto result = fake_adb->client_.sync()->QueueTx(
      fidl::VectorView<uint8_t>::FromExternal(test_data, sizeof(test_data)));
  ASSERT_TRUE(result.ok());
  ASSERT_TRUE(result->is_ok());

  incoming_.SyncCall([](IncomingNamespace* infra) {
    EXPECT_EQ(infra->fake_dev.fake_endpoint(kBulkInEp).pending_request_count(), 0);
  });
}

TEST_F(UsbAdbTest, SetUpTearDown) { ASSERT_NO_FATAL_FAILURE(); }

TEST_F(UsbAdbTest, StartStop) {
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  auto fake_adb = CreateFakeAdbDaemon();

  // Calls during Stop().
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  // Close fake_adb so that Stop() will be invoked.
  fake_adb.reset();
  stop_sync_->Wait();
}

TEST_F(UsbAdbTest, SendAdbMessage) {
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  auto fake_adb = CreateFakeAdbDaemon();
  fake_adb->CheckStatus(fuchsia_hardware_adb::StatusFlags(0));

  Configure();
  fake_adb->CheckStatus(fuchsia_hardware_adb::StatusFlags::kOnline);

  // Sending data that fits within a single VMO request
  SendTestData(fake_adb, kVmoDataSize - 2);
  // Sending data that is exactly fills up a single VMO request
  SendTestData(fake_adb, kVmoDataSize);
  // Sending data that exceeds a single VMO request
  SendTestData(fake_adb, kVmoDataSize + 2);
  // Sending data that exceeds kBulkTxRxCount VMO requests (the last packet should be stored in
  // queue)
  SendTestData(fake_adb, kVmoDataSize * kBulkTxRxCount + 2);
  // Sending data that exceeds kBulkTxRxCount + 1 VMO requests (probably unneeded test, but added
  // for good measure.)
  SendTestData(fake_adb, kVmoDataSize * (kBulkTxRxCount + 1) + 2);

  // Calls during Stop().
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  // Close fake_adb so that Stop() will be invoked.
  fake_adb.reset();
  stop_sync_->Wait();
}

TEST_F(UsbAdbTest, RecvAdbMessage) {
  // Call set_configured of usb adb.
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  Configure();
  auto fake_adb = CreateFakeAdbDaemon();
  fake_adb->CheckStatus(fuchsia_hardware_adb::StatusFlags::kOnline);

  // Queue a receive request before the data is available. The request will not get an immediate
  // reply. Data fits within a single VMO request.
  constexpr uint32_t kReceiveSize = kVmoDataSize - 2;
  libsync::Completion wait;
  fake_adb->client_->Receive().ThenExactlyOnce(
      [&wait, &kReceiveSize](
          fidl::WireUnownedResult<::fuchsia_hardware_adb::UsbAdbImpl::Receive>& response) -> void {
        ASSERT_OK(response.status());
        ASSERT_FALSE(response.value().is_error());
        ASSERT_EQ(response.value().value()->data.count(), kReceiveSize);
        wait.Signal();
      });
  // Invoke request completion on bulk out endpoint.
  incoming_.SyncCall([&](IncomingNamespace* infra) {
    infra->fake_dev.fake_endpoint(kBulkOutEp).RequestComplete(ZX_OK, kReceiveSize);
  });
  do {
    fake_adb->loop_.RunUntilIdle();
  } while (wait.Wait(zx::msec(1)) != ZX_OK);

  // Calls during Stop().
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  // Close fake_adb so that Stop() will be invoked.
  fake_adb.reset();
  stop_sync_->Wait();
}

}  // namespace usb_adb_function
