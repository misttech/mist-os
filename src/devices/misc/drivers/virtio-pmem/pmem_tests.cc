// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/fake-bti/bti.h>
#include <lib/fake-resource/resource.h>
#include <lib/virtio/backends/fake.h>

#include <gtest/gtest.h>

#include "src/devices/misc/drivers/virtio-pmem/pmem.h"
#include "src/devices/misc/drivers/virtio-pmem/virtio/pmem.h"
#include "src/lib/testing/predicates/status.h"

namespace {

// Fake virtio 'backend' for a virtio-pmem device.
class FakeBackendForPmem final : public virtio::FakeBackend {
 public:
  static constexpr uint64_t kFakeStart = 0xaf000000;
  static constexpr uint64_t kFakeSize = 0x20000000;

  explicit FakeBackendForPmem(zx::unowned_bti fake_bti)
      : virtio::FakeBackend({{0, 1}}), fake_bti_(std::move(fake_bti)) {}

  ~FakeBackendForPmem() final = default;

  uint64_t ReadFeatures() final {
    uint64_t features = FakeBackend::ReadFeatures();

    features |= VIRTIO_F_VERSION_1;
    features |= VIRTIO_PMEM_F_SHMEM_REGION;

    return features;
  }

  void SetFeatures(uint64_t bitmap) final {
    FakeBackend::SetFeatures(bitmap);

    // Don't declare support for VIRTIO_PMEM_F_SHMEM_REGION.
    ZX_ASSERT((bitmap & VIRTIO_PMEM_F_SHMEM_REGION) == 0);
  }

  // FakeBackend does not implement 64 bit config reads.
  void ReadDeviceConfig(uint16_t offset, uint64_t* value) final {
    if (offset == offsetof(virtio_pmem_config, start)) {
      *value = kFakeStart;
    } else if (offset == offsetof(virtio_pmem_config, size)) {
      *value = kFakeSize;
    } else {
      ZX_ASSERT_MSG(false, "Invalid offset");
    }
  }

 private:
  zx::unowned_bti fake_bti_;
};

TEST(PmemDevice, Init) {
  fdf_testing::ScopedGlobalLogger logger;

  zx::bti fake_bti;
  ASSERT_OK(fake_bti_create(fake_bti.reset_and_get_address()));
  zx::resource fake_root_resource;
  ASSERT_OK(fake_root_resource_create(fake_root_resource.reset_and_get_address()));
  zx::resource fake_mmio_resource;
  ASSERT_OK(zx::resource::create(fake_root_resource, 0 /* options */,
                                 FakeBackendForPmem::kFakeStart, FakeBackendForPmem::kFakeSize, "",
                                 0, &fake_mmio_resource));

  auto backend = std::make_unique<FakeBackendForPmem>(fake_bti.borrow());
  auto* backend_ptr = backend.get();
  virtio::PmemDevice device(std::move(fake_bti), std::move(backend), std::move(fake_mmio_resource));

  ASSERT_OK(device.Init());
  EXPECT_EQ(backend_ptr->DeviceState(), virtio::FakeBackend::State::DRIVER_OK);
}

}  // namespace
