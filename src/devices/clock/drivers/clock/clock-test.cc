// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "clock.h"

#include <fidl/fuchsia.hardware.clock/cpp/fidl.h>
#include <fuchsia/hardware/clockimpl/cpp/banjo.h>
#include <lib/async-default/include/lib/async/default.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/stdcompat/span.h>

#include <array>
#include <optional>

#include "src/lib/testing/predicates/status.h"

namespace {

class FakeClockImpl : public ddk::ClockImplProtocol<FakeClockImpl>,
                      public fdf::WireServer<fuchsia_hardware_clockimpl::ClockImpl> {
 public:
  struct FakeClock {
    std::optional<bool> enabled;
    std::optional<uint64_t> rate_hz;
    std::optional<uint32_t> input_idx;
  };

  fuchsia_hardware_clockimpl::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_clockimpl::Service::InstanceHandler(
        {.device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                           fidl::kIgnoreBindingClosure)});
  }

  compat::DeviceServer::BanjoConfig GetBanjoConfig() {
    compat::DeviceServer::BanjoConfig config{.default_proto_id = ZX_PROTOCOL_CLOCK_IMPL};
    config.callbacks[ZX_PROTOCOL_CLOCK_IMPL] = banjo_server_.callback();
    return config;
  }

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
    if (zx_status_t status = ClockImplDisable(request->id); status == ZX_OK) {
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
    if (zx_status_t status = ClockImplSetInput(request->id, request->idx); status == ZX_OK) {
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

  fdf::ServerBindingGroup<fuchsia_hardware_clockimpl::ClockImpl> bindings_;
  compat::BanjoServer banjo_server_{ZX_PROTOCOL_CLOCK_IMPL, this, &clock_impl_protocol_ops_};
};

class Environment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    if (serve_clock_impl_fidl_) {
      zx::result result = to_driver_vfs.AddService<fuchsia_hardware_clockimpl::Service>(
          clock_impl_.GetInstanceHandler());
      if (result.is_error()) {
        return result.take_error();
      }
    }

    compat::DeviceServer::BanjoConfig banjo_config;
    if (serve_clock_impl_banjo_) {
      banjo_config = clock_impl_.GetBanjoConfig();
    }
    device_server_.Init(component::kDefaultInstance, "root", {}, std::move(banjo_config));
    zx_status_t status =
        device_server_.AddMetadata(DEVICE_METADATA_CLOCK_INIT, encoded_clock_init_metadata_.data(),
                                   encoded_clock_init_metadata_.size());
    if (status != ZX_OK) {
      return zx::error(status);
    }

    status = device_server_.AddMetadata(DEVICE_METADATA_CLOCK_IDS, nullptr, 0);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    return zx::make_result(
        device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs));

    return zx::ok();
  }

  void Init(const fuchsia_hardware_clockimpl::wire::InitMetadata& clock_init_metadata,
            bool serve_clock_impl_fidl, bool serve_clock_impl_banjo) {
    serve_clock_impl_fidl_ = serve_clock_impl_fidl;
    serve_clock_impl_banjo_ = serve_clock_impl_banjo;
    fit::result encoded = fidl::Persist(clock_init_metadata);
    ASSERT_TRUE(encoded.is_ok());
    encoded_clock_init_metadata_ = std::move(encoded.value());
  }

  FakeClockImpl& clock_impl() { return clock_impl_; }

 private:
  FakeClockImpl clock_impl_;
  bool serve_clock_impl_fidl_;
  bool serve_clock_impl_banjo_;
  compat::DeviceServer device_server_;
  std::vector<uint8_t> encoded_clock_init_metadata_;
};

class ClockTestConfig {
 public:
  using DriverType = ClockDriver;
  using EnvironmentType = Environment;
};

class ClockTest : public ::testing::Test {
 public:
  void TearDown() override { ASSERT_OK(driver_test_.StopDriver()); }

