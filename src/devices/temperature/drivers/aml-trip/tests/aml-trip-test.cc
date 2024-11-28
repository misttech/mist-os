// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../aml-trip.h"

#include <fidl/fuchsia.hardware.clock/cpp/wire_messaging.h>
#include <fidl/fuchsia.hardware.clock/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/common_types.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/wire_types.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/runtime/testing/cpp/sync_helpers.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/cpp/arena.h>
#include <lib/fdf/testing.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/resource.h>
#include <zircon/errors.h>

#include <cstdint>
#include <limits>
#include <memory>
#include <optional>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <src/devices/bus/testing/fake-pdev/fake-pdev.h>
#include <src/devices/temperature/drivers/aml-trip/aml-trip-device.h>
#include <zxtest/zxtest.h>

#include "../aml-trip-device.h"

namespace temperature {

namespace {
static constexpr char kTestName[] = "test-trip-device";

class FakeSensorMmio {
 public:
  FakeSensorMmio() : mmio_(sizeof(uint32_t), kRegCount) {
    mmio_[kTstat0Offset].SetReadCallback([]() {
      // Magic temp code that evaluates to roughly 41.9C with a sensor trim = 0.
      return 0x205c;
    });
    mmio_[kTstat1Offset].SetReadCallback([this]() { return tstat1_; });
  }

  fdf::MmioBuffer mmio() { return mmio_.GetMmioBuffer(); }

  void SetIrqStat(int index, bool stat) {
    if (stat) {
      tstat1_ |= (1u << index);
    } else {
      tstat1_ &= ~(1u << index);
    }
  }

 private:
  static constexpr size_t kTstat0Offset = 0x40;
  static constexpr size_t kTstat1Offset = 0x44;
  static constexpr size_t kRegCount = 0x80 / sizeof(uint32_t);

  uint8_t tstat1_ = 0x0;

  ddk_fake::FakeMmioRegRegion mmio_;
};

class TestEnvironmentWrapper {
 public:
  fdf::DriverStartArgs Setup(fake_pdev::FakePDevFidl::Config pdev_config) {
    zx::result start_args_result = node_.CreateStartArgsAndServe();
    EXPECT_OK(start_args_result.status_value());

    EXPECT_OK(
        env_.Initialize(std::move(start_args_result->incoming_directory_server)).status_value());

    pdev_.SetConfig(std::move(pdev_config));

    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

    auto pdev_result =
        env_.incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
            pdev_.GetInstanceHandler(dispatcher));
    EXPECT_OK(pdev_result.status_value());

    compat_server_.Initialize("default");
    // compat_server_.AddMetadata(DEVICE_METADATA_TRIP, &kTestMetadata, sizeof(kTestMetadata));
    zx_status_t status = compat_server_.Serve(dispatcher, &env_.incoming_directory());
    ZX_ASSERT(status == ZX_OK);

    return std::move(start_args_result->start_args);
  }

  fdf_testing::TestNode& Node() { return node_; }

 private:
  fdf_testing::TestNode node_{"root"};
  fdf_testing::internal::TestEnvironment env_;
  fake_pdev::FakePDevFidl pdev_;
  compat::DeviceServer compat_server_;
};

}  // namespace

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
class AmlTripTest : public zxtest::Test {
 public:
  AmlTripTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        driver_dispatcher_(runtime_.StartBackgroundDispatcher()),
        dut_(driver_dispatcher_->async_dispatcher(), std::in_place),
        test_environment_(env_dispatcher_->async_dispatcher(), std::in_place) {}

  void SetUp() override {
    fake_pdev::FakePDevFidl::Config config{.use_fake_irq = true};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq_));

    ASSERT_OK(irq_.duplicate(ZX_RIGHT_SAME_RIGHTS, &irq_dupe_));
    config.irqs[0] = std::move(irq_dupe_);

    constexpr size_t kTrimRegSize = 0x04;

    config.mmios[kSensorMmioIndex] = sensor_mmio_.mmio();
    config.mmios[kTrimMmioIndex] =
        fdf_testing::CreateMmioBuffer(kTrimRegSize, ZX_CACHE_POLICY_UNCACHED_DEVICE);

    pdev_device_info_t devinfo;
    strcpy(devinfo.name, kTestName);
    config.device_info = devinfo;

    fdf::DriverStartArgs start_args =
        test_environment_.SyncCall(&TestEnvironmentWrapper::Setup, std::move(config));

    zx::result start_result = runtime_.RunToCompletion(dut_.SyncCall(
        &fdf_testing::internal::DriverUnderTest<AmlTrip>::Start, std::move(start_args)));
    ASSERT_OK(start_result.status_value());

    test_environment_.SyncCall([this](TestEnvironmentWrapper* env_wrapper) {
      auto client_channel = env_wrapper->Node().children().at("aml-trip-device").ConnectToDevice();
      client_.Bind(fidl::ClientEnd<fuchsia_hardware_trippoint::TripPoint>(
          std::move(client_channel.value())));
      ASSERT_TRUE(client_.is_valid());
    });
  }

  void TearDown() override {
    zx::result run_result = runtime_.RunToCompletion(
        dut_.SyncCall(&fdf_testing::internal::DriverUnderTest<AmlTrip>::PrepareStop));
    ZX_ASSERT(run_result.is_ok());
  }

 protected:
  zx::interrupt irq_, irq_dupe_;
  FakeSensorMmio sensor_mmio_;
  fidl::WireSyncClient<fuchsia_hardware_trippoint::TripPoint> client_;

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  async_patterns::TestDispatcherBound<fdf_testing::internal::DriverUnderTest<AmlTrip>> dut_;
  async_patterns::TestDispatcherBound<TestEnvironmentWrapper> test_environment_;
};

