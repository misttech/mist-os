// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/fake-bti/cpp/fake-bti.h>
#include <lib/driver/fake-platform-device/cpp/fake-pdev.h>
#include <lib/driver/fake-resource/cpp/fake-resource.h>
#include <lib/driver/platform-device/cpp/pdev.h>

namespace fdf_fake_platform_device {

void FakePDev::GetMmioById(GetMmioByIdRequestView request, GetMmioByIdCompleter::Sync& completer) {
  auto mmio = config_.mmios.find(request->index);
  if (mmio == config_.mmios.end()) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }
  fidl::Arena arena;
  auto builder = fuchsia_hardware_platform_device::wire::Mmio::Builder(arena);
  if (auto* mmio_info = std::get_if<fdf::PDev::MmioInfo>(&mmio->second); mmio_info) {
    builder.offset(mmio_info->offset).size(mmio_info->size);
    zx::vmo dup;
    mmio_info->vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
    builder.vmo(std::move(dup));
  } else {
    auto& mmio_buffer = std::get<fdf::MmioBuffer>(mmio->second);
    builder.offset(reinterpret_cast<size_t>(&mmio_buffer));
  }
  completer.ReplySuccess(builder.Build());
}

void FakePDev::GetMmioByName(GetMmioByNameRequestView request,
                             GetMmioByNameCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void FakePDev::GetInterruptById(GetInterruptByIdRequestView request,
                                GetInterruptByIdCompleter::Sync& completer) {
  auto itr = config_.irqs.find(request->index);
  if (itr == config_.irqs.end()) {
    if (!config_.use_fake_irq) {
      completer.ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }
    zx::interrupt irq;
    if (zx_status_t status = zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq);
        status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    return completer.ReplySuccess(std::move(irq));
  }
  zx::interrupt irq;
  itr->second.duplicate(ZX_RIGHT_SAME_RIGHTS, &irq);
  completer.ReplySuccess(std::move(irq));
}

void FakePDev::GetInterruptByName(GetInterruptByNameRequestView request,
                                  GetInterruptByNameCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void FakePDev::GetBtiById(GetBtiByIdRequestView request, GetBtiByIdCompleter::Sync& completer) {
  zx::bti bti;
  auto itr = config_.btis.find(request->index);
  if (itr == config_.btis.end()) {
    if (!config_.use_fake_bti) {
      completer.ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }
    zx_status_t status = fake_bti_create(bti.reset_and_get_address());
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
  } else {
    itr->second.duplicate(ZX_RIGHT_SAME_RIGHTS, &bti);
  }
  completer.ReplySuccess(std::move(bti));
}

void FakePDev::GetBtiByName(GetBtiByNameRequestView request,
                            GetBtiByNameCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void FakePDev::GetSmcById(GetSmcByIdRequestView request, GetSmcByIdCompleter::Sync& completer) {
  zx::resource smc;
  auto itr = config_.smcs.find(request->index);
  if (itr == config_.smcs.end()) {
    if (!config_.use_fake_smc) {
      completer.ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }
    zx_status_t status = fake_root_resource_create(smc.reset_and_get_address());
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
  } else {
    itr->second.duplicate(ZX_RIGHT_SAME_RIGHTS, &smc);
  }
  completer.ReplySuccess(std::move(smc));
}

void FakePDev::GetSmcByName(GetSmcByNameRequestView request,
                            GetSmcByNameCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void FakePDev::GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) {
  if (!config_.device_info.has_value()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  fdf::PDev::DeviceInfo& info = config_.device_info.value();
  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::NodeDeviceInfo::Builder(arena)
                             .vid(info.vid)
                             .pid(info.pid)
                             .did(info.did)
                             .mmio_count(info.mmio_count)
                             .irq_count(info.irq_count)
                             .bti_count(info.bti_count)
                             .smc_count(info.smc_count)
                             .metadata_count(info.metadata_count)
                             .name(info.name)
                             .Build());
}

void FakePDev::GetBoardInfo(GetBoardInfoCompleter::Sync& completer) {
  if (!config_.board_info.has_value()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  fdf::PDev::BoardInfo& info = config_.board_info.value();
  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::BoardInfo::Builder(arena)
                             .vid(info.vid)
                             .pid(info.pid)
                             .board_name(info.board_name)
                             .board_revision(info.board_revision)
                             .Build());
}

void FakePDev::GetPowerConfiguration(GetPowerConfigurationCompleter::Sync& completer) {
  static fidl::Arena arena;
  fidl::VectorView<fuchsia_hardware_power::wire::PowerElementConfiguration> value;
  value.Allocate(arena, config_.power_elements.size());
  for (size_t i = 0; i < config_.power_elements.size(); i++) {
    value[i] = fidl::ToWire(arena, config_.power_elements[i]);
  }
  completer.ReplySuccess(value);
}

void FakePDev::GetMetadata(GetMetadataRequestView request, GetMetadataCompleter::Sync& completer) {
  auto metadata = metadata_.find(request->type);
  if (metadata == metadata_.end()) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }
  completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(metadata->second));
}

void FakePDev::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {}

}  // namespace fdf_fake_platform_device

zx::result<fdf::MmioBuffer> fdf::internal::PDevMakeMmioBufferWeak(fdf::PDev::MmioInfo& pdev_mmio,
                                                                  uint32_t cache_policy) {
  if (pdev_mmio.vmo.is_valid()) {
    return MmioBuffer::Create(pdev_mmio.offset, pdev_mmio.size, std::move(pdev_mmio.vmo),
                              cache_policy);
  }

  auto* mmio_buffer = reinterpret_cast<MmioBuffer*>(pdev_mmio.offset);
  return zx::ok(std::move(*mmio_buffer));
}
