// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "usb-endpoint/usb-endpoint-server.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/fake-bti/bti.h>

#include <zxtest/zxtest.h>

namespace {

class FakeEndpoint : public usb_endpoint::UsbEndpoint {
 public:
  FakeEndpoint(const zx::bti& bti, uint8_t ep_addr,
               fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> server)
      : usb_endpoint::UsbEndpoint(bti, ep_addr) {
    loop_.StartThread("fake-endpoint-loop");
    Connect(loop_.dispatcher(), std::move(server));
  }

  // fuchsia_hardware_usb_new.Endpoint protocol implementation.
  void GetInfo(GetInfoCompleter::Sync& completer) override {
    completer.Reply(fit::as_error(ZX_ERR_NOT_SUPPORTED));
  }
  void QueueRequests(QueueRequestsRequest& request,
                     QueueRequestsCompleter::Sync& completer) override {}
  void CancelAll(CancelAllCompleter::Sync& completer) override {
    completer.Reply(fit::as_error(ZX_ERR_NOT_SUPPORTED));
  }

  sync_completion_t unbound_;

 private:
  void OnUnbound(fidl::UnbindInfo info,
                 fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> server_end) override {
    usb_endpoint::UsbEndpoint::OnUnbound(info, std::move(server_end));
    sync_completion_signal(&unbound_);
  }

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

class UsbEndpointServerTest : public zxtest::Test {
 public:
  void SetUp() override {
    client_loop_.StartThread("client-loop");
    ASSERT_OK(fake_bti_create(fake_bti_.reset_and_get_address()));

    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_usb_endpoint::Endpoint>();
    ASSERT_TRUE(endpoints.is_ok());
    ep_ = std::make_unique<FakeEndpoint>(fake_bti_, 0, std::move(endpoints->server));

    client_.Bind(std::move(endpoints->client), client_loop_.dispatcher(), &event_handler_);
  }

  void VerifyRegisteredVmos(size_t count) {
    size_t actual;
    EXPECT_OK(fake_bti_get_pinned_vmos(fake_bti_.get(), nullptr, 0, &actual));
    EXPECT_EQ(actual, count);
  }

 protected:
  async::Loop client_loop_{&kAsyncLoopConfigNeverAttachToThread};
  zx::bti fake_bti_;
  std::unique_ptr<FakeEndpoint> ep_;
  fidl::SharedClient<fuchsia_hardware_usb_endpoint::Endpoint> client_;

  class EventHandler : public fidl::AsyncEventHandler<fuchsia_hardware_usb_endpoint::Endpoint> {
   public:
    void OnCompletion(
        fidl::Event<fuchsia_hardware_usb_endpoint::Endpoint::OnCompletion>& event) override {
      completion_count_++;
      request_count_ += static_cast<uint32_t>(event.completion().size());
      sync_completion_signal(&received_on_completion_);
    }

