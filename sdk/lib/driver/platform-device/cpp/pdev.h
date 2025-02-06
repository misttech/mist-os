// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_PLATFORM_DEVICE_CPP_PDEV_H_
#define LIB_DRIVER_PLATFORM_DEVICE_CPP_PDEV_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/driver/power/cpp/types.h>
#include <lib/fidl/cpp/natural_types.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/bti.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <string>
#include <type_traits>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf {

// A helper class that wraps the `fuchsia.hardware.platform.device/Device` FIDL calls.
// This class exists to make it simpler for clients to move onto the platform device FIDL.
class PDev {
 public:
  static constexpr char kFragmentName[] = "pdev";

  PDev() = default;
  explicit PDev(fidl::ClientEnd<fuchsia_hardware_platform_device::Device> client);

  zx::result<fdf::MmioBuffer> MapMmio(
      uint32_t index, uint32_t cache_policy = ZX_CACHE_POLICY_UNCACHED_DEVICE) const;
  zx::result<fdf::MmioBuffer> MapMmio(
      cpp17::string_view name, uint32_t cache_policy = ZX_CACHE_POLICY_UNCACHED_DEVICE) const;

  template <typename FidlType>
  zx::result<FidlType> GetFidlMetadata(std::string_view metadata_id) const {
    static_assert(fidl::IsFidlType<FidlType>::value, "|FidlType| must be a FIDL domain object.");
    static_assert(!fidl::IsResource<FidlType>::value,
                  "|FidlType| cannot be a resource type. Resources cannot be persisted.");

    fidl::WireResult<fuchsia_hardware_platform_device::Device::GetMetadata> encoded_metadata =
        pdev_->GetMetadata(fidl::StringView::FromExternal(metadata_id));
    if (!encoded_metadata.ok()) {
      return zx::error(encoded_metadata.status());
    }
    if (encoded_metadata->is_error()) {
      return encoded_metadata->take_error();
    }

    auto result = fidl::Unpersist<FidlType>(encoded_metadata.value()->metadata.get());
    if (result.is_error()) {
      return zx::error(result.error_value().status());
    }

    return zx::ok(result.value());
  }

  struct MmioInfo {
    zx_off_t offset;
    size_t size;
    zx::vmo vmo;
  };
  zx::result<MmioInfo> GetMmio(uint32_t index) const;
  zx::result<MmioInfo> GetMmio(cpp17::string_view name) const;

  zx::result<zx::interrupt> GetInterrupt(uint32_t index, uint32_t flags = 0) const;
  zx::result<zx::interrupt> GetInterrupt(cpp17::string_view name, uint32_t flags = 0) const;
  zx::result<zx::bti> GetBti(uint32_t index) const;
  zx::result<zx::resource> GetSmc(uint32_t index) const;

  struct DeviceInfo {
    uint32_t vid;
    uint32_t pid;
    uint32_t did;
    uint32_t mmio_count;
    uint32_t irq_count;
    uint32_t bti_count;
    uint32_t smc_count;
    uint32_t metadata_count;
    std::string name;
  };
  zx::result<DeviceInfo> GetDeviceInfo() const;

  struct BoardInfo {
    uint32_t vid;
    uint32_t pid;
    std::string board_name;
    uint32_t board_revision;
  };
  zx::result<BoardInfo> GetBoardInfo() const;

  zx::result<std::vector<fdf_power::PowerElementConfiguration>> GetPowerConfiguration();

  /// Uses the provided namespace and platform device instance to get a power
  /// configuration, add corresponding power elements to the power topology, and
  /// return `fdf_power::ElementDesc` objects equivalent to the power configuration.
  ///
  /// This function retrieves the config via |dev| and then calls
  /// `fdf_power::ApplyPowerConfiguration`, see its documentation for additional
  /// information.
  fit::result<fdf_power::Error, std::vector<fdf_power::ElementDesc>> GetAndApplyPowerConfiguration(
      const fdf::Namespace& ns);

  bool is_valid() const { return pdev_.is_valid(); }

 private:
  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev_;
};

namespace internal {

// This helper is marked weak because it is sometimes necessary to provide a
// test-only version that allows for MMIO fakes or mocks to reach the driver under test.
// For example say you have a fake Protocol device that needs to smuggle a fake MMIO:
//
//  class FakePDev {
//    .....
//    zx::result<MmioInfo> PDevGetMmio(uint32_t index) {
//      MmioInfo out_mmio = {};
//      out_mmio.offset = reinterpret_cast<size_t>(this);
//      return zx::ok(out_mmio);
//    }
//    .....
//  };
//
// The actual implementation expects a real {size, offset, VMO} and therefore it will
// fail. But works if a replacement PDevMakeMmioBufferWeak in the test is provided:
//
//  zx::result<fdf::MmioBuffer> PDevMakeMmioBufferWeak(const MmioInfo& pdev_mmio) {
//    auto* test_harness = reinterpret_cast<FakePDev*>(pdev_mmio.offset);
//    return zx::ok(test_harness->fake_mmio());
//  }
//
//  Note that if you are using `//src/devices/testing/fake-pdev`, it provides and implementation of
//  this functionality for you and you should instead invoke it's
//  `set_mmio(uint32_t index, fdf::MmioBuffer mmio)` method instead.
//
zx::result<fdf::MmioBuffer> PDevMakeMmioBufferWeak(PDev::MmioInfo& pdev_mmio,
                                                   uint32_t cache_policy);
}  // namespace internal
}  // namespace fdf

#endif  // FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

#endif  // LIB_DRIVER_PLATFORM_DEVICE_CPP_PDEV_H_