TEST_F(AmlTripTest, TestReadTemperature) {
  auto result = client_->GetTemperatureCelsius();

  ASSERT_TRUE(result.ok());

  // Expect the measured temperature to be within 0.01C of 41.9 for floating
  // point error.
  EXPECT_LT(abs(result->temp - 41.9), 0.01);
}

TEST_F(AmlTripTest, TestGetSensorName) {
  auto result = client_->GetSensorName();

  ASSERT_TRUE(result.ok());

  const std::string name(result->name.begin(), result->name.end());

  EXPECT_EQ(name, kTestName);
}

TEST_F(AmlTripTest, TestGetTripPointDescriptors) {
  auto result = client_->GetTripPointDescriptors();

  ASSERT_TRUE(result.ok());

  for (const auto& desc : result.value()->descriptors) {
    if (desc.index < 4) {
      EXPECT_EQ(desc.type, fuchsia_hardware_trippoint::TripPointType::kOneshotTempAbove);
    } else {
      EXPECT_EQ(desc.type, fuchsia_hardware_trippoint::TripPointType::kOneshotTempBelow);
    }
    EXPECT_TRUE(desc.configuration.is_cleared_trip_point());
  }
}

TEST_F(AmlTripTest, TestSetTripBadIndex) {
  fuchsia_hardware_trippoint::wire::OneshotTempAboveTripPoint tp;
  tp.critical_temperature_celsius = 30.0;

  fuchsia_hardware_trippoint::wire::TripPointDescriptor desc;
  desc.index = 8;  // This is out of range!
  desc.type = fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempAbove;
  desc.configuration =
      fuchsia_hardware_trippoint::wire::TripPointValue::WithOneshotTempAboveTripPoint(tp);

  std::vector<fuchsia_hardware_trippoint::wire::TripPointDescriptor> descs;
  descs.push_back(desc);

  auto descs_view =
      fidl::VectorView<fuchsia_hardware_trippoint::wire::TripPointDescriptor>::FromExternal(descs);

  auto set_result = client_->SetTripPoints(descs_view);
  EXPECT_TRUE(set_result.ok());
  EXPECT_TRUE(set_result->is_error());
}

TEST_F(AmlTripTest, TestSetTripBadType) {
  auto result = client_->GetTripPointDescriptors();

  ASSERT_TRUE(result.ok());

  auto descs_view = result.value()->descriptors;

  std::vector<fuchsia_hardware_trippoint::wire::TripPointDescriptor> descs;

  auto rise_desc = std::find_if(
      descs_view.begin(), descs_view.end(),
      [](fuchsia_hardware_trippoint::wire::TripPointDescriptor& d) {
        return d.type == fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempAbove;
      });

  // Make sure at least one descriptor is found.
  ASSERT_NE(rise_desc, descs_view.end());

  // The hardware told us that this was a rise trip point but we're going to try to configure
  // it as a fall trip point. This should be an error.
  fuchsia_hardware_trippoint::wire::OneshotTempBelowTripPoint tp;
  tp.critical_temperature_celsius = 30.0;
  rise_desc->type = fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempBelow;
  rise_desc->configuration =
      fuchsia_hardware_trippoint::wire::TripPointValue::WithOneshotTempBelowTripPoint(tp);

  descs.push_back(*rise_desc);
  auto set_desc_view =
      fidl::VectorView<fuchsia_hardware_trippoint::wire::TripPointDescriptor>::FromExternal(descs);

  auto set_result = client_->SetTripPoints(set_desc_view);
  EXPECT_TRUE(set_result.ok());
  EXPECT_TRUE(set_result->is_error());
}

