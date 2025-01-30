// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "clock.h"

#include <fidl/fuchsia.hardware.clock/cpp/fidl.h>
#include <lib/async-default/include/lib/async/default.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/metadata/cpp/metadata_server.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/stdcompat/span.h>

#include <array>
#include <optional>

#include "src/lib/testing/predicates/status.h"

namespace {

class FakeClockImpl : public fdf::WireServer<fuchsia_hardware_clockimpl::ClockImpl> {
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

  cpp20::span<const FakeClock> clocks() const { return {clocks_.data(), clocks_.size()}; }

 private:
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_clockimpl::ClockImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void Enable(fuchsia_hardware_clockimpl::wire::ClockImplEnableRequest* request, fdf::Arena& arena,
              EnableCompleter::Sync& completer) override {
    if (request->id >= clocks_.size()) {
      completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
      return;
    }
    clocks_[request->id].enabled.emplace(true);
    completer.buffer(arena).ReplySuccess();
  }

  void Disable(fuchsia_hardware_clockimpl::wire::ClockImplDisableRequest* request,
               fdf::Arena& arena, DisableCompleter::Sync& completer) override {
    if (request->id >= clocks_.size()) {
      completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
      return;
    }
    clocks_[request->id].enabled.emplace(false);
    completer.buffer(arena).ReplySuccess();
  }

  void IsEnabled(fuchsia_hardware_clockimpl::wire::ClockImplIsEnabledRequest* request,
                 fdf::Arena& arena, IsEnabledCompleter::Sync& completer) override {
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void SetRate(fuchsia_hardware_clockimpl::wire::ClockImplSetRateRequest* request,
               fdf::Arena& arena, SetRateCompleter::Sync& completer) override {
    if (request->id >= clocks_.size()) {
      completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
      return;
    }
    clocks_[request->id].rate_hz.emplace(request->hz);
    completer.buffer(arena).ReplySuccess();
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
    if (request->id >= clocks_.size()) {
      completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
      return;
    }
    clocks_[request->id].input_idx.emplace(request->idx);
    completer.buffer(arena).ReplySuccess();
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
};

class Environment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    auto* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

    {
      zx::result result = to_driver_vfs.AddService<fuchsia_hardware_clockimpl::Service>(
          clock_impl_.GetInstanceHandler());
      if (result.is_error()) {
        return result.take_error();
      }
    }

    {
      zx::result result = clock_init_metadata_server_.Serve(to_driver_vfs, dispatcher);
      if (result.is_error()) {
        return result.take_error();
      }
    }

    device_server_.Init(component::kDefaultInstance, "root");
    zx_status_t status = device_server_.AddMetadata(DEVICE_METADATA_CLOCK_IDS, nullptr, 0);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    return zx::make_result(device_server_.Serve(dispatcher, &to_driver_vfs));

    return zx::ok();
  }

  void Init(const fuchsia_hardware_clockimpl::InitMetadata& clock_init_metadata) {
    ASSERT_OK(clock_init_metadata_server_.SetMetadata(clock_init_metadata));
  }

  FakeClockImpl& clock_impl() { return clock_impl_; }

 private:
  FakeClockImpl clock_impl_;
  compat::DeviceServer device_server_;
  fdf_metadata::MetadataServer<fuchsia_hardware_clockimpl::InitMetadata>
      clock_init_metadata_server_;
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
  void StartDriver(const fuchsia_hardware_clockimpl::InitMetadata& metadata,
                   zx_status_t expected_start_driver_status = ZX_OK) {
    driver_test_.RunInEnvironmentTypeContext(
        [&](Environment& environment) mutable { environment.Init(metadata); });
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
  fuchsia_hardware_clockimpl::InitMetadata metadata{
      {.steps = {{{.id = 3, .call = fuchsia_hardware_clockimpl::InitCall::WithEnable({})}},
                 {{.id = 3, .call = fuchsia_hardware_clockimpl::InitCall::WithInputIdx(100)}},
                 {{.id = 3, .call = fuchsia_hardware_clockimpl::InitCall::WithRateHz(500'000'000)}},
                 {{.id = 1, .call = fuchsia_hardware_clockimpl::InitCall::WithEnable({})}},
                 {{.id = 1, .call = fuchsia_hardware_clockimpl::InitCall::WithInputIdx(99)}},
                 {{.id = 1, .call = fuchsia_hardware_clockimpl::InitCall::WithRateHz(400'000'000)}},
                 {{.id = 1, .call = fuchsia_hardware_clockimpl::InitCall::WithDisable({})}},
                 {{.id = 1, .call = fuchsia_hardware_clockimpl::InitCall::WithInputIdx(101)}},
                 {{.id = 1, .call = fuchsia_hardware_clockimpl::InitCall::WithRateHz(600'000'000)}},
                 {{.id = 2, .call = fuchsia_hardware_clockimpl::InitCall::WithDisable({})}},
                 {{.id = 2, .call = fuchsia_hardware_clockimpl::InitCall::WithInputIdx(1)}},
                 {{.id = 4, .call = fuchsia_hardware_clockimpl::InitCall::WithRateHz(100'000)}}}}};

  StartDriver(metadata);

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
  fuchsia_hardware_clockimpl::InitMetadata metadata{
      {.steps = {
           {{.id = 3, .call = fuchsia_hardware_clockimpl::InitCall::WithEnable({})}},
           {{.id = 3, .call = fuchsia_hardware_clockimpl::InitCall::WithInputIdx(100)}},
           {{.id = 3, .call = fuchsia_hardware_clockimpl::InitCall::WithRateHz(500'000'000)}},
           {{.id = 1, .call = fuchsia_hardware_clockimpl::InitCall::WithEnable({})}},
           // This step should return an error due to the clock index being out of range.
           {{.id = 10, .call = fuchsia_hardware_clockimpl::InitCall::WithInputIdx(99)}},
           {{.id = 1, .call = fuchsia_hardware_clockimpl::InitCall::WithRateHz(400'000'000)}},
           {{.id = 2, .call = fuchsia_hardware_clockimpl::InitCall::WithDisable({})}},
           {{.id = 2, .call = fuchsia_hardware_clockimpl::InitCall::WithInputIdx(1)}},
           {{.id = 4, .call = fuchsia_hardware_clockimpl::InitCall::WithRateHz(100'000)}},
       }}};

  StartDriver(metadata, ZX_ERR_OUT_OF_RANGE);
}

}  // namespace
