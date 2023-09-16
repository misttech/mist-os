// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BUS_LIB_DEVICE_PROTOCOL_PDEV_INCLUDE_LIB_DEVICE_PROTOCOL_PDEV_FIDL_H_
#define SRC_DEVICES_BUS_LIB_DEVICE_PROTOCOL_PDEV_INCLUDE_LIB_DEVICE_PROTOCOL_PDEV_FIDL_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/ddk/device.h>
#include <lib/mmio/mmio-buffer.h>

typedef struct pdev_mmio {
  // Offset from beginning of VMO where the mmio region begins.
  zx_off_t offset;
  // Size of mmio region.
  uint64_t size;
  zx_handle_t vmo;
} pdev_mmio_t;

typedef struct pdev_device_info {
  uint32_t vid;
  uint32_t pid;
  uint32_t did;
  uint32_t mmio_count;
  uint32_t irq_count;
  uint32_t bti_count;
  uint32_t smc_count;
  uint32_t metadata_count;
  uint32_t reserved[8];
  char name[32];
} pdev_device_info_t;

typedef struct pdev_board_info {
  // Vendor ID for the board.
  uint32_t vid;
  // Product ID for the board.
  uint32_t pid;
  // Board name from the boot image platform ID record,
  // (or from the BIOS on x86 platforms).
  char board_name[32];
  // Board specific revision number.
  uint32_t board_revision;
} pdev_board_info_t;

namespace ddk {

// A helper class that wraps the `fuchsia.hardware.platform.device/Device` FIDL calls.
// This class exists to make it simpler for clients to move onto the platform device FIDL
// instead of relying on Banjo proxying. It has the same API as the `PDev` class.
class PDevFidl {
 public:
  static constexpr char kFragmentName[] = "pdev";

  PDevFidl() = default;
  explicit PDevFidl(fidl::ClientEnd<fuchsia_hardware_platform_device::Device> client);

  // TODO(https://fxbug.dev/122534): Remove these.
  // These constructors exist to match the PDev class. They can fail, so `is_valid` must
  // be checked on the object after being created. Please prefer using the `Create` methods.
  explicit PDevFidl(zx_device_t* parent);
  explicit PDevFidl(zx_device_t* parent, const char* fragment_name);

  static zx::result<PDevFidl> Create(zx_device_t* parent);
  static zx::result<PDevFidl> Create(zx_device_t* parent, const char* fragment_name);

  // TODO(https://fxbug.dev/122534): Remove these.
  static PDevFidl FromFragment(zx_device_t* parent);
  static zx_status_t FromFragment(zx_device_t* parent, PDevFidl* out);

  void ShowInfo();

  zx_status_t MapMmio(uint32_t index, std::optional<fdf::MmioBuffer>* mmio,
                      uint32_t cache_policy = ZX_CACHE_POLICY_UNCACHED_DEVICE);
  zx_status_t GetInterrupt(uint32_t index, zx::interrupt* out) {
    return GetInterrupt(index, 0, out);
  }

  // The functions below get their signature from fuchsia.hardware.platform.device Banjo.
  zx_status_t GetMmio(uint32_t index, pdev_mmio_t* out_mmio) const;
  zx_status_t GetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq) const;
  zx_status_t GetBti(uint32_t index, zx::bti* out_bti) const;
  zx_status_t GetSmc(uint32_t index, zx::resource* out_smc) const;
  zx_status_t GetDeviceInfo(pdev_device_info_t* out_info) const;
  zx_status_t GetBoardInfo(pdev_board_info_t* out_info) const;

  bool is_valid() const { return pdev_.is_valid(); }

 private:
  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev_;
};

// This helper is marked weak because it is sometimes necessary to provide a
// test-only version that allows for MMIO fakes or mocks to reach the driver under test.
// For example say you have a fake Protocol device that needs to smuggle a fake MMIO:
//
//  class FakePDev : public ddk::PDevProtocol<FakePDev, ddk::base_protocol> {
//    .....
//    zx_status_t PDevGetMmio(uint32_t index, pdev_mmio_t* out_mmio) {
//      out_mmio->offset = reinterpret_cast<size_t>(this);
//      return ZX_OK;
//    }
//    .....
//  };
//
// The actual implementation expects a real {size, offset, VMO} and therefore it will
// fail. But works if a replacement PDevMakeMmioBufferWeak in the test is provided:
//
//  zx_status_t PDevMakeMmioBufferWeak(
//      const pdev_mmio_t& pdev_mmio, std::optional<MmioBuffer>* mmio) {
//    auto* test_harness = reinterpret_cast<FakePDev*>(pdev_mmio.offset);
//    mmio->emplace(test_harness->fake_mmio());
//    return ZX_OK;
//  }
//
//  Note that if you are using `//src/devices/testing/fake-pdev`, it provides and implementation of
//  this functionality for you and you should instead invoke it's
//  `set_mmio(uint32_t index, fdf::MmioBuffer mmio)` method instead.
//
zx_status_t PDevMakeMmioBufferWeak(const pdev_mmio_t& pdev_mmio,
                                   std::optional<fdf::MmioBuffer>* mmio, uint32_t cache_policy);

}  // namespace ddk

#endif  // SRC_DEVICES_BUS_LIB_DEVICE_PROTOCOL_PDEV_INCLUDE_LIB_DEVICE_PROTOCOL_PDEV_FIDL_H_
