// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.usb.phy/cpp/driver/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

#include <queue>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <usb-phy/usb-phy.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

class FakeUsbPhyServerBase {
 public:
  ~FakeUsbPhyServerBase() { EXPECT_TRUE(expected_connected_.empty()); }

  zx_status_t ConnectStatusChanged(bool connected) {
    fbl::AutoLock _(&lock_);
    EXPECT_GT(expected_connected_.size(), 0);
    EXPECT_EQ(expected_connected_.front(), connected);
    expected_connected_.pop();
    return ZX_OK;
  }
  void ExpectConnectStatusChanged(bool expected_connected) {
    fbl::AutoLock _(&lock_);
    expected_connected_.push(expected_connected);
  }

 private:
  fbl::Mutex lock_;
  std::queue<bool> expected_connected_ __TA_GUARDED(lock_);
};

template <class FakeUsbPhyServer>
class UsbPhyTest : public zxtest::Test {
 public:
  void GetClient(std::string_view fragment_name) {
    auto result = usb_phy::UsbPhyClient::Create(root_.get(), fragment_name);
    ASSERT_TRUE(result.is_ok());
    client_ = std::move(*result);
  }
  void GetClient() {
    auto result = usb_phy::UsbPhyClient::Create(root_.get());
    ASSERT_TRUE(result.is_ok());
    client_ = std::move(*result);
  }

  void TestConnectStatusChanged() {
    incoming_.SyncCall(
        [](IncomingNamespace* ns) { ns->fake_phy_server_.ExpectConnectStatusChanged(true); });
    client_->ConnectStatusChanged(true);

    incoming_.SyncCall(
        [](IncomingNamespace* ns) { ns->fake_phy_server_.ExpectConnectStatusChanged(false); });
    client_->ConnectStatusChanged(false);
  }

 protected:
  std::shared_ptr<zx_device_t> root_ = MockDevice::FakeRootParent();
  fdf::UnownedSynchronizedDispatcher dispatcher_ =
      fdf_testing::DriverRuntime::GetInstance()->StartBackgroundDispatcher();

  struct IncomingNamespace {
    fdf::OutgoingDirectory outgoing =
        fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get());
    FakeUsbPhyServer fake_phy_server_;
  };
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{dispatcher_->async_dispatcher(),
                                                                   std::in_place};
  std::optional<usb_phy::UsbPhyClient> client_;
};

// FIDL Tests.
namespace fidl_tests {

class FakeUsbPhyFidlServer : public FakeUsbPhyServerBase,
                             public fdf::Server<fuchsia_hardware_usb_phy::UsbPhy> {
 public:
  void ConnectStatusChanged(ConnectStatusChangedRequest& request,
                            ConnectStatusChangedCompleter::Sync& completer) override {
    auto status = FakeUsbPhyServerBase::ConnectStatusChanged(request.connected());
    if (status != ZX_OK) {
      completer.Reply(fit::error(status));
      return;
    }
    completer.Reply(fit::ok());
  }
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_usb_phy::UsbPhy> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    ASSERT_TRUE(false);
  }

  fuchsia_hardware_usb_phy::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_usb_phy::Service::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                          fidl::kIgnoreBindingClosure),
    });
  }

 private:
  fdf::ServerBindingGroup<fuchsia_hardware_usb_phy::UsbPhy> bindings_;
};

class UsbPhyFidlTest : public UsbPhyTest<FakeUsbPhyFidlServer> {
 public:
  void SetUpAsFragment() {
    auto io_eps = fidl::Endpoints<fuchsia_io::Directory>::Create();
    incoming_.SyncCall([&](IncomingNamespace* ns) {
      ASSERT_OK(ns->outgoing.AddService<fuchsia_hardware_usb_phy::Service>(
                    ns->fake_phy_server_.GetInstanceHandler()),
                "phy");
      ASSERT_OK(ns->outgoing.Serve(std::move(io_eps.server)));
    });
    root_->AddFidlService(fuchsia_hardware_usb_phy::Service::Name, std::move(io_eps.client), "phy");

    GetClient("phy");
  }

  void SetUpAsParent() {
    auto io_eps = fidl::Endpoints<fuchsia_io::Directory>::Create();
    incoming_.SyncCall([&](IncomingNamespace* ns) {
      ASSERT_OK(ns->outgoing.AddService<fuchsia_hardware_usb_phy::Service>(
          ns->fake_phy_server_.GetInstanceHandler()));
      ASSERT_OK(ns->outgoing.Serve(std::move(io_eps.server)));
    });
    root_->AddFidlService(fuchsia_hardware_usb_phy::Service::Name, std::move(io_eps.client));

    GetClient();
  }
};

TEST_F(UsbPhyFidlTest, AsFragment) {
  SetUpAsFragment();
  TestConnectStatusChanged();
}

TEST_F(UsbPhyFidlTest, AsParent) {
  SetUpAsParent();
  TestConnectStatusChanged();
}

}  // namespace fidl_tests

// Banjo Tests
namespace banjo_tests {

class FakeUsbPhyBanjoServer
    : public FakeUsbPhyServerBase,
      public ddk::UsbPhyProtocol<FakeUsbPhyBanjoServer, ddk::base_protocol> {
 public:
  void UsbPhyConnectStatusChanged(bool connected) {
    ASSERT_OK(FakeUsbPhyServerBase::ConnectStatusChanged(connected));
  }

  const usb_phy_protocol_ops_t* PhyOps() { return &usb_phy_protocol_ops_; }
};

class UsbPhyBanjoTest : public UsbPhyTest<FakeUsbPhyBanjoServer> {
 public:
  void SetUpAsFragment() {
    incoming_.SyncCall([&](IncomingNamespace* ns) {
      root_->AddProtocol(ZX_PROTOCOL_USB_PHY, ns->fake_phy_server_.PhyOps(), &ns->fake_phy_server_,
                         "phy");
    });

    GetClient("phy");
  }

  void SetUpAsParent() {
    incoming_.SyncCall([&](IncomingNamespace* ns) {
      root_->AddProtocol(ZX_PROTOCOL_USB_PHY, ns->fake_phy_server_.PhyOps(), &ns->fake_phy_server_);
    });

    GetClient();
  }
};

TEST_F(UsbPhyBanjoTest, AsFragment) {
  SetUpAsFragment();
  TestConnectStatusChanged();
}

TEST_F(UsbPhyBanjoTest, AsParent) {
  SetUpAsParent();
  TestConnectStatusChanged();
}

}  // namespace banjo_tests

}  // namespace
