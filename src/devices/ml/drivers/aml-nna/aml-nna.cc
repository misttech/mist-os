// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-nna.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <stdlib.h>
#include <unistd.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <memory>

#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/verisilicon/platform/cpp/bind.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "a5-nna-regs.h"
#include "s905d3-nna-regs.h"
#include "t931-nna-regs.h"

namespace {

// constexpr uint32_t kNna = 0;
constexpr uint32_t kHiu = 1;
constexpr uint32_t kPowerDomain = 2;
constexpr uint32_t kMemoryDomain = 3;
// constexpr uint32_t kSram = 5;
}  // namespace

namespace aml_nna {

zx_status_t AmlNnaDevice::Init() {
  if (nna_block_.nna_power_version == kNnaPowerDomain) {
    zx_status_t status = PowerDomainControl(true);
    if (status != ZX_OK) {
      zxlogf(ERROR, "PowerDomainControl failed: %s\n", zx_status_get_string(status));
      return status;
    }
  } else {
    power_mmio_.ClearBits32(nna_block_.nna_regs.domain_power_sleep_bits,
                            nna_block_.nna_regs.domain_power_sleep_offset);

    memory_pd_mmio_.Write32(0, nna_block_.nna_regs.hhi_mem_pd_reg0_offset);

    memory_pd_mmio_.Write32(0, nna_block_.nna_regs.hhi_mem_pd_reg1_offset);

    // set bit[12]=0
    auto clear_result = reset_->WriteRegister32(nna_block_.nna_regs.reset_level2_offset,
                                                aml_registers::NNA_RESET2_LEVEL_MASK, 0);
    if (!clear_result.ok()) {
      zxlogf(ERROR, "Failed to send request to clear reset register: %s",
             clear_result.status_string());
      return clear_result.status();
    }
    if (clear_result->is_error()) {
      zxlogf(ERROR, "Failed to clear reset register: %s",
             zx_status_get_string(clear_result->error_value()));
      return clear_result->error_value();
    }

    power_mmio_.ClearBits32(nna_block_.nna_regs.domain_power_iso_bits,
                            nna_block_.nna_regs.domain_power_iso_offset);

    // set bit[12]=1
    auto set_result = reset_->WriteRegister32(nna_block_.nna_regs.reset_level2_offset,
                                              aml_registers::NNA_RESET2_LEVEL_MASK,
                                              aml_registers::NNA_RESET2_LEVEL_MASK);
    if (!set_result.ok()) {
      zxlogf(ERROR, "Failed to send request to set reset register: %s", set_result.status_string());
      return set_result.status();
    }
    if (set_result->is_error()) {
      zxlogf(ERROR, "Failed to set reset register: %s",
             zx_status_get_string(set_result->error_value()));
      return set_result->error_value();
    }
  }
  // Setup Clocks.
  // VIPNANOQ Core clock
  hiu_mmio_.SetBits32(nna_block_.clock_core_control_bits, nna_block_.clock_control_offset);
  // VIPNANOQ Axi clock
  hiu_mmio_.SetBits32(nna_block_.clock_axi_control_bits, nna_block_.clock_control_offset);

  return ZX_OK;
}

zx_status_t AmlNnaDevice::PowerDomainControl(bool turn_on) {
  ZX_ASSERT(smc_monitor_.is_valid());
  static const zx_smc_parameters_t kSetPdCall =
      aml_pd_smc::CreatePdSmcCall(nna_block_.nna_domain_id, turn_on ? 1 : 0);

  zx_smc_result_t result;
  zx_status_t status = zx_smc_call(smc_monitor_.get(), &kSetPdCall, &result);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Call zx_smc_call failed: %s", zx_status_get_string(status));
  }

  return status;
}

