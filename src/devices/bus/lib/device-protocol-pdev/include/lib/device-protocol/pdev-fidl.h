// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BUS_LIB_DEVICE_PROTOCOL_PDEV_INCLUDE_LIB_DEVICE_PROTOCOL_PDEV_FIDL_H_
#define SRC_DEVICES_BUS_LIB_DEVICE_PROTOCOL_PDEV_INCLUDE_LIB_DEVICE_PROTOCOL_PDEV_FIDL_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/device-protocol/pdev.h>
#include <lib/fidl/cpp/natural_types.h>

namespace ddk {

// A helper class that wraps the `fuchsia.hardware.platform.device/Device` FIDL calls.
// This class exists to make it simpler for clients to move onto the platform device FIDL
// instead of relying on Banjo proxying. It has the same API as the `PDev` class.
class PDevFidl {
 public:
  static constexpr char kFragmentName[] = "pdev";

  PDevFidl() = default;
  explicit PDevFidl(fidl::ClientEnd<fuchsia_hardware_platform_device::Device> client);

  // TODO(https://fxbug.dev/42073495): Remove these.
  // These constructors exist to match the PDev class. They can fail, so `is_valid` must
  // be checked on the object after being created. Please prefer using the `Create` methods.
  explicit PDevFidl(zx_device_t* parent);
  explicit PDevFidl(zx_device_t* parent, const char* fragment_name);

  static zx::result<PDevFidl> Create(zx_device_t* parent);
  static zx::result<PDevFidl> Create(zx_device_t* parent, const char* fragment_name);

  // TODO(https://fxbug.dev/42073495): Remove these.
  static PDevFidl FromFragment(zx_device_t* parent);
  static zx_status_t FromFragment(zx_device_t* parent, PDevFidl* out);

  void ShowInfo();

  zx_status_t MapMmio(uint32_t index, std::optional<fdf::MmioBuffer>* mmio,
                      uint32_t cache_policy = ZX_CACHE_POLICY_UNCACHED_DEVICE);
  zx_status_t GetInterrupt(uint32_t index, zx::interrupt* out) {
    return GetInterrupt(index, 0, out);
  }

  template <typename FidlType>
  zx::result<FidlType> GetFidlMetadata(std::string_view metadata_id) const {
    static_assert(fidl::IsFidlType<FidlType>::value, "|FidlType| must be a FIDL domain object.");
    static_assert(!fidl::IsResource<FidlType>::value,
                  "|FidlType| cannot be a resource type. Resources cannot be persisted.");

    fidl::WireResult encoded_metadata =
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

  // The functions below get their signature from fuchsia.hardware.platform.device Banjo.
  zx_status_t GetMmio(uint32_t index, pdev_mmio_t* out_mmio) const;
  zx_status_t GetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq);
  zx_status_t GetBti(uint32_t index, zx::bti* out_bti);
  zx_status_t GetSmc(uint32_t index, zx::resource* out_smc) const;
  zx_status_t GetDeviceInfo(pdev_device_info_t* out_info);
  zx_status_t GetBoardInfo(pdev_board_info_t* out_info) const;

  bool is_valid() const { return pdev_.is_valid(); }

 private:
  fidl::WireSyncClient<fuchsia_hardware_platform_device::Device> pdev_;
};

}  // namespace ddk

#endif  // SRC_DEVICES_BUS_LIB_DEVICE_PROTOCOL_PDEV_INCLUDE_LIB_DEVICE_PROTOCOL_PDEV_FIDL_H_
