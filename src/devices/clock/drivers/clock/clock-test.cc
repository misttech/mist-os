// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "clock.h"

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fuchsia/hardware/clockimpl/cpp/banjo.h>
#include <lib/ddk/metadata.h>
#include <lib/stdcompat/span.h>

#include <array>
#include <optional>

#include <zxtest/zxtest.h>

#include "sdk/lib/async_patterns/testing/cpp/dispatcher_bound.h"
#include "sdk/lib/driver/testing/cpp/driver_runtime.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

class FakeClockImpl : public ddk::ClockImplProtocol<FakeClockImpl>,
                      public fdf::WireServer<fuchsia_hardware_clockimpl::ClockImpl> {
 public:
  struct FakeClock {
    std::optional<bool> enabled;
    std::optional<uint64_t> rate_hz;
    std::optional<uint32_t> input_idx;
  };

  zx_status_t ClockImplEnable(uint32_t id) {
    if (id >= clocks_.size()) {
      return ZX_ERR_OUT_OF_RANGE;
    }
    clocks_[id].enabled.emplace(true);
    return ZX_OK;
  }

  zx_status_t ClockImplDisable(uint32_t id) {
    if (id >= clocks_.size()) {
      return ZX_ERR_OUT_OF_RANGE;
    }
    clocks_[id].enabled.emplace(false);
    return ZX_OK;
  }

  zx_status_t ClockImplIsEnabled(uint32_t id, bool* out_enabled) { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t ClockImplSetRate(uint32_t id, uint64_t hz) {
    if (id >= clocks_.size()) {
      return ZX_ERR_OUT_OF_RANGE;
    }
    clocks_[id].rate_hz.emplace(hz);
    return ZX_OK;
  }

  zx_status_t ClockImplQuerySupportedRate(uint32_t id, uint64_t hz, uint64_t* out_hz) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t ClockImplGetRate(uint32_t id, uint64_t* out_hz) { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t ClockImplSetInput(uint32_t id, uint32_t idx) {
    if (id >= clocks_.size()) {
      return ZX_ERR_OUT_OF_RANGE;
    }
    clocks_[id].input_idx.emplace(idx);
    return ZX_OK;
  }

  zx_status_t ClockImplGetNumInputs(uint32_t id, uint32_t* out_n) { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t ClockImplGetInput(uint32_t id, uint32_t* out_index) { return ZX_ERR_NOT_SUPPORTED; }

  const clock_impl_protocol_ops_t* ops() const { return &clock_impl_protocol_ops_; }

  cpp20::span<const FakeClock> clocks() const { return {clocks_.data(), clocks_.size()}; }

 private:
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_clockimpl::ClockImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void Enable(fuchsia_hardware_clockimpl::wire::ClockImplEnableRequest* request, fdf::Arena& arena,
              EnableCompleter::Sync& completer) override {
    if (zx_status_t status = ClockImplEnable(request->id); status == ZX_OK) {
      completer.buffer(arena).ReplySuccess();
    } else {
      completer.buffer(arena).ReplyError(status);
    }
  }

  void Disable(fuchsia_hardware_clockimpl::wire::ClockImplDisableRequest* request,
               fdf::Arena& arena, DisableCompleter::Sync& completer) override {
    if (zx_status_t status = ClockImplEnable(request->id); status == ZX_OK) {
      completer.buffer(arena).ReplySuccess();
    } else {
      completer.buffer(arena).ReplyError(status);
    }
  }

  void IsEnabled(fuchsia_hardware_clockimpl::wire::ClockImplIsEnabledRequest* request,
                 fdf::Arena& arena, IsEnabledCompleter::Sync& completer) override {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void SetRate(fuchsia_hardware_clockimpl::wire::ClockImplSetRateRequest* request,
               fdf::Arena& arena, SetRateCompleter::Sync& completer) override {
    if (zx_status_t status = ClockImplSetRate(request->id, request->hz); status == ZX_OK) {
      completer.buffer(arena).ReplySuccess();
    } else {
      completer.buffer(arena).ReplyError(status);
    }
  }

  void QuerySupportedRate(
      fuchsia_hardware_clockimpl::wire::ClockImplQuerySupportedRateRequest* request,
      fdf::Arena& arena, QuerySupportedRateCompleter::Sync& completer) override {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetRate(fuchsia_hardware_clockimpl::wire::ClockImplGetRateRequest* request,
               fdf::Arena& arena, GetRateCompleter::Sync& completer) override {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void SetInput(fuchsia_hardware_clockimpl::wire::ClockImplSetInputRequest* request,
                fdf::Arena& arena, SetInputCompleter::Sync& completer) override {
    if (zx_status_t status = ClockImplSetRate(request->id, request->idx); status == ZX_OK) {
      completer.buffer(arena).ReplySuccess();
    } else {
      completer.buffer(arena).ReplyError(status);
    }
  }

  void GetNumInputs(fuchsia_hardware_clockimpl::wire::ClockImplGetNumInputsRequest* request,
                    fdf::Arena& arena, GetNumInputsCompleter::Sync& completer) override {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetInput(fuchsia_hardware_clockimpl::wire::ClockImplGetInputRequest* request,
                fdf::Arena& arena, GetInputCompleter::Sync& completer) override {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  std::array<FakeClock, 6> clocks_;
};

TEST(ClockTest, ConfigureClocks) {
  fidl::Arena arena;

  FakeClockImpl clock_impl;

  std::shared_ptr fake_parent = MockDevice::FakeRootParent();

  fake_parent->AddProtocol(ZX_PROTOCOL_CLOCK_IMPL, clock_impl.ops(), &clock_impl);
  fake_parent->SetMetadata(DEVICE_METADATA_CLOCK_IDS, nullptr, 0);

  fuchsia_hardware_clockimpl::wire::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fuchsia_hardware_clockimpl::wire::InitStep>(arena, 12);

  metadata.steps[0] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(3)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithEnable({}))
                          .Build();

  metadata.steps[0] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(3)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithEnable({}))
                          .Build();
  metadata.steps[1] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(3)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithInputIdx(100))
                          .Build();
  metadata.steps[2] =
      fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
          .id(3)
          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithRateHz(arena, 500'000'000))
          .Build();

  metadata.steps[3] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(1)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithEnable({}))
                          .Build();
  metadata.steps[4] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(1)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithInputIdx(99))
                          .Build();
  metadata.steps[5] =
      fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
          .id(1)
          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithRateHz(arena, 400'000'000))
          .Build();

  metadata.steps[6] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(1)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithDisable({}))
                          .Build();
  metadata.steps[7] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(1)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithInputIdx(101))
                          .Build();
  metadata.steps[8] =
      fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
          .id(1)
          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithRateHz(arena, 600'000'000))
          .Build();

  metadata.steps[9] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(2)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithDisable({}))
                          .Build();
  metadata.steps[10] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                           .id(2)
                           .call(fuchsia_hardware_clockimpl::wire::InitCall::WithInputIdx(1))
                           .Build();

  metadata.steps[11] =
      fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
          .id(4)
          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithRateHz(arena, 100'000))
          .Build();

  const fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());
  const std::vector<uint8_t>& message = encoded.value();

  fake_parent->SetMetadata(DEVICE_METADATA_CLOCK_INIT, message.data(), message.size());

  EXPECT_OK(ClockDevice::Create(nullptr, fake_parent.get()));

  EXPECT_EQ(fake_parent->child_count(), 1);

  ASSERT_TRUE(clock_impl.clocks()[3].enabled.has_value());
  EXPECT_TRUE(clock_impl.clocks()[3].enabled.value());

  ASSERT_TRUE(clock_impl.clocks()[3].input_idx.has_value());
  EXPECT_EQ(clock_impl.clocks()[3].input_idx.value(), 100);

  ASSERT_TRUE(clock_impl.clocks()[3].rate_hz.has_value());
  EXPECT_EQ(clock_impl.clocks()[3].rate_hz.value(), 500'000'000);

  ASSERT_TRUE(clock_impl.clocks()[1].enabled.has_value());
  EXPECT_FALSE(clock_impl.clocks()[1].enabled.value());

  ASSERT_TRUE(clock_impl.clocks()[1].input_idx.has_value());
  EXPECT_EQ(clock_impl.clocks()[1].input_idx.value(), 101);

  ASSERT_TRUE(clock_impl.clocks()[1].rate_hz.has_value());
  EXPECT_EQ(clock_impl.clocks()[1].rate_hz.value(), 600'000'000);

  ASSERT_TRUE(clock_impl.clocks()[2].enabled.has_value());
  EXPECT_FALSE(clock_impl.clocks()[2].enabled.value());

  ASSERT_TRUE(clock_impl.clocks()[2].input_idx.has_value());
  EXPECT_EQ(clock_impl.clocks()[2].input_idx.value(), 1);

  EXPECT_FALSE(clock_impl.clocks()[2].rate_hz.has_value());

  ASSERT_TRUE(clock_impl.clocks()[4].rate_hz.has_value());
  EXPECT_EQ(clock_impl.clocks()[4].rate_hz.value(), 100'000);

  EXPECT_FALSE(clock_impl.clocks()[4].enabled.has_value());
  EXPECT_FALSE(clock_impl.clocks()[4].input_idx.has_value());

  EXPECT_FALSE(clock_impl.clocks()[0].enabled.has_value());
  EXPECT_FALSE(clock_impl.clocks()[0].rate_hz.has_value());
  EXPECT_FALSE(clock_impl.clocks()[0].input_idx.has_value());

  EXPECT_FALSE(clock_impl.clocks()[5].enabled.has_value());
  EXPECT_FALSE(clock_impl.clocks()[5].rate_hz.has_value());
  EXPECT_FALSE(clock_impl.clocks()[5].input_idx.has_value());
}