// static
zx_status_t AmlNnaDevice::Create(void* ctx, zx_device_t* parent) {
  zx_status_t status;

  ddk::PDevFidl pdev = ddk::PDevFidl::FromFragment(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "Could not get platform device protocol");
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto reset_register_client =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_registers::Service::Device>(parent,
                                                                                  "register-reset");
  if (reset_register_client.is_error()) {
    return reset_register_client.status_value();
  }

  std::optional<fdf::MmioBuffer> hiu_mmio;
  status = pdev.MapMmio(kHiu, &hiu_mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.MapMmio failed %d\n", status);
    return status;
  }

  std::optional<fdf::MmioBuffer> power_mmio;
  status = pdev.MapMmio(kPowerDomain, &power_mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.MapMmio failed %d\n", status);
    return status;
  }

  std::optional<fdf::MmioBuffer> memory_pd_mmio;
  status = pdev.MapMmio(kMemoryDomain, &memory_pd_mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.MapMmio failed %d\n", status);
    return status;
  }

  pdev_device_info_t info;
  status = pdev.GetDeviceInfo(&info);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev.GetDeviceInfo failed %d\n", status);
    return status;
  }

  NnaBlock nna_block;
  zx::resource smc_monitor;
  switch (info.pid) {
    case PDEV_PID_AMLOGIC_A311D:
    case PDEV_PID_AMLOGIC_T931:
      nna_block = T931NnaBlock;
      break;
    case PDEV_PID_AMLOGIC_S905D3:
      nna_block = S905d3NnaBlock;
      break;
    case PDEV_PID_AMLOGIC_A5:
      nna_block = A5NnaBlock;
      status = pdev.GetSmc(0, &smc_monitor);
      if (status != ZX_OK) {
        zxlogf(ERROR, "unable to get sip monitor handle: %s", zx_status_get_string(status));
        return status;
      }
      break;
    default:
      zxlogf(ERROR, "unhandled PID 0x%x", info.pid);
      return ZX_ERR_INVALID_ARGS;
  }

  auto device = std::make_unique<AmlNnaDevice>(
      parent, std::move(*hiu_mmio), std::move(*power_mmio), std::move(*memory_pd_mmio),
      std::move(reset_register_client.value()), nna_block, std::move(smc_monitor),
      fdf::Dispatcher::GetCurrent()->async_dispatcher());

  if ((status = device->Init()) != ZX_OK) {
    zxlogf(ERROR, "Could not init device %d.", status);
    return status;
  }

  auto connector = [device = device.get()](zx::channel request) {
    using ServiceMember = fuchsia_hardware_platform_device::Service::Device;
    auto status =
        device_connect_fragment_fidl_protocol(device->parent(), "pdev", ServiceMember::ServiceName,
                                              ServiceMember::Name, request.release());
    if (status != ZX_OK) {
      zxlogf(ERROR, "could not forward pdev protocol: %s", zx_status_get_string(status));
    }
  };
  zx::result result = device->outgoing_.AddUnmanagedProtocolAt(
      connector, "svc/fuchsia.hardware.platform.device.Service/default", "device");
  if (result.is_error()) {
    zxlogf(ERROR, "could not add service to outgoing directory: %s", result.status_string());
    return result.error_value();
  }

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "could not add service to outgoing directory: %s", result.status_string());
    return endpoints.status_value();
  }
  if (zx::result result = device->outgoing_.Serve(std::move(endpoints->server));
      result.is_error()) {
    return result.error_value();
  }

  zx_device_prop_t props[] = {
      {BIND_PROTOCOL, 0, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE},
      {BIND_PLATFORM_DEV_VID, 0,
       bind_fuchsia_verisilicon_platform::BIND_PLATFORM_DEV_VID_VERISILICON},
      {BIND_PLATFORM_DEV_PID, 0, bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC},
      {BIND_PLATFORM_DEV_DID, 0,
       bind_fuchsia_verisilicon_platform::BIND_PLATFORM_DEV_DID_MAGMA_VIP},
  };

  std::array fidl_service_offers = {
      fuchsia_hardware_platform_device::Service::Name,
  };

  status = device->DdkAdd(ddk::DeviceAddArgs("aml-nna")
                              .set_props(props)
                              .set_fidl_service_offers(fidl_service_offers)
                              .set_outgoing_dir(endpoints->client.TakeChannel())
                              .forward_metadata(parent, 0));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not create aml nna device: %d\n", status);
    return status;
  }
  zxlogf(INFO, "Added aml_nna device\n");

  // intentionally leaked as it is now held by DevMgr.
  [[maybe_unused]] auto ptr = device.release();
  return status;
}

void AmlNnaDevice::DdkRelease() { delete this; }

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = AmlNnaDevice::Create;
  return ops;
}();

}  // namespace aml_nna

// clang-format off
ZIRCON_DRIVER(aml_nna, aml_nna::driver_ops, "zircon", "0.1");
