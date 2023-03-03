// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"

#include <lib/fake-bti/bti.h>
#include <lib/fake-resource/resource.h>

namespace fake_pdev {

void FakePDevFidl::GetMmio(GetMmioRequestView request, GetMmioCompleter::Sync& completer) {
  auto mmio = config_.mmios.find(request->index);
  if (mmio == config_.mmios.end()) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }
  fidl::Arena arena;
  auto builder = fuchsia_hardware_platform_device::wire::Mmio::Builder(arena)
                     .offset(mmio->second.offset)
                     .size(mmio->second.size);
  if (mmio->second.vmo.is_valid()) {
    zx::vmo dup;
    mmio->second.vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
    builder.vmo(std::move(dup));
  }
  completer.ReplySuccess(builder.Build());
}
void FakePDevFidl::GetInterrupt(GetInterruptRequestView request,
                                GetInterruptCompleter::Sync& completer) {
  auto itr = config_.irqs.find(request->index);
  if (itr == config_.irqs.end()) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }
  zx::interrupt irq;
  itr->second.duplicate(ZX_RIGHT_SAME_RIGHTS, &irq);
  completer.ReplySuccess(std::move(irq));
}

void FakePDevFidl::GetBti(GetBtiRequestView request, GetBtiCompleter::Sync& completer) {
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

void FakePDevFidl::GetSmc(GetSmcRequestView request, GetSmcCompleter::Sync& completer) {
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

void FakePDevFidl::GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) {
  if (!config_.device_info.has_value()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  pdev_device_info_t& info = config_.device_info.value();
  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::DeviceInfo::Builder(arena)
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

void FakePDevFidl::GetBoardInfo(GetBoardInfoCompleter::Sync& completer) {
  if (!config_.board_info.has_value()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  pdev_board_info_t& info = config_.board_info.value();
  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::BoardInfo::Builder(arena)
                             .vid(info.vid)
                             .pid(info.pid)
                             .board_name(info.board_name)
                             .board_revision(info.board_revision)
                             .Build());
}

}  // namespace fake_pdev