TEST(ClockTest, ConfigureClocksError) {
  fidl::Arena arena;

  FakeClockImpl clock_impl;

  std::shared_ptr fake_parent = MockDevice::FakeRootParent();

  fake_parent->AddProtocol(ZX_PROTOCOL_CLOCK_IMPL, clock_impl.ops(), &clock_impl);
  fake_parent->SetMetadata(DEVICE_METADATA_CLOCK_IDS, nullptr, 0);

  fuchsia_hardware_clockimpl::wire::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fuchsia_hardware_clockimpl::wire::InitStep>(arena, 9);

  metadata.steps[0] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(3)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithEnable({}))
                          .Build();
  metadata.steps[1] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(3)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithInputIdx(100))
                          .Build();
  metadata.steps[2] =
      fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
          .id(3)
          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithRateHz(arena, 500'000'000))
          .Build();

  metadata.steps[3] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(1)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithEnable({}))
                          .Build();

  // This step should return an error due to the clock index being out of range.
  metadata.steps[4] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(10)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithInputIdx(99))
                          .Build();

  metadata.steps[5] =
      fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
          .id(1)
          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithRateHz(arena, 400'000'000))
          .Build();

  metadata.steps[6] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(2)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithDisable({}))
                          .Build();
  metadata.steps[7] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(2)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithInputIdx(1))
                          .Build();

  metadata.steps[8] =
      fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
          .id(4)
          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithRateHz(arena, 100'000))
          .Build();

  const fit::result encoded = fidl::Persist(metadata);
  ASSERT_TRUE(encoded.is_ok());
  const std::vector<uint8_t>& message = encoded.value();

  fake_parent->SetMetadata(DEVICE_METADATA_CLOCK_INIT, message.data(), message.size());

  EXPECT_OK(ClockDevice::Create(nullptr, fake_parent.get()));

  EXPECT_EQ(fake_parent->child_count(), 0);

  ASSERT_TRUE(clock_impl.clocks()[3].enabled.has_value());
  EXPECT_TRUE(clock_impl.clocks()[3].enabled.value());

  ASSERT_TRUE(clock_impl.clocks()[3].input_idx.has_value());
  EXPECT_EQ(clock_impl.clocks()[3].input_idx.value(), 100);

  ASSERT_TRUE(clock_impl.clocks()[3].rate_hz.has_value());
  EXPECT_EQ(clock_impl.clocks()[3].rate_hz.value(), 500'000'000);

  ASSERT_TRUE(clock_impl.clocks()[1].enabled.has_value());
  EXPECT_TRUE(clock_impl.clocks()[1].enabled.value());

  // None of the steps after the error should be executed.

  EXPECT_FALSE(clock_impl.clocks()[1].rate_hz.has_value());
  EXPECT_FALSE(clock_impl.clocks()[2].enabled.has_value());
  EXPECT_FALSE(clock_impl.clocks()[2].input_idx.has_value());
  EXPECT_FALSE(clock_impl.clocks()[4].rate_hz.has_value());
}