    sync_completion_t received_on_completion_;
    std::atomic_uint32_t completion_count_;
    std::atomic_uint32_t request_count_;
  };
  EventHandler event_handler_;
};

TEST_F(UsbEndpointServerTest, RegisterVmosTest) {
  std::vector<fuchsia_hardware_usb_endpoint::VmoInfo> vmo_info;
  vmo_info.emplace_back(std::move(fuchsia_hardware_usb_endpoint::VmoInfo().id(8).size(32)));
  sync_completion_t wait;
  client_->RegisterVmos({std::move(vmo_info)})
      .Then([&](const fidl::Result<fuchsia_hardware_usb_endpoint::Endpoint::RegisterVmos>& result) {
        ASSERT_TRUE(result.is_ok());
        EXPECT_EQ(result->vmos().size(), 1);
        EXPECT_EQ(result->vmos().at(0).id(), 8);
        sync_completion_signal(&wait);
      });
  sync_completion_wait(&wait, zx::time::infinite().get());

  VerifyRegisteredVmos(1);
  auto tmp_req = fuchsia_hardware_usb_request::Request();
  tmp_req.data()
      .emplace()
      .emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(8))
      .offset(0)
      .size(32);
  auto req_var = usb_endpoint::RequestVariant(usb::FidlRequest(std::move(tmp_req)));
  auto iters = ep_->get_iter(req_var, zx_system_get_page_size());
  ASSERT_TRUE(iters.is_ok());
  EXPECT_EQ(iters->size(), 1);
  EXPECT_EQ((*iters->at(0).begin()).second, 32);
}

TEST_F(UsbEndpointServerTest, RegisterMultipleVmosTest) {
  std::vector<fuchsia_hardware_usb_endpoint::VmoInfo> vmo_info;
  vmo_info.emplace_back(std::move(fuchsia_hardware_usb_endpoint::VmoInfo().id(8).size(32)));
  vmo_info.emplace_back(std::move(fuchsia_hardware_usb_endpoint::VmoInfo().id(5).size(16)));
  vmo_info.emplace_back(std::move(fuchsia_hardware_usb_endpoint::VmoInfo().id(8).size(48)));
  sync_completion_t wait;
  client_->RegisterVmos({std::move(vmo_info)})
      .Then([&](const fidl::Result<fuchsia_hardware_usb_endpoint::Endpoint::RegisterVmos>& result) {
        ASSERT_TRUE(result.is_ok());
        EXPECT_EQ(result->vmos().size(), 2);
        EXPECT_EQ(result->vmos().at(0).id(), 8);
        EXPECT_EQ(result->vmos().at(1).id(), 5);
        sync_completion_signal(&wait);
      });
  sync_completion_wait(&wait, zx::time::infinite().get());

  VerifyRegisteredVmos(2);
  auto tmp_req = fuchsia_hardware_usb_request::Request();
  tmp_req.data()
      .emplace()
      .emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(8))
      .offset(0)
      .size(32);
  tmp_req.data()
      ->emplace_back()
      .buffer(fuchsia_hardware_usb_request::Buffer::WithVmoId(5))
      .offset(0)
      .size(16);
  auto req_var = usb_endpoint::RequestVariant(usb::FidlRequest(std::move(tmp_req)));
  auto iters = ep_->get_iter(req_var, zx_system_get_page_size());
  ASSERT_TRUE(iters.is_ok());
  EXPECT_EQ(iters->size(), 2);
  EXPECT_EQ((*iters->at(0).begin()).second, 32);
  EXPECT_EQ((*iters->at(1).begin()).second, 16);
}

TEST_F(UsbEndpointServerTest, UnregisterVmosTest) {
  std::vector<fuchsia_hardware_usb_endpoint::VmoInfo> vmo_info;
  vmo_info.emplace_back(std::move(fuchsia_hardware_usb_endpoint::VmoInfo().id(8).size(32)));
  sync_completion_t wait;
  client_->RegisterVmos({std::move(vmo_info)})
      .Then([&](const fidl::Result<fuchsia_hardware_usb_endpoint::Endpoint::RegisterVmos>& result) {
        ASSERT_TRUE(result.is_ok());
        sync_completion_signal(&wait);
      });
  sync_completion_wait(&wait, zx::time::infinite().get());
  sync_completion_reset(&wait);

  VerifyRegisteredVmos(1);

  client_->UnregisterVmos(std::vector<uint64_t>{4})
      .Then(
          [&](const fidl::Result<fuchsia_hardware_usb_endpoint::Endpoint::UnregisterVmos>& result) {
            ASSERT_TRUE(result.is_ok());
            EXPECT_EQ(result->failed_vmo_ids().size(), 1);
            EXPECT_EQ(result->failed_vmo_ids().at(0), 4);
            sync_completion_signal(&wait);
          });
  sync_completion_wait(&wait, zx::time::infinite().get());
  sync_completion_reset(&wait);

  VerifyRegisteredVmos(1);

  client_->UnregisterVmos(std::vector<uint64_t>{8, 3})
      .Then(
          [&](const fidl::Result<fuchsia_hardware_usb_endpoint::Endpoint::UnregisterVmos>& result) {
            ASSERT_TRUE(result.is_ok());
            EXPECT_EQ(result->failed_vmo_ids().size(), 1);
            EXPECT_EQ(result->failed_vmo_ids().at(0), 3);
            sync_completion_signal(&wait);
          });
  sync_completion_wait(&wait, zx::time::infinite().get());
  sync_completion_reset(&wait);

  VerifyRegisteredVmos(0);
}

