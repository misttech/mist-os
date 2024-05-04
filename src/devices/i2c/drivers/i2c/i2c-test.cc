// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c.h"

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/fdf/env.h>

#include <ddktl/device.h>
#include <zxtest/zxtest.h>

#include "fake-incoming-namespace.h"
#include "lib/ddk/metadata.h"
#include "src/devices/i2c/drivers/i2c/i2c-child.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {
namespace fi2c = fuchsia_hardware_i2c_businfo::wire;

fi2c::I2CChannel MakeChannel(fidl::AnyArena& arena, uint16_t addr) {
  return fi2c::I2CChannel::Builder(arena).address(addr).Build();
}

}  // namespace

class I2cMetadataTest : public zxtest::Test {
 public:
  void TearDown() override {
    auto* parent = fake_root_.get();
    for (auto& device : parent->children()) {
      device_async_remove(device.get());
    }

    mock_ddk::ReleaseFlaggedDevices(parent);
  }

 protected:
  void CreateDevice(fi2c::I2CBusMetadata& metadata) {
    // Setup i2c impl parent
    auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
    incoming_.SyncCall(&i2c::FakeIncomingNamespace::AddI2cImplService, std::move(endpoints.server));
    fake_root_->AddFidlService(fuchsia_hardware_i2cimpl::Service::Name,
                               std::move(endpoints.client));

    // Setup metadata
    fit::result metadata_bytes = fidl::Persist(metadata);
    ASSERT_TRUE(metadata_bytes.is_ok());
    fake_root_->SetMetadata(DEVICE_METADATA_I2C_CHANNELS, metadata_bytes->data(),
                            metadata_bytes->size());

    ASSERT_OK(i2c::I2cDevice::Create(nullptr, fake_root_.get()));
    ASSERT_EQ(fake_root_->child_count(), 1);
    i2c_ = fake_root_->GetLatestChild()->GetDeviceContext<i2c::I2cDevice>();
    ASSERT_NOT_NULL(i2c_);
  }

  i2c::I2cDevice* i2c() { return i2c_; }

 private:
  std::shared_ptr<zx_device> fake_root_ = MockDevice::FakeRootParent();
  i2c::I2cDevice* i2c_;

  fdf::UnownedSynchronizedDispatcher incoming_dispatcher_{
      fdf_testing::DriverRuntime::GetInstance()->StartBackgroundDispatcher()};
  async_patterns::TestDispatcherBound<i2c::FakeIncomingNamespace> incoming_{
      incoming_dispatcher_->async_dispatcher(), std::in_place, 64};
};

TEST_F(I2cMetadataTest, ProvidesMetadataToChildren) {
  std::vector<fi2c::I2CChannel> channels;
  fidl::Arena<> arena;
  channels.emplace_back(MakeChannel(arena, 0xa));
  channels.emplace_back(MakeChannel(arena, 0xb));
  auto channels_view = fidl::VectorView<fi2c::I2CChannel>::FromExternal(channels);
  auto metadata = fi2c::I2CBusMetadata::Builder(arena).channels(channels_view).Build();

  CreateDevice(metadata);

  zx_device_t* i2c_device = i2c()->zxdev();
  // There should be two devices per channel.
  ASSERT_EQ(i2c_device->child_count(), 2);

  for (auto& child : i2c_device->children()) {
    uint16_t expected_addr = 0xff;
    for (const auto& prop : child->GetProperties()) {
      if (prop.id == BIND_I2C_ADDRESS) {
        expected_addr = static_cast<uint16_t>(prop.value);
      }
    }

    auto decoded =
        ddk::GetEncodedMetadata<fi2c::I2CChannel>(child.get(), DEVICE_METADATA_I2C_DEVICE);
    ASSERT_TRUE(decoded.is_ok());
    ASSERT_EQ(decoded->address(), expected_addr);
  }
}