TEST(ClockTest, BanjoPreferredOverFidl) {
  constexpr uint32_t kTestClockId = 3;

  fdf_testing::DriverRuntime runtime;

  FakeClockImpl clockimpl_banjo;
  FakeClockImpl clockimpl_fidl;

  zx::result clockimpl_endpoints = fdf::CreateEndpoints<fuchsia_hardware_clockimpl::ClockImpl>();
  ASSERT_TRUE(clockimpl_endpoints.is_ok());
  fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(clockimpl_endpoints->server),
                  &clockimpl_fidl);

  const clock_impl_protocol_t clockimpl_banjo_proto{clockimpl_banjo.ops(), &clockimpl_banjo};
  ClockImplProxy proxy(&clockimpl_banjo_proto, std::move(clockimpl_endpoints->client));
  ClockDevice dut(nullptr, std::move(proxy), kTestClockId);

  auto clock_endpoints = fidl::Endpoints<fuchsia_hardware_clock::Clock>::Create();

  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                   std::move(clock_endpoints.server), &dut);

  fidl::WireClient<fuchsia_hardware_clock::Clock> clock_client(
      std::move(clock_endpoints.client), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  clock_client->SetRate(1'000'000).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_clock::Clock::SetRate>& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime.Quit();
      });

  // Run the foreground dispatcher until the callback stops it.
  runtime.Run();

  // The dut was provided with Banjo and FIDL clients, but only the Banjo client should receive this
  // call.
  ASSERT_TRUE(clockimpl_banjo.clocks()[kTestClockId].rate_hz);
  EXPECT_EQ(*clockimpl_banjo.clocks()[kTestClockId].rate_hz, 1'000'000);

  EXPECT_FALSE(clockimpl_fidl.clocks()[kTestClockId].rate_hz);
}