 protected:
  void StartDriver(const fuchsia_hardware_clockimpl::wire::InitMetadata& metadata,
                   bool serve_clock_impl_fidl, bool serve_clock_impl_banjo,
                   zx_status_t expected_start_driver_status = ZX_OK) {
    driver_test_.RunInEnvironmentTypeContext([&](Environment& environment) mutable {
      environment.Init(metadata, serve_clock_impl_fidl, serve_clock_impl_banjo);
    });
    ASSERT_EQ(driver_test_.StartDriver().status_value(), expected_start_driver_status);
  }

  std::vector<FakeClockImpl::FakeClock> GetClocks() {
    std::vector<FakeClockImpl::FakeClock> clocks;
    driver_test_.RunInEnvironmentTypeContext([&dst = clocks](Environment& environment) mutable {
      auto src = environment.clock_impl().clocks();
      dst = std::vector<FakeClockImpl::FakeClock>(src.begin(), src.end());
    });
    return clocks;
  }

 private:
  fdf_testing::BackgroundDriverTest<ClockTestConfig> driver_test_;
};

TEST_F(ClockTest, ConfigureClocks) {
  fidl::Arena arena;
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

  StartDriver(metadata, true, false);

  auto clocks = GetClocks();
  ASSERT_TRUE(clocks[3].enabled.has_value());
  EXPECT_TRUE(clocks[3].enabled.value());

  ASSERT_TRUE(clocks[3].input_idx.has_value());
  EXPECT_EQ(clocks[3].input_idx.value(), 100u);

  ASSERT_TRUE(clocks[3].rate_hz.has_value());
  EXPECT_EQ(clocks[3].rate_hz.value(), 500'000'000u);

  ASSERT_TRUE(clocks[1].enabled.has_value());
  EXPECT_FALSE(clocks[1].enabled.value());

  ASSERT_TRUE(clocks[1].input_idx.has_value());
  EXPECT_EQ(clocks[1].input_idx.value(), 101u);

  ASSERT_TRUE(clocks[1].rate_hz.has_value());
  EXPECT_EQ(clocks[1].rate_hz.value(), 600'000'000u);

  ASSERT_TRUE(clocks[2].enabled.has_value());
  EXPECT_FALSE(clocks[2].enabled.value());

  ASSERT_TRUE(clocks[2].input_idx.has_value());
  EXPECT_EQ(clocks[2].input_idx.value(), 1u);

  EXPECT_FALSE(clocks[2].rate_hz.has_value());

  ASSERT_TRUE(clocks[4].rate_hz.has_value());
  EXPECT_EQ(clocks[4].rate_hz.value(), 100'000u);

  EXPECT_FALSE(clocks[4].enabled.has_value());
  EXPECT_FALSE(clocks[4].input_idx.has_value());

  EXPECT_FALSE(clocks[0].enabled.has_value());
  EXPECT_FALSE(clocks[0].rate_hz.has_value());
  EXPECT_FALSE(clocks[0].input_idx.has_value());

  EXPECT_FALSE(clocks[5].enabled.has_value());
  EXPECT_FALSE(clocks[5].rate_hz.has_value());
  EXPECT_FALSE(clocks[5].input_idx.has_value());
}

TEST_F(ClockTest, ConfigureClocksError) {
  fidl::Arena arena;
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

  StartDriver(metadata, true, false, ZX_ERR_OUT_OF_RANGE);
}

// Verify that the clock driver can interact with the clock-impl protocol via banjo.
TEST_F(ClockTest, CanUseBanjo) {
  fidl::Arena arena;
  fuchsia_hardware_clockimpl::wire::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fuchsia_hardware_clockimpl::wire::InitStep>(arena, 1);

  // Perform some arbitrary step that requires the clock driver to interact with the clock-impl
  // protocol.
  metadata.steps[0] = fuchsia_hardware_clockimpl::wire::InitStep::Builder(arena)
                          .id(3)
                          .call(fuchsia_hardware_clockimpl::wire::InitCall::WithEnable({}))
                          .Build();

  StartDriver(metadata, false, true);

  auto clocks = GetClocks();
  ASSERT_TRUE(clocks[3].enabled.has_value());
  EXPECT_TRUE(clocks[3].enabled.value());
}

}  // namespace
