// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPMI_TESTING_INCLUDE_LIB_MOCK_SPMI_MOCK_SPMI_H_
#define SRC_DEVICES_SPMI_TESTING_INCLUDE_LIB_MOCK_SPMI_MOCK_SPMI_H_

#include <fidl/fuchsia.hardware.spmi/cpp/fidl.h>
#include <fidl/fuchsia.hardware.spmi/cpp/test_base.h>

#include <optional>
#include <queue>

#include <gtest/gtest.h>

namespace mock_spmi {

namespace {

// To use switch like logic for std::visitor on std::variat.
template <class... Ts>
struct overloaded : Ts... {
  using Ts::operator()...;
};

}  // namespace

class MockSpmi : public fidl::testing::TestBase<fuchsia_hardware_spmi::Device> {
 public:
  void ExpectGetProperties(uint16_t sid, std::string name) {
    expectations_.push({
        .type = CallType::kGetProperties,
        .sid = sid,
        .name = name,
    });
  }

  void ExpectExtendedRegisterReadLong(uint16_t address, uint32_t size_bytes,
                                      std::vector<uint8_t> expected_data) {
    expectations_.push({
        .type = CallType::kRead,
        .address = address,
        .size_bytes = size_bytes,
        .data = std::move(expected_data),
    });
  }

  void ExpectExtendedRegisterReadLong(uint16_t address, uint32_t size_bytes,
                                      fuchsia_hardware_spmi::DriverError expected_error) {
    expectations_.push({
        .type = CallType::kRead,
        .error = expected_error,
        .address = address,
        .size_bytes = size_bytes,
    });
  }

  void ExpectExtendedRegisterWriteLong(
      uint16_t address, std::vector<uint8_t> data,
      std::optional<fuchsia_hardware_spmi::DriverError> expected_error = std::nullopt) {
    expectations_.push({
        .type = CallType::kWrite,
        .error = expected_error,
        .address = address,
        .data = std::move(data),
    });
  }

  void ExpectWatchControllerWriteCommands(uint8_t address, uint16_t data) {
    if (expected_watches_.empty() ||
        std::holds_alternative<std::vector<fuchsia_hardware_spmi::Register8>>(
            expected_watches_.front())) {
      expected_watches_.emplace(std::vector<fuchsia_hardware_spmi::Register8>{{address, data}});
      return;
    }

    auto& request = std::get<WatchControllerRequest>(expected_watches_.front());
    EXPECT_EQ(request.address, address);
    zx::eventpair e0, e1;
    EXPECT_EQ(zx::eventpair::create(0, &e0, &e1), ZX_OK);
    request.completer.Reply(
        zx::ok(fuchsia_hardware_spmi::DeviceWatchControllerWriteCommandsResponse{
            std::vector<fuchsia_hardware_spmi::Register8>{{address, data}},
            request.wake_lease_requested ? std::move(e0) : zx::eventpair{}}));
    expected_watches_.pop();
  }

  void VerifyAndClear() {
    EXPECT_TRUE(expectations_.empty());
    expectations_ = {};
  }

  fidl::ServerBindingGroup<fuchsia_hardware_spmi::Device> bindings_;

 private:
  enum CallType : uint8_t {
    kRead = 0,
    kWrite = 1,
    kGetProperties = 2,
  };

  struct SpmiExpectation {
    CallType type;

    std::optional<fuchsia_hardware_spmi::DriverError> error = std::nullopt;

    uint16_t address;
    uint32_t size_bytes;
    std::vector<uint8_t> data;

    uint16_t sid;
    std::string name;
  };

  void GetProperties(GetPropertiesCompleter::Sync& completer) override {
    ASSERT_FALSE(expectations_.empty());
    auto expectation = std::move(expectations_.front());
    expectations_.pop();

    ASSERT_EQ(expectation.type, CallType::kGetProperties);
    completer.Reply({{
        .sid = expectation.sid,
        .name = expectation.name,
    }});
  }

