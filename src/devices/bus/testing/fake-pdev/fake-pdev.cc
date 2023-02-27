// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"

#include <lib/fake-bti/bti.h>
#include <lib/fake-resource/resource.h>

namespace fake_pdev {

zx::unowned_interrupt FakePDev::CreateVirtualInterrupt(uint32_t idx) {
  fbl::AutoLock al(&lock_);
  zx::interrupt irq;
  auto status = zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq);
  ZX_ASSERT(status == ZX_OK);
  zx::unowned_interrupt ret(irq);
  irqs_[idx] = std::move(irq);
  return ret;
}

zx_status_t FakePDev::PDevGetMmio(uint32_t index, pdev_mmio_t* out_mmio) {
  fbl::AutoLock al(&lock_);
  auto mmio = mmios_.find(index);
  if (mmio == mmios_.end()) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  zx::vmo dup;
  mmio->second.vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
  out_mmio->vmo = dup.release();
  out_mmio->offset = mmio->second.offset;
  out_mmio->size = mmio->second.size;
  return ZX_OK;
}

zx_status_t FakePDev::PDevGetBti(uint32_t index, zx::bti* out_bti) {
  fbl::AutoLock al(&lock_);
  auto bti = btis_.find(index);
  if (bti == btis_.end()) {
    if (use_fake_bti_) {
      return fake_bti_create(out_bti->reset_and_get_address());
    }
    return ZX_ERR_OUT_OF_RANGE;
  }
  bti->second.duplicate(ZX_RIGHT_SAME_RIGHTS, out_bti);
  return ZX_OK;
}

zx_status_t FakePDev::PDevGetSmc(uint32_t index, zx::resource* out_resource) {
  fbl::AutoLock al(&lock_);
  auto smc = smcs_.find(index);
  if (smc == smcs_.end()) {
    if (use_fake_smc_) {
      return fake_root_resource_create(out_resource->reset_and_get_address());
    }
    return ZX_ERR_OUT_OF_RANGE;
  }
  smc->second.duplicate(ZX_RIGHT_SAME_RIGHTS, out_resource);
  return ZX_OK;
}

zx_status_t FakePDev::PDevGetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq) {
  fbl::AutoLock al(&lock_);
  auto irq = irqs_.find(index);
  if (irq == irqs_.end()) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  irq->second.duplicate(ZX_RIGHT_SAME_RIGHTS, out_irq);
  return ZX_OK;
}

zx_status_t FakePDev::PDevGetDeviceInfo(pdev_device_info_t* out_info) {
  fbl::AutoLock al(&lock_);
  if (device_info_) {
    *out_info = *device_info_;
    return ZX_OK;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t FakePDev::PDevGetBoardInfo(pdev_board_info_t* out_info) {
  fbl::AutoLock al(&lock_);
  if (board_info_) {
    *out_info = *board_info_;
    return ZX_OK;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

void FakePDevFidl::GetMmio(GetMmioRequestView request, GetMmioCompleter::Sync& completer) {
  auto mmio = config_.mmios.find(request->index);
  if (mmio == config_.mmios.end()) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }
  zx::vmo dup;
  mmio->second.vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup);
  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::Mmio::Builder(arena)
                             .offset(mmio->second.offset)
                             .size(mmio->second.size)
                             .vmo(std::move(dup))
                             .Build());
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