TEST_F(AmlTripTest, TestSetCriticalTempInfinity) {
  auto result = client_->GetTripPointDescriptors();

  ASSERT_TRUE(result.ok());

  auto descs_view = result.value()->descriptors;

  std::vector<fuchsia_hardware_trippoint::wire::TripPointDescriptor> descs;

  auto rise_desc = std::find_if(
      descs_view.begin(), descs_view.end(),
      [](fuchsia_hardware_trippoint::wire::TripPointDescriptor& d) {
        return d.type == fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempAbove;
      });

  // Make sure at least one descriptor is found.
  ASSERT_NE(rise_desc, descs_view.end());

  // It's illegal to configure the trip point temperature as Infinity.
  fuchsia_hardware_trippoint::wire::OneshotTempAboveTripPoint tp;
  tp.critical_temperature_celsius = std::numeric_limits<float>::infinity();
  rise_desc->type = fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempAbove;
  rise_desc->configuration =
      fuchsia_hardware_trippoint::wire::TripPointValue::WithOneshotTempAboveTripPoint(tp);

  descs.push_back(*rise_desc);
  auto set_desc_view =
      fidl::VectorView<fuchsia_hardware_trippoint::wire::TripPointDescriptor>::FromExternal(descs);

  auto set_result = client_->SetTripPoints(set_desc_view);
  EXPECT_TRUE(set_result.ok());
  EXPECT_TRUE(set_result->is_error());
}

TEST_F(AmlTripTest, TestTypeConfigMismatch) {
  auto result = client_->GetTripPointDescriptors();

  ASSERT_TRUE(result.ok());

  auto descs_view = result.value()->descriptors;

  std::vector<fuchsia_hardware_trippoint::wire::TripPointDescriptor> descs;

  auto rise_desc = std::find_if(
      descs_view.begin(), descs_view.end(),
      [](fuchsia_hardware_trippoint::wire::TripPointDescriptor& d) {
        return d.type == fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempAbove;
      });

  // Make sure at least one descriptor is found.
  ASSERT_NE(rise_desc, descs_view.end());

  // The hardware told us that this was a rise trip point but we're going to try to configure
  // it as a fall trip point. Unlike the test above, we're not going to flip the
  // trip point type field.
  fuchsia_hardware_trippoint::wire::OneshotTempBelowTripPoint tp;
  tp.critical_temperature_celsius = 30.0;
  rise_desc->configuration =
      fuchsia_hardware_trippoint::wire::TripPointValue::WithOneshotTempBelowTripPoint(tp);

  descs.push_back(*rise_desc);
  auto set_desc_view =
      fidl::VectorView<fuchsia_hardware_trippoint::wire::TripPointDescriptor>::FromExternal(descs);

  auto set_result = client_->SetTripPoints(set_desc_view);
  EXPECT_TRUE(set_result.ok());
  EXPECT_TRUE(set_result->is_error());
}

TEST_F(AmlTripTest, TestTripPointSuccess) {
  auto result = client_->GetTripPointDescriptors();

  ASSERT_TRUE(result.ok());

  auto descs_view = result.value()->descriptors;

  std::vector<fuchsia_hardware_trippoint::wire::TripPointDescriptor> descs;

  auto rise_desc = std::find_if(
      descs_view.begin(), descs_view.end(),
      [](fuchsia_hardware_trippoint::wire::TripPointDescriptor& d) {
        return d.type == fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempAbove;
      });

  // Make sure at least one descriptor is found.
  ASSERT_NE(rise_desc, descs_view.end());

  fuchsia_hardware_trippoint::wire::OneshotTempAboveTripPoint tp;
  tp.critical_temperature_celsius = 30.0;
  rise_desc->configuration =
      fuchsia_hardware_trippoint::wire::TripPointValue::WithOneshotTempAboveTripPoint(tp);

  descs.push_back(*rise_desc);
  auto set_desc_view =
      fidl::VectorView<fuchsia_hardware_trippoint::wire::TripPointDescriptor>::FromExternal(descs);

  auto set_result = client_->SetTripPoints(set_desc_view);
  EXPECT_TRUE(set_result.ok());
  EXPECT_TRUE(set_result->is_ok());

  auto get_result = client_->GetTripPointDescriptors();
  ASSERT_TRUE(get_result.ok());

  // Trigger an interrupt and set the registers to make it look like an
  // interrupt is pending.
  sensor_mmio_.SetIrqStat(rise_desc->index, true);

  irq_.trigger(0, zx::clock::get_boot());

  auto wait_result = client_->WaitForAnyTripPoint();

  EXPECT_TRUE(wait_result.ok());
  EXPECT_TRUE(wait_result->is_ok());
}