  void ExtendedRegisterReadLong(ExtendedRegisterReadLongRequest& request,
                                ExtendedRegisterReadLongCompleter::Sync& completer) override {
    ASSERT_FALSE(expectations_.empty());
    auto expectation = std::move(expectations_.front());
    expectations_.pop();

    ASSERT_EQ(expectation.type, CallType::kRead);
    EXPECT_EQ(expectation.address, request.address());
    EXPECT_EQ(expectation.size_bytes, request.size_bytes());
    EXPECT_EQ(expectation.size_bytes, expectation.data.size());
    if (expectation.error.has_value()) {
      completer.Reply(zx::error(expectation.error.value()));
    } else {
      completer.Reply(zx::ok(std::move(expectation.data)));
    }
  }

  void ExtendedRegisterWriteLong(ExtendedRegisterWriteLongRequest& request,
                                 ExtendedRegisterWriteLongCompleter::Sync& completer) override {
    ASSERT_FALSE(expectations_.empty());
    auto expectation = std::move(expectations_.front());
    expectations_.pop();

    ASSERT_EQ(expectation.type, CallType::kWrite);
    EXPECT_EQ(expectation.address, request.address());
    ASSERT_EQ(expectation.data.size(), request.data().size());
    EXPECT_EQ(expectation.data, std::vector<uint8_t>(request.data().begin(), request.data().end()));
    if (expectation.error.has_value()) {
      completer.Reply(zx::error(expectation.error.value()));
    } else {
      completer.Reply(zx::ok());
    }
  }

  void WatchControllerWriteCommands(
      WatchControllerWriteCommandsRequest& request,
      WatchControllerWriteCommandsCompleter::Sync& completer) override {
    EXPECT_EQ(request.size(), 1);

    if (expected_watches_.empty() ||
        std::holds_alternative<WatchControllerRequest>(expected_watches_.front())) {
      expected_watches_.emplace(
          WatchControllerRequest{.address = request.address(),
                                 .completer = completer.ToAsync(),
                                 .wake_lease_requested = request.setup_wake_lease().is_valid()});
      return;
    }

    auto& data = std::get<std::vector<fuchsia_hardware_spmi::Register8>>(expected_watches_.front());
    EXPECT_EQ(data.size(), 1);
    EXPECT_EQ(data[0].address(), request.address());

    zx::eventpair e0, e1;
    EXPECT_EQ(zx::eventpair::create(0, &e0, &e1), ZX_OK);
    completer.Reply(zx::ok(fuchsia_hardware_spmi::DeviceWatchControllerWriteCommandsResponse{
        std::move(data), request.setup_wake_lease().is_valid() ? std::move(e0) : zx::eventpair{}}));
    expected_watches_.pop();
  }

  void CancelWatchControllerWriteCommands(
      CancelWatchControllerWriteCommandsRequest& request,
      CancelWatchControllerWriteCommandsCompleter::Sync& completer) override {
    EXPECT_EQ(request.size(), 1);

    if (expected_watches_.empty()) {
      ZX_ASSERT(false);
      return;
    }

    std::visit(overloaded{[&](WatchControllerRequest& req) {
                            EXPECT_EQ(req.address, request.address());
                            req.completer.Reply(zx::error(ZX_ERR_CANCELED));
                            expected_watches_.pop();
                          },
                          [](std::vector<fuchsia_hardware_spmi::Register8>&) { ZX_ASSERT(false); }},
               expected_watches_.front());
    completer.Reply(zx::ok());
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_spmi::Device> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {
    ASSERT_TRUE(false);
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    FAIL();
  }

  std::queue<SpmiExpectation> expectations_;
  struct WatchControllerRequest {
    uint8_t address;
    WatchControllerWriteCommandsCompleter::Async completer;
    bool wake_lease_requested;
  };
  std::queue<std::variant<WatchControllerRequest, std::vector<fuchsia_hardware_spmi::Register8>>>
      expected_watches_;
};

}  // namespace mock_spmi

#endif  // SRC_DEVICES_SPMI_TESTING_INCLUDE_LIB_MOCK_SPMI_MOCK_SPMI_H_
