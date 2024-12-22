// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_FAKE_PLATFORM_DEVICE_CPP_FAKE_PDEV_H_
#define LIB_DRIVER_FAKE_PLATFORM_DEVICE_CPP_FAKE_PDEV_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/driver/platform-device/cpp/pdev.h>
#include <lib/mmio/mmio.h>

#include <map>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf_fake {

using Mmio = std::variant<fdf::PDev::MmioInfo, fdf::MmioBuffer>;

class FakePDev final : public fidl::WireServer<fuchsia_hardware_platform_device::Device> {
 public:
  // Allows for `std::string_view`'s to be used when searching an unordered map that uses
  // `std::string` as its key.
  struct StringHash {
    using hash_type = std::hash<std::string_view>;
    using is_transparent = void;

    std::size_t operator()(const char* str) const { return hash_type{}(str); }
    std::size_t operator()(std::string_view str) const { return hash_type{}(str); }
    std::size_t operator()(std::string const& str) const { return hash_type{}(str); }
  };

  using MetadataMap =
      std::unordered_map<std::string, std::vector<uint8_t>, StringHash, std::equal_to<>>;
  using InterruptNamesMap = std::unordered_map<std::string, uint32_t, StringHash, std::equal_to<>>;

  struct Config final {
    // If true, a bti will be generated lazily if it does not exist.
    bool use_fake_bti = false;

    // If true, a smc will be generated lazily if it does not exist.
    bool use_fake_smc = false;

    // If true, an irq will be generated lazily if it does not exist.
    bool use_fake_irq = false;

    std::map<uint32_t, Mmio> mmios;
    std::map<uint32_t, zx::interrupt> irqs;
    InterruptNamesMap irq_names;
    std::map<uint32_t, zx::bti> btis;
    std::map<uint32_t, zx::resource> smcs;

    std::optional<fdf::PDev::DeviceInfo> device_info;
    std::optional<fdf::PDev::BoardInfo> board_info;
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    std::vector<fuchsia_hardware_power::PowerElementConfiguration> power_elements;
#endif
  };

  FakePDev() = default;

  FakePDev(const FakePDev&) = delete;
  FakePDev& operator=(const FakePDev&) = delete;
  FakePDev(FakePDev&&) = delete;
  FakePDev& operator=(FakePDev&&) = delete;

  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler(
      async_dispatcher_t* dispatcher = nullptr) {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(
            this, dispatcher ? dispatcher : async_get_default_dispatcher(),
            fidl::kIgnoreBindingClosure),
    });
  }

  zx_status_t Connect(fidl::ServerEnd<fuchsia_hardware_platform_device::Device> request) {
    binding_group_.AddBinding(async_get_default_dispatcher(), std::move(request), this,
                              fidl::kIgnoreBindingClosure);
    return ZX_OK;
  }

  zx_status_t SetConfig(Config&& config) {
    config_ = std::move(config);
    return ZX_OK;
  }

  void set_metadata(MetadataMap metadata) { metadata_ = std::move(metadata); }

  template <typename FidlType>
  zx_status_t AddFidlMetadata(std::string metadata_id, const FidlType& metadata) {
    static_assert(fidl::IsFidlType<FidlType>::value, "|FidlType| must be a FIDL domain object.");
    static_assert(!fidl::IsResource<FidlType>::value,
                  "|FidlType| cannot be a resource type. Resources cannot be persisted.");
    fit::result encoded_metadata = fidl::Persist(metadata);
    if (encoded_metadata.is_error()) {
      return encoded_metadata.error_value().status();
    }
    metadata_.insert({std::move(metadata_id), std::move(encoded_metadata.value())});
    return ZX_OK;
  }

 private:
  void GetMmioById(GetMmioByIdRequestView request, GetMmioByIdCompleter::Sync& completer) override;
  void GetMmioByName(GetMmioByNameRequestView request,
                     GetMmioByNameCompleter::Sync& completer) override;
  void GetInterruptById(GetInterruptByIdRequestView request,
                        GetInterruptByIdCompleter::Sync& completer) override;
  void GetInterruptByName(GetInterruptByNameRequestView request,
                          GetInterruptByNameCompleter::Sync& completer) override;
  void GetBtiById(GetBtiByIdRequestView request, GetBtiByIdCompleter::Sync& completer) override;
  void GetBtiByName(GetBtiByNameRequestView request,
                    GetBtiByNameCompleter::Sync& completer) override;
  void GetSmcById(GetSmcByIdRequestView request, GetSmcByIdCompleter::Sync& completer) override;
  void GetSmcByName(GetSmcByNameRequestView request,
                    GetSmcByNameCompleter::Sync& completer) override;
  void GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) override;
  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override;
  void GetPowerConfiguration(GetPowerConfigurationCompleter::Sync& completer) override;
  void GetMetadata(GetMetadataRequestView request, GetMetadataCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  zx::result<zx::interrupt> GetInterruptById(uint32_t index);

  Config config_;
  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> binding_group_;
  MetadataMap metadata_;
};

}  // namespace fdf_fake

#endif  // FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

#endif  // LIB_DRIVER_FAKE_PLATFORM_DEVICE_CPP_FAKE_PDEV_H_