TEST_F(AmlTripTest, TestTwoTripPointSuccess) {
  auto result = client_->GetTripPointDescriptors();

  ASSERT_TRUE(result.ok());

  auto descs_view = result.value()->descriptors;

  std::vector<fuchsia_hardware_trippoint::wire::TripPointDescriptor> descs;

  auto rise_desc = std::find_if(
      descs_view.begin(), descs_view.end(),
      [](fuchsia_hardware_trippoint::wire::TripPointDescriptor& d) {
        return d.type == fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempAbove;
      });

  auto fall_desc = std::find_if(
      descs_view.begin(), descs_view.end(),
      [](fuchsia_hardware_trippoint::wire::TripPointDescriptor& d) {
        return d.type == fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempBelow;
      });

  // Make sure at least one descriptor is found.
  ASSERT_NE(rise_desc, descs_view.end());
  ASSERT_NE(fall_desc, descs_view.end());

  fuchsia_hardware_trippoint::wire::OneshotTempAboveTripPoint rise_tp;
  rise_tp.critical_temperature_celsius = 30.0;
  rise_desc->configuration =
      fuchsia_hardware_trippoint::wire::TripPointValue::WithOneshotTempAboveTripPoint(rise_tp);

  fuchsia_hardware_trippoint::wire::OneshotTempBelowTripPoint fall_tp;
  fall_tp.critical_temperature_celsius = 50.0;
  fall_desc->configuration =
      fuchsia_hardware_trippoint::wire::TripPointValue::WithOneshotTempBelowTripPoint(fall_tp);

  descs.push_back(*rise_desc);
  descs.push_back(*fall_desc);

  auto set_desc_view =
      fidl::VectorView<fuchsia_hardware_trippoint::wire::TripPointDescriptor>::FromExternal(descs);

  auto set_result = client_->SetTripPoints(set_desc_view);
  EXPECT_TRUE(set_result.ok());
  EXPECT_TRUE(set_result->is_ok());

  // Trigger an interrupt and set the registers to make it look like an
  // interrupt is pending.
  sensor_mmio_.SetIrqStat(rise_desc->index, true);
  sensor_mmio_.SetIrqStat(fall_desc->index, true);

  irq_.trigger(0, zx::clock::get_boot());

  auto first_result = client_->WaitForAnyTripPoint();
  EXPECT_TRUE(first_result.ok());
  EXPECT_TRUE(first_result->is_ok());

  auto second_result = client_->WaitForAnyTripPoint();
  EXPECT_TRUE(second_result.ok());
  EXPECT_TRUE(second_result->is_ok());
}

TEST_F(AmlTripTest, TestWaitNoConfigured) {
  auto wait_result = client_->WaitForAnyTripPoint();

  // Waiting without configuring any trips is an error.
  EXPECT_TRUE(wait_result.ok());
  EXPECT_FALSE(wait_result->is_ok());
}

TEST_F(AmlTripTest, TestClearTripPointAfterFire) {
  auto result = client_->GetTripPointDescriptors();

  ASSERT_TRUE(result.ok());

  auto descs_view = result.value()->descriptors;

  std::vector<fuchsia_hardware_trippoint::wire::TripPointDescriptor> descs;

  auto rise_desc = std::find_if(
      descs_view.begin(), descs_view.end(),
      [](fuchsia_hardware_trippoint::wire::TripPointDescriptor& d) {
        return d.type == fuchsia_hardware_trippoint::wire::TripPointType::kOneshotTempAbove;
      });

  // Make sure at least one descriptor is found.
  ASSERT_NE(rise_desc, descs_view.end());

  fuchsia_hardware_trippoint::wire::OneshotTempAboveTripPoint tp;
  tp.critical_temperature_celsius = 30.0;
  rise_desc->configuration =
      fuchsia_hardware_trippoint::wire::TripPointValue::WithOneshotTempAboveTripPoint(tp);

  descs.push_back(*rise_desc);
  auto set_desc_view =
      fidl::VectorView<fuchsia_hardware_trippoint::wire::TripPointDescriptor>::FromExternal(descs);

  auto set_result = client_->SetTripPoints(set_desc_view);
  EXPECT_TRUE(set_result.ok());
  EXPECT_TRUE(set_result->is_ok());

  // Trigger an interrupt and set the registers to make it look like an
  // interrupt is pending.
  sensor_mmio_.SetIrqStat(rise_desc->index, true);

  irq_.trigger(0, zx::clock::get_boot());

  // Clear the trip point that just fired.
  fuchsia_hardware_trippoint::wire::ClearedTripPoint cleared;
  descs.at(0).configuration =
      fuchsia_hardware_trippoint::wire::TripPointValue::WithClearedTripPoint(cleared);
  auto set_clear_result = client_->SetTripPoints(set_desc_view);
  EXPECT_TRUE(set_clear_result.ok());
  EXPECT_TRUE(set_clear_result->is_ok());

  auto wait_result = client_->WaitForAnyTripPoint();

  EXPECT_TRUE(wait_result.ok());
  EXPECT_FALSE(wait_result->is_ok());  // No trip points should be configured.
}

}  // namespace temperature