TEST_F(UsbEndpointServerTest, UnboundTest) {
  std::vector<fuchsia_hardware_usb_endpoint::VmoInfo> vmo_info;
  vmo_info.emplace_back(std::move(fuchsia_hardware_usb_endpoint::VmoInfo().id(8).size(32)));
  sync_completion_t wait;
  client_->RegisterVmos({std::move(vmo_info)})
      .Then([&](const fidl::Result<fuchsia_hardware_usb_endpoint::Endpoint::RegisterVmos>& result) {
        ASSERT_TRUE(result.is_ok());
        sync_completion_signal(&wait);
      });
  sync_completion_wait(&wait, zx::time::infinite().get());

  VerifyRegisteredVmos(1);

  // Trigger unbind
  { auto unused = std::move(client_); }

  sync_completion_wait(&ep_->unbound_, zx::time::infinite().get());
  VerifyRegisteredVmos(0);
}

TEST_F(UsbEndpointServerTest, RequestCompleteFidlTest) {
  ep_->RequestComplete(
      ZX_OK, 0,
      usb::FidlRequest(std::move(fuchsia_hardware_usb_request::Request().defer_completion(false))));
  sync_completion_wait(&event_handler_.received_on_completion_, zx::time::infinite().get());
  EXPECT_EQ(event_handler_.completion_count_.load(), 1);
  EXPECT_EQ(event_handler_.request_count_.load(), 1);
  sync_completion_reset(&event_handler_.received_on_completion_);

  ep_->RequestComplete(
      ZX_OK, 0,
      usb::FidlRequest(std::move(fuchsia_hardware_usb_request::Request().defer_completion(false))));
  sync_completion_wait(&event_handler_.received_on_completion_, zx::time::infinite().get());
  EXPECT_EQ(event_handler_.completion_count_.load(), 2);
  EXPECT_EQ(event_handler_.request_count_.load(), 2);
  sync_completion_reset(&event_handler_.received_on_completion_);
}

TEST_F(UsbEndpointServerTest, RequestCompleteDeferredFidlTest) {
  ep_->RequestComplete(
      ZX_OK, 5,
      usb::FidlRequest(std::move(fuchsia_hardware_usb_request::Request().defer_completion(true))));

  ep_->RequestComplete(
      ZX_OK, 9,
      usb::FidlRequest(std::move(fuchsia_hardware_usb_request::Request().defer_completion(true))));

  ep_->RequestComplete(
      ZX_ERR_INTERNAL, 0,
      usb::FidlRequest(std::move(fuchsia_hardware_usb_request::Request().defer_completion(true))));

  sync_completion_wait(&event_handler_.received_on_completion_, zx::time::infinite().get());
  EXPECT_EQ(event_handler_.completion_count_.load(), 1);
  EXPECT_EQ(event_handler_.request_count_.load(), 3);
  sync_completion_reset(&event_handler_.received_on_completion_);

  ep_->RequestComplete(
      ZX_OK, 15,
      usb::FidlRequest(std::move(fuchsia_hardware_usb_request::Request().defer_completion(true))));

  ep_->RequestComplete(
      ZX_OK, 3,
      usb::FidlRequest(std::move(fuchsia_hardware_usb_request::Request().defer_completion(false))));

  sync_completion_wait(&event_handler_.received_on_completion_, zx::time::infinite().get());
  EXPECT_EQ(event_handler_.completion_count_.load(), 2);
  EXPECT_EQ(event_handler_.request_count_.load(), 5);
  sync_completion_reset(&event_handler_.received_on_completion_);
}

TEST_F(UsbEndpointServerTest, RequestCompleteBanjoTest) {
  constexpr size_t kBaseReqSize = sizeof(usb_request_t);
  constexpr size_t kFirstLayerReqSize = usb::Request<void>::RequestSize(kBaseReqSize);

  bool called = false;
  auto callback = [](void* ctx, usb_request_t* request) {
    *static_cast<bool*>(ctx) = true;
    // We take ownership.
    usb::Request<void> unused(request, kBaseReqSize);
  };
  usb_request_complete_callback_t complete_cb = {
      .callback = callback,
      .ctx = &called,
  };

  std::optional<usb::Request<void>> request;
  ASSERT_EQ(usb::Request<void>::Alloc(&request, 0, 0, kFirstLayerReqSize), ZX_OK);

  ep_->RequestComplete(ZX_OK, 0,
                       usb::BorrowedRequest<void>(request->take(), complete_cb, kBaseReqSize));
  EXPECT_TRUE(called);
}

}  // namespace