TEST(ClockTest, FallBackToFidl) {
  constexpr uint32_t kTestClockId = 3;

  fdf_testing::DriverRuntime runtime;
  async_dispatcher_t* background_dispatcher =
      runtime.StartBackgroundDispatcher()->async_dispatcher();

  // Bind the FakeClockImpl to the foreground dispatcher, and the ClockDevice to the background
  // dispatcher.

  FakeClockImpl clockimpl_fidl;

  zx::result clockimpl_endpoints = fdf::CreateEndpoints<fuchsia_hardware_clockimpl::ClockImpl>();
  ASSERT_TRUE(clockimpl_endpoints.is_ok());
  fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(clockimpl_endpoints->server),
                  &clockimpl_fidl);

  ClockImplProxy proxy({}, std::move(clockimpl_endpoints->client));
  async_patterns::TestDispatcherBound<ClockDevice> dut(background_dispatcher, std::in_place,
                                                       nullptr, std::move(proxy), kTestClockId);

  auto clock_endpoints = fidl::Endpoints<fuchsia_hardware_clock::Clock>::Create();

  dut.SyncCall([&](ClockDevice* device) {
    fidl::BindServer(background_dispatcher, std::move(clock_endpoints.server), device);
  });

  fidl::WireClient<fuchsia_hardware_clock::Clock> clock_client(
      std::move(clock_endpoints.client), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  // Call SetRate on the foreground dispatcher and run until the ClockDevice responds.
  clock_client->SetRate(1'000'000).ThenExactlyOnce(
      [&](fidl::WireUnownedResult<fuchsia_hardware_clock::Clock::SetRate>& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime.Quit();
      });

  runtime.Run();

  ASSERT_TRUE(clockimpl_fidl.clocks()[kTestClockId].rate_hz);
  EXPECT_EQ(*clockimpl_fidl.clocks()[kTestClockId].rate_hz, 1'000'000);
}

}  // namespace
