// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "adb-function.h"

#include <fidl/fuchsia.hardware.adb/cpp/fidl.h>
#include <fuchsia/hardware/usb/function/cpp/banjo-mock.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/sync/completion.h>

#include <map>
#include <vector>

#include <usb/usb-request.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/lib/usb-endpoint/testing/fake-usb-endpoint-server.h"
#include "zircon/system/ulib/async-default/include/lib/async/default.h"

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
  std::shared_ptr<
      fake_usb_endpoint::FakeUsbFidlProvider<fuchsia_hardware_usb_function::UsbFunction>>
      fake_dev;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

class UsbAdbTest : public zxtest::Test {
 public:
  static constexpr uint32_t kBulkOutEp = 1;
  static constexpr uint32_t kBulkInEp = 2;

  void SetUp() override {
    ASSERT_OK(incoming_loop_.StartThread("usb-adb-endpoint-loop"));

    parent_ = MockDevice::FakeRootParent();
    parent_->AddProtocol(ZX_PROTOCOL_USB_FUNCTION, mock_usb_.GetProto()->ops,
                         mock_usb_.GetProto()->ctx);
    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);
    incoming_.SyncCall([server = std::move(endpoints->server)](IncomingNamespace* infra) mutable {
      infra->fake_dev = std::make_shared<
          fake_usb_endpoint::FakeUsbFidlProvider<fuchsia_hardware_usb_function::UsbFunction>>(
          async_get_default_dispatcher());

      ASSERT_OK(
          infra->outgoing.template AddService<fuchsia_hardware_usb_function::UsbFunctionService>(
              fuchsia_hardware_usb_function::UsbFunctionService::InstanceHandler({
                  .device = infra->fake_dev->bind_handler(async_get_default_dispatcher()),
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
    adb_loop_ = std::make_unique<async::Loop>(&kAsyncLoopConfigNeverAttachToThread);
    auto adb = std::make_unique<UsbAdbDevice>(parent_.get(), adb_loop_->dispatcher(), &stop_sync_);
    auto dev = adb.get();
    incoming_.SyncCall([](IncomingNamespace* infra) {
      infra->fake_dev->ExpectConnectToEndpoint(kBulkOutEp);
      infra->fake_dev->ExpectConnectToEndpoint(kBulkInEp);
    });
    // adb_loop_ needs to run for a bit for Init() to be able to call RegisterVmos()
    adb_loop_->StartThread();
    ASSERT_OK(dev->Init());
    adb_loop_->ResetQuit();  // Stop threads so we can control adb_loop_ execution
    // Mock ddk owns the reference.
    [[maybe_unused]] auto released = adb.release();

    auto adb_ctxt = parent_->GetLatestChild()->GetDeviceContext<UsbAdbDevice>();
    ASSERT_EQ(dev, adb_ctxt);

    auto adb_endpoints = fidl::CreateEndpoints<fuchsia_hardware_adb::Device>();
    ASSERT_TRUE(adb_endpoints.is_ok());
    ASSERT_OK(fidl_loop_.StartThread("usb-adb-test-loop"));
    fidl::BindServer(fidl_loop_.dispatcher(), std::move(adb_endpoints->server), dev);
    adb_client_.Bind(std::move(adb_endpoints->client));
  }

  void TearDown() override {
    mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
    mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
    parent_->GetLatestChild()->SuspendNewOp(0, false, 0);
    if (configured_) {
      incoming_.SyncCall([&](IncomingNamespace* infra) {
        for (size_t i = 0; i < kBulkRxCount; i++) {
          infra->fake_dev->fake_endpoint(kBulkOutEp).RequestComplete(ZX_ERR_CANCELED, 0);
        }
      });
      adb_loop_->RunUntilIdle();
    }
    parent_->GetLatestChild()->WaitUntilSuspendReplyCalled();
    fidl_loop_.Shutdown();
    parent_ = nullptr;
    mock_usb_.VerifyAndClear();
  }

 protected:
  MockUsbFunction mock_usb_;
  std::shared_ptr<MockDevice> parent_;
  async::Loop fidl_loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::unique_ptr<async::Loop> adb_loop_;
  fidl::WireSyncClient<fuchsia_hardware_adb::Device> adb_client_;
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  bool configured_ = false;
  sync_completion_t stop_sync_;
};

// Fake Adb protocol service.
class FakeAdbDaemon : public fidl::WireAsyncEventHandler<fuchsia_hardware_adb::UsbAdbImpl> {
 public:
  explicit FakeAdbDaemon() : loop_(&kAsyncLoopConfigNeverAttachToThread) {
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_adb::UsbAdbImpl>();
    ASSERT_TRUE(endpoints.is_ok());
    client_ = fidl::WireSharedClient<fuchsia_hardware_adb::UsbAdbImpl>(std::move(endpoints->client),
                                                                       loop_.dispatcher(), this);
    server_ = std::move(endpoints->server);
  }

  ~FakeAdbDaemon() { loop_.Shutdown(); }

  fidl::ServerEnd<fuchsia_hardware_adb::UsbAdbImpl>&& GetServer() { return std::move(server_); }

  void OnStatusChanged(
      fidl::WireEvent<fuchsia_hardware_adb::UsbAdbImpl::OnStatusChanged>* event) override {
    status_ = event->status;
  }

  async::Loop& loop() { return loop_; }
  fuchsia_hardware_adb::StatusFlags status() { return status_; }
  fidl::WireSharedClient<fuchsia_hardware_adb::UsbAdbImpl>& client() { return client_; }

 private:
  fidl::WireSharedClient<fuchsia_hardware_adb::UsbAdbImpl> client_;
  fuchsia_hardware_adb::StatusFlags status_;
  async::Loop loop_;
  fidl::ServerEnd<fuchsia_hardware_adb::UsbAdbImpl> server_;
};

TEST_F(UsbAdbTest, SetUpTearDown) { ASSERT_NO_FATAL_FAILURE(); }

TEST_F(UsbAdbTest, StartStop) {
  auto fake_adb = std::make_unique<FakeAdbDaemon>();
  ASSERT_NO_FATAL_FAILURE();
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  ASSERT_OK(adb_client_->Start(std::move(fake_adb->GetServer())));

  // Calls during Stop().
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  // Close fake_adb so that Stop() will be invoked.
  fake_adb.reset();
  adb_loop_->RunUntilIdle();
  sync_completion_wait(&stop_sync_, ZX_TIME_INFINITE);
}

TEST_F(UsbAdbTest, SendAdbMessage) {
  auto fake_adb = std::make_unique<FakeAdbDaemon>();
  ASSERT_NO_FATAL_FAILURE();

  // Start adb transactions.
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  ASSERT_OK(adb_client_->Start(std::move(fake_adb->GetServer())));

  // Call set_configured of usb adb to bring the interface online.
  mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
  mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
  mock_usb_.function.ops->set_configured(mock_usb_.function.ctx, true, USB_SPEED_FULL);
  configured_ = true;
  // Run loop to send event
  adb_loop_->RunUntilIdle();
  // Run loop to receive event
  while (fake_adb->status() != fuchsia_hardware_adb::StatusFlags::kOnline) {
    EXPECT_EQ(ZX_OK, fake_adb->loop().RunUntilIdle());
  }

  // Queue transaction and check that the request is passed down the driver stack.
  uint8_t test_data[] = "test-data";
  sync_completion_t completion;
  ASSERT_OK(fake_adb->loop().StartThread("adb-send-thread"));
  fake_adb->client()
      ->QueueTx(fidl::VectorView<uint8_t>::FromExternal(test_data, sizeof(test_data)))
      .ThenExactlyOnce(
          [&](fidl::WireUnownedResult<fuchsia_hardware_adb::UsbAdbImpl::QueueTx>& response) {
            ASSERT_TRUE(response->is_ok());
            sync_completion_signal(&completion);
          });
  adb_loop_->RunUntilIdle();
  sync_completion_wait(&completion, zx::duration::infinite().get());

  incoming_.SyncCall([&](IncomingNamespace* infra) {
    infra->fake_dev->fake_endpoint(kBulkInEp).RequestComplete(ZX_ERR_CANCELED, 0);
  });
  adb_loop_->RunUntilIdle();

  // Calls during Stop().
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  // Close fake_adb so that Stop() will be invoked.
  fake_adb.reset();
  adb_loop_->RunUntilIdle();
  sync_completion_wait(&stop_sync_, ZX_TIME_INFINITE);
}

TEST_F(UsbAdbTest, RecvAdbMessage) {
  // Call set_configured of usb adb.
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  mock_usb_.function.ops->set_configured(mock_usb_.function.ctx, true, USB_SPEED_FULL);
  configured_ = true;

  auto fake_adb = std::make_unique<FakeAdbDaemon>();
  ASSERT_NO_FATAL_FAILURE();

  // Start adb transactions. This will also result in endpoint configuration.
  mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
  mock_usb_.ExpectConfigEp(ZX_OK, {}, {});
  ASSERT_OK(adb_client_->Start(std::move(fake_adb->GetServer())));

  // Run loop to send event
  adb_loop_->RunUntilIdle();
  // Run loop to receive event
  while (fake_adb->status() != fuchsia_hardware_adb::StatusFlags::kOnline) {
    EXPECT_EQ(ZX_OK, fake_adb->loop().RunUntilIdle());
  }

  // Queue a receive request before the data is available. The request will not get an immediate
  // reply. Queue a Receive request.
  const uint8_t test_data[] = "test-data";
  sync_completion_t completion;
  ASSERT_OK(fake_adb->loop().StartThread("adb-recv-thread"));
  fake_adb->client()->Receive().ThenExactlyOnce(
      [&](fidl::WireUnownedResult<::fuchsia_hardware_adb::UsbAdbImpl::Receive>& response) -> void {
        ASSERT_OK(response.status());
        ASSERT_FALSE(response.value().is_error());
        ASSERT_EQ(response.value().value()->data.count(), sizeof(test_data));
        sync_completion_signal(&completion);
      });
  // Invoke request completion on bulk out endpoint.
  incoming_.SyncCall([&](IncomingNamespace* infra) {
    infra->fake_dev->fake_endpoint(kBulkOutEp).RequestComplete(ZX_OK, sizeof(test_data));
  });

  // Process the Receive request and wait for completion.
  adb_loop_->RunUntilIdle();
  sync_completion_wait(&completion, zx::duration::infinite().get());

  // Calls during Stop().
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb_.ExpectDisableEp(ZX_OK, kBulkInEp);
  // Close fake_adb so that Stop() will be invoked.
  fake_adb.reset();
  adb_loop_->RunUntilIdle();
  sync_completion_wait(&stop_sync_, ZX_TIME_INFINITE);
}

}  // namespace usb_adb_function
