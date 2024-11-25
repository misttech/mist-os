// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <fidl/fuchsia.hardware.spmi/cpp/test_base.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/namespace.h>

#include <zxtest/zxtest.h>

#include "lib/zx/result.h"
#include "spmi-ctl-impl.h"

class FakeSpmi : public fidl::testing::TestBase<fuchsia_hardware_spmi::Device>,
                 public fidl::Server<fuchsia_hardware_spmi::Debug> {
 public:
  explicit FakeSpmi(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  // FIDL natural C++ methods for fuchsia.hardware.spmi.
  void GetProperties(GetPropertiesCompleter::Sync& completer) override {
    fuchsia_hardware_spmi::DeviceGetPropertiesResponse response;
    response.sid(123);
    completer.Reply(std::move(response));
  }
  void ExtendedRegisterReadLong(ExtendedRegisterReadLongRequest& request,
                                ExtendedRegisterReadLongCompleter::Sync& completer) override {
    // Only allow reads on address written to.
    if (!address_ || *address_ != request.address()) {
      completer.Reply(zx::error(fuchsia_hardware_spmi::DriverError::kBadState));
      return;
    }
    read_size_ = request.size_bytes();
    return completer.Reply(zx::ok(data_));
  }
  void ExtendedRegisterWriteLong(ExtendedRegisterWriteLongRequest& request,
                                 ExtendedRegisterWriteLongCompleter::Sync& completer) override {
    address_.emplace(request.address());
    data_ = request.data();
    return completer.Reply(zx::ok());
  }
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_spmi::Device> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    FAIL();
  }

  void ConnectTarget(ConnectTargetRequest& request,
                     ConnectTargetCompleter::Sync& completer) override {
    if (request.target_id() >= fuchsia_hardware_spmi::kMaxTargets) {
      completer.Reply(fit::error(fuchsia_hardware_spmi::DriverError::kInvalidArgs));
      return;
    }
    target_id_ = request.target_id();
    device_bindings_.AddBinding(dispatcher_, std::move(request.server()), this,
                                fidl::kIgnoreBindingClosure);
    completer.Reply(zx::ok());
  }
  void GetControllerProperties(GetControllerPropertiesCompleter::Sync& completer) override {
    completer.Reply({{"spmi-controller"}});
  }
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_spmi::Debug> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  uint8_t target_id() const { return target_id_; }
  std::vector<uint8_t>& data() { return data_; }
  uint16_t address() { return *address_; }
  size_t read_size() { return read_size_; }

 private:
  async_dispatcher_t* const dispatcher_;
  uint8_t target_id_{fuchsia_hardware_spmi::kMaxTargets};
  std::optional<uint16_t> address_;
  std::vector<uint8_t> data_;
  size_t read_size_;
  fidl::ServerBindingGroup<fuchsia_hardware_spmi::Device> device_bindings_;
};

class SpmiCtlTest : public zxtest::Test {
 public:
  void SetUp() override {
    loop_ = std::make_unique<async::Loop>(&kAsyncLoopConfigAttachToCurrentThread);
    ASSERT_OK(loop_->StartThread("spmi-ctl-test-loop"));
    spmi_ = std::make_unique<FakeSpmi>(loop_->dispatcher());
  }

  void TearDown() override { loop_->Shutdown(); }

  int CallSpmiCtl(std::vector<std::string> args) {
    constexpr size_t kMaxArgs = 64;
    char* argv[kMaxArgs];
    ZX_ASSERT(args.size() <= kMaxArgs);
    for (size_t i = 0; i < args.size(); ++i) {
      argv[i] = const_cast<char*>(args[i].c_str());
    }
    fidl::ClientEnd<fuchsia_hardware_spmi::Debug> client;
    zx::result server = fidl::CreateEndpoints(&client);
    ZX_ASSERT(server.status_value() == ZX_OK);
    fidl::BindServer(loop_->dispatcher(), std::move(server.value()), spmi_.get());
    spmi_ctl_.emplace(SpmiCtl(std::move(client)));
    return spmi_ctl_->Execute(static_cast<int>(args.size()), argv);
  }

 protected:
  std::unique_ptr<async::Loop> loop_;
  std::unique_ptr<FakeSpmi> spmi_;
  std::optional<SpmiCtl> spmi_ctl_;
};

TEST_F(SpmiCtlTest, UnknownCommands) {
  EXPECT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "0", "-b"}), -1);
  EXPECT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "0", "--bad"}), -1);
}

TEST_F(SpmiCtlTest, InvalidTarget) {
  EXPECT_EQ(CallSpmiCtl({"spmi-ctl", "-a", "0x1234", "-r", "4"}), -1);
  EXPECT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "-a", "0x1234", "-r", "4"}), -1);
  EXPECT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "16", "-a", "0x1234", "-r", "4"}), -1);
  EXPECT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "100", "-a", "0x1234", "-r", "4"}), -1);
}

TEST_F(SpmiCtlTest, ReadWrite) {
  // Successful.
  std::vector<uint8_t> kCannedData = {0x12, 0x34, 0x56};
  ASSERT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "0", "-a", "0x1234", "-w", "0x12", "0x34", "0x56"}), 0);
  EXPECT_EQ(spmi_->target_id(), 0);
  EXPECT_TRUE(spmi_->data() == kCannedData);
  EXPECT_EQ(spmi_->address(), 0x1234);
  ASSERT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "11", "-a", "0x1234", "-r", "4"}), 0);
  EXPECT_EQ(spmi_->target_id(), 11);
  EXPECT_EQ(spmi_->read_size(), 4);
  EXPECT_EQ(spmi_->address(), 0x1234);
  ASSERT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "0", "-a", "0x1234", "-w", "1", "2", "3", "4", "5", "6",
                         "7", "8", "9"}),
            0);
  EXPECT_EQ(spmi_->target_id(), 0);
  kCannedData = {1, 2, 3, 4, 5, 6, 7, 8, 9};
  EXPECT_TRUE(spmi_->data() == kCannedData);
  EXPECT_EQ(spmi_->address(), 0x1234);
  ASSERT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "0", "-a", "0x1234", "-r", "9"}), 0);
  EXPECT_EQ(spmi_->target_id(), 0);
  EXPECT_EQ(spmi_->read_size(), 9);
  EXPECT_EQ(spmi_->address(), 0x1234);

  // Errors.
  ASSERT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "0", "-w", "0x12", "0x34", "0x56"}),
            -1);  // No address.
  ASSERT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "0", "-a", "0x5678", "-r", "4"}),
            -1);  // Unknown address.
  ASSERT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "0", "-a", "0x10000", "-r", "4"}),
            -1);                                                              // Address too big.
  ASSERT_EQ(CallSpmiCtl({"spmi-ctl", "-t", "0", "-a", "0x1234", "-w"}), -1);  // Write no data.
}
