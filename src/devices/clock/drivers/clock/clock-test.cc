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

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

class FakeClockImpl : public ddk::ClockImplProtocol<FakeClockImpl> {
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
  std::array<FakeClock, 6> clocks_;
};

TEST(ClockTest, ConfigureClocks) {
  fidl::Arena arena;

  FakeClockImpl clock_impl;

  std::shared_ptr fake_parent = MockDevice::FakeRootParent();

  fake_parent->AddProtocol(ZX_PROTOCOL_CLOCK_IMPL, clock_impl.ops(), &clock_impl);
  fake_parent->SetMetadata(DEVICE_METADATA_CLOCK_IDS, nullptr, 0);

  fuchsia_hardware_clock::wire::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fuchsia_hardware_clock::wire::InitStep>(arena, 12);

  metadata.steps[0] = {3, fuchsia_hardware_clock::wire::InitCall::WithEnable({})};
  metadata.steps[1] = {3, fuchsia_hardware_clock::wire::InitCall::WithInputIdx(100)};
  metadata.steps[2] = {3, fuchsia_hardware_clock::wire::InitCall::WithRateHz(arena, 500'000'000)};

  metadata.steps[3] = {1, fuchsia_hardware_clock::wire::InitCall::WithEnable({})};
  metadata.steps[4] = {1, fuchsia_hardware_clock::wire::InitCall::WithInputIdx(99)};
  metadata.steps[5] = {1, fuchsia_hardware_clock::wire::InitCall::WithRateHz(arena, 400'000'000)};

  metadata.steps[6] = {1, fuchsia_hardware_clock::wire::InitCall::WithDisable({})};
  metadata.steps[7] = {1, fuchsia_hardware_clock::wire::InitCall::WithInputIdx(101)};
  metadata.steps[8] = {1, fuchsia_hardware_clock::wire::InitCall::WithRateHz(arena, 600'000'000)};

  metadata.steps[9] = {2, fuchsia_hardware_clock::wire::InitCall::WithDisable({})};
  metadata.steps[10] = {2, fuchsia_hardware_clock::wire::InitCall::WithInputIdx(1)};

  metadata.steps[11] = {4, fuchsia_hardware_clock::wire::InitCall::WithRateHz(arena, 100'000)};

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

  fuchsia_hardware_clock::wire::InitMetadata metadata;
  metadata.steps = fidl::VectorView<fuchsia_hardware_clock::wire::InitStep>(arena, 9);

  metadata.steps[0] = {3, fuchsia_hardware_clock::wire::InitCall::WithEnable({})};
  metadata.steps[1] = {3, fuchsia_hardware_clock::wire::InitCall::WithInputIdx(100)};
  metadata.steps[2] = {3, fuchsia_hardware_clock::wire::InitCall::WithRateHz(arena, 500'000'000)};

  metadata.steps[3] = {1, fuchsia_hardware_clock::wire::InitCall::WithEnable({})};

  // This step should return an error due to the clock index being out of range.
  metadata.steps[4] = {10, fuchsia_hardware_clock::wire::InitCall::WithInputIdx(99)};

  metadata.steps[5] = {1, fuchsia_hardware_clock::wire::InitCall::WithRateHz(arena, 400'000'000)};

  metadata.steps[6] = {2, fuchsia_hardware_clock::wire::InitCall::WithDisable({})};
  metadata.steps[7] = {2, fuchsia_hardware_clock::wire::InitCall::WithInputIdx(1)};

  metadata.steps[8] = {4, fuchsia_hardware_clock::wire::InitCall::WithRateHz(arena, 100'000)};

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

}  // namespace
