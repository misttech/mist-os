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
    mock_usb_.ExpectSetInterface(ZX_OK, {});

    auto [client_end, server_end] = fidl::Endpoints<fadb::Device>::Create();
    std::optional<fidl::ServerBinding<fadb::Device>> binding;
    EXPECT_OK(fdf::RunOnDispatcherSync(
        adb_dispatcher_->async_dispatcher(), [&server_end, &binding, this]() {
          binding.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server_end),
                          dev_->GetDeviceContext<UsbAdbDevice>(), fidl::kIgnoreBindingClosure);
        }));
    std::unique_ptr<FakeAdbDaemon> adb_daemon =
        std::make_unique<FakeAdbDaemon>(std::move(client_end));
    EXPECT_OK(fdf::RunOnDispatcherSync(adb_dispatcher_->async_dispatcher(),
                                       [&binding]() { binding.reset(); }));

    return adb_daemon;
  }

  void ReleaseFakeAdbDaemon(std::unique_ptr<FakeAdbDaemon>& fake_adb) {
    // Calls during Stop().
    mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
    mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
    mock_usb_.ExpectSetInterface(ZX_OK, {});

    libsync::Completion stop_sync;
    dev_->GetDeviceContext<UsbAdbDevice>()->SetShutdownCallback(
        [&stop_sync]() { stop_sync.Signal(); });
    fake_adb.reset();
    incoming_.SyncCall([&](IncomingNamespace* infra) {
      for (size_t i = 0; i < kBulkTxRxCount; i++) {
        infra->fake_dev.fake_endpoint(kBulkOutEp).RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0);
      }
    });
    stop_sync.Wait();
  }

  void SendTestData(std::unique_ptr<FakeAdbDaemon>& fake_adb, size_t size);

 private:
  void SetUp() override {
    ASSERT_EQ(ZX_OK, incoming_loop_.StartThread("incoming-ns-thread"));

    parent_->AddProtocol(ZX_PROTOCOL_USB_FUNCTION, mock_usb_.GetProto()->ops,
                         mock_usb_.GetProto()->ctx);
    auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
    incoming_.SyncCall([server = std::move(endpoints.server)](IncomingNamespace* infra) mutable {
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
                            std::move(endpoints.client));

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
      ASSERT_OK(dev->Init());

      // The DDK now owns this reference.
      [[maybe_unused]] auto released = adb.release();
    }));

    dev_ = parent_->GetLatestChild();
    ASSERT_EQ(dev, dev_->GetDeviceContext<UsbAdbDevice>());

    // Call set_configured of usb adb to bring the interface online.
    mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
    mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
    mock_usb_.function.ops->set_configured(mock_usb_.function.ctx, true, USB_SPEED_FULL);
  }

  void TearDown() override {
    mock_usb_.ExpectSetInterface(ZX_OK, {});

    ASSERT_OK(fdf::RunOnDispatcherSync(adb_dispatcher_->async_dispatcher(),
                                       [this]() { dev_->UnbindOp(); }));
    parent_->GetLatestChild()->WaitUntilUnbindReplyCalled();
    mock_usb_.VerifyAndClear();
    parent_ = nullptr;
  }

  std::shared_ptr<MockDevice> parent_ = MockDevice::FakeRootParent();
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  fdf::UnownedSynchronizedDispatcher adb_dispatcher_ =
      mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher();
  zx_device_t* dev_;

 protected:
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  MockUsbFunction mock_usb_;
};

// Fake Adb protocol service.
class UsbAdbTest::FakeAdbDaemon {
 private:
  class EventHandler : public fidl::WireAsyncEventHandler<fadb::UsbAdbImpl> {
   public:
    ~EventHandler() { EXPECT_TRUE(expected_statuses_.empty()); }

    void OnStatusChanged(fidl::WireEvent<fadb::UsbAdbImpl::OnStatusChanged>* event) override {
      ASSERT_FALSE(expected_statuses_.empty());
      EXPECT_EQ(event->status, expected_statuses_.front());
      expected_statuses_.pop();
    }

    std::queue<fadb::StatusFlags> expected_statuses_;
  };
  EventHandler event_handler_;

 public:
  explicit FakeAdbDaemon(
      fidl::ClientEnd<fadb::Device> client,
      fidl::Endpoints<fadb::UsbAdbImpl> endpoints = fidl::Endpoints<fadb::UsbAdbImpl>::Create())
      : client_(std::move(endpoints.client), loop_.dispatcher(), &event_handler_) {
    ExpectOnStatusChanged(fadb::StatusFlags::kOnline);
    EXPECT_OK(fidl::WireCall(client)->Start(std::move(endpoints.server)));
  }

  void ExpectOnStatusChanged(fadb::StatusFlags expected_status) {
    event_handler_.expected_statuses_.push(expected_status);
  }

  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  fidl::WireClient<fadb::UsbAdbImpl> client_;
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

TEST_F(UsbAdbTest, SetUpTearDown) {
  ASSERT_NO_FATAL_FAILURE();
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
}

TEST_F(UsbAdbTest, StartStop) {
  auto fake_adb = CreateFakeAdbDaemon();
  fake_adb->loop_.RunUntilIdle();

  ReleaseFakeAdbDaemon(fake_adb);
}

TEST_F(UsbAdbTest, SendAdbMessage) {
  auto fake_adb = CreateFakeAdbDaemon();
  fake_adb->loop_.RunUntilIdle();

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

  ReleaseFakeAdbDaemon(fake_adb);
}

TEST_F(UsbAdbTest, RecvAdbMessage) {
  auto fake_adb = CreateFakeAdbDaemon();
  fake_adb->loop_.RunUntilIdle();

  // Queue a receive request before the data is available. The request will not get an immediate
  // reply. Data fits within a single VMO request.
  constexpr uint32_t kReceiveSize = kVmoDataSize - 2;
  fake_adb->client_->Receive().ThenExactlyOnce(
      [&kReceiveSize,
       &fake_adb](fidl::WireUnownedResult<fadb::UsbAdbImpl::Receive>& response) -> void {
        ASSERT_OK(response.status());
        ASSERT_FALSE(response.value().is_error());
        ASSERT_EQ(response.value().value()->data.count(), kReceiveSize);
        fake_adb->loop_.Quit();
      });
  // Invoke request completion on bulk out endpoint.
  incoming_.SyncCall([&](IncomingNamespace* infra) {
    infra->fake_dev.fake_endpoint(kBulkOutEp).RequestComplete(ZX_OK, kReceiveSize);
  });
  ASSERT_EQ(fake_adb->loop_.Run(), ZX_ERR_CANCELED);

  ReleaseFakeAdbDaemon(fake_adb);
}

}  // namespace usb_adb_function
