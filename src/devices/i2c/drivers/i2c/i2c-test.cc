// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c.h"

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/async/cpp/task.h>
#include <lib/fdf/env.h>

#include <ddktl/device.h>
#include <zxtest/zxtest.h>

#include "lib/ddk/metadata.h"
#include "sdk/lib/driver/runtime/testing/cpp/dispatcher.h"
#include "src/devices/i2c/drivers/i2c/i2c-child.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {
namespace fi2c = fuchsia_hardware_i2c_businfo::wire;

class FakeI2cImpl;
using DeviceType = ddk::Device<FakeI2cImpl>;

class FakeI2cImpl : public DeviceType,
                    public ddk::I2cImplProtocol<FakeI2cImpl, ddk::base_protocol> {
 public:
  explicit FakeI2cImpl(zx_device_t* parent, std::vector<uint8_t> metadata)
      : DeviceType(parent), metadata_(std::move(metadata)) {
    ASSERT_OK(DdkAdd("fake-i2c-impl"));
    zxdev()->SetMetadata(DEVICE_METADATA_I2C_CHANNELS, metadata_.data(), metadata_.size());
  }

  static FakeI2cImpl* Create(zx_device_t* parent, std::vector<fi2c::I2CChannel> channels) {
    auto channels_view = fidl::VectorView<fi2c::I2CChannel>::FromExternal(channels);

    fidl::Arena<> arena;
    auto metadata = fi2c::I2CBusMetadata::Builder(arena).channels(channels_view).Build();

    auto bytes = fidl::Persist(metadata);
    ZX_ASSERT(bytes.is_ok());
    auto impl = new FakeI2cImpl(parent, std::move(bytes.value()));
    return impl;
  }
  zx_status_t I2cImplGetMaxTransferSize(uint64_t* out_size) {
    *out_size = 64;
    return ZX_OK;
  }
  zx_status_t I2cImplSetBitrate(uint32_t bitrate) { return ZX_OK; }

  zx_status_t I2cImplTransact(const i2c_impl_op_t* op_list, size_t op_count) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  void DdkRelease() { delete this; }

 private:
  void AddMetadata() {}
  std::vector<uint8_t> metadata_;
};

fi2c::I2CChannel MakeChannel(fidl::AnyArena& arena, uint16_t addr) {
  return fi2c::I2CChannel::Builder(arena).address(addr).Build();
}

}  // namespace

class I2cMetadataTest : public zxtest::Test {
 public:
  void SetUp() override {
    // TODO(fxb/124464): Migrate test to use dispatcher integration.
    fake_root_ = MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED();
  }

  void TearDown() override {
    auto result = fdf::RunOnDispatcherSync(dispatcher_.dispatcher(), [parent = fake_root_.get()]() {
      for (auto& device : parent->children()) {
        device_async_remove(device.get());
      }

      mock_ddk::ReleaseFlaggedDevices(parent);
    });
    EXPECT_TRUE(result.is_ok());
  }

 protected:
  fdf::TestSynchronizedDispatcher dispatcher_{fdf::kDispatcherManaged};
  std::shared_ptr<zx_device> fake_root_;
};

TEST_F(I2cMetadataTest, ProvidesMetadataToChildren) {
  std::vector<fi2c::I2CChannel> channels;
  fidl::Arena<> arena;
  channels.emplace_back(MakeChannel(arena, 0xa));
  channels.emplace_back(MakeChannel(arena, 0xb));

  auto impl = FakeI2cImpl::Create(fake_root_.get(), std::move(channels));

  {
    auto result = fdf::RunOnDispatcherSync(dispatcher_.dispatcher(), [parent = impl->zxdev()]() {
      // Make the fake I2C driver.
      ASSERT_OK(i2c::I2cDevice::Create(nullptr, parent));
    });
    EXPECT_TRUE(result.is_ok());
  }

  // Check the number of devices we have makes sense.
  ASSERT_EQ(impl->zxdev()->child_count(), 1);

  // I2cDevice::Create has posted a task to create children on its dispatcher. Verify device
  // properties on that dispatcher to ensure we are running after the children have been added.
  {
    auto result = fdf::RunOnDispatcherSync(dispatcher_.dispatcher(), [impl]() {
      zx_device_t* i2c = impl->zxdev()->GetLatestChild();
      // There should be two devices per channel.
      ASSERT_EQ(i2c->child_count(), 2);

      for (auto& child : i2c->children()) {
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
    });
    EXPECT_TRUE(result.is_ok());
  }
}

int main(int argc, char** argv) {
  // This must be called exactly once before any test cases run.
  fdf_env_start();
  // TODO(https://fxbug.dev/122526): Remove this once the elf runner no longer
  // fools libc into block-buffering stdout.
  setlinebuf(stdout);
  return RUN_ALL_TESTS(argc, argv);
}
