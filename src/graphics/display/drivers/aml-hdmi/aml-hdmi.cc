// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/aml-hdmi/aml-hdmi.h"

#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fidl/epitaph.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/resource.h>
#include <zircon/assert.h>
#include <zircon/status.h>
#include <zircon/syscalls/smc.h>

#include <cinttypes>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/auto_lock.h>

#include "src/graphics/display/lib/amlogic-hdmitx/amlogic-hdmitx.h"
#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/designware/color-param.h"
#include "src/graphics/display/lib/designware/hdmi-transmitter-controller-impl.h"
#include "src/graphics/display/lib/designware/hdmi-transmitter-controller.h"

namespace aml_hdmi {

zx_status_t AmlHdmiDevice::Bind() {
  // TODO(fxbug.dev/132267): Use the builder / factory pattern instead
  // of multiple stateful steps (such as Create() and Bind()) when
  // initializing the device.

  if (!pdev_.is_valid()) {
    zxlogf(ERROR, "Could not get ZX_PROTOCOL_PDEV protocol");
    return ZX_ERR_NO_RESOURCES;
  }

  // Map registers
  static constexpr uint32_t kHdmitxControllerIpIndex = 0;
  std::optional<fdf::MmioBuffer> hdmitx_controller_ip_mmio;
  zx_status_t status = pdev_.MapMmio(kHdmitxControllerIpIndex, &hdmitx_controller_ip_mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not map MMIO HDMITX Controller IP registers: %s",
           zx_status_get_string(status));
    return status;
  }

  {
    fbl::AllocChecker alloc_checker;
    std::unique_ptr<designware_hdmi::HdmiTransmitterController> designware_controller =
        fbl::make_unique_checked<designware_hdmi::HdmiTransmitterControllerImpl>(
            &alloc_checker, std::move(*hdmitx_controller_ip_mmio));
    if (!alloc_checker.check()) {
      zxlogf(ERROR, "Could not allocate memory for HdmiDw");
      return ZX_ERR_NO_MEMORY;
    }

    static constexpr uint32_t kHdmitxTopLevelIndex = 1;
    std::optional<fdf::MmioBuffer> hdmitx_top_level_mmio;
    status = pdev_.MapMmio(kHdmitxTopLevelIndex, &hdmitx_top_level_mmio);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Could not map MMIO HDMITX top-level registers: %s",
             zx_status_get_string(status));
      return status;
    }

    static constexpr uint32_t kSiliconProviderSmcIndex = 0;
    zx::resource smc;
    status = pdev_.GetSmc(kSiliconProviderSmcIndex, &smc);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Could not get secure monitor call (SMC) resource: %s",
             zx_status_get_string(status));
      return status;
    }

    hdmi_transmitter_ = fbl::make_unique_checked<amlogic_display::HdmiTransmitter>(
        &alloc_checker, std::move(designware_controller), std::move(*hdmitx_top_level_mmio),
        std::move(smc));
    if (!alloc_checker.check()) {
      zxlogf(ERROR, "Could not allocate memory for HdmiTransmitter");
      return ZX_ERR_NO_MEMORY;
    }
  }

  async_dispatcher_t* dispatcher =
      fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher());
  outgoing_ = component::OutgoingDirectory(dispatcher);

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  fuchsia_hardware_hdmi::Service::InstanceHandler handler({
      .device = bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
  });
  zx::result<> result = outgoing_->AddService<fuchsia_hardware_hdmi::Service>(std::move(handler));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory: %s", result.status_string());
    return result.status_value();
  }

  result = outgoing_->Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory: %s", result.status_string());
    return result.status_value();
  }

  std::array offers = {
      fuchsia_hardware_hdmi::Service::Name,
  };

  status = DdkAdd(ddk::DeviceAddArgs("aml-hdmi")
                      .set_fidl_service_offers(offers)
                      .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

void AmlHdmiDevice::Reset(ResetRequestView request, ResetCompleter::Sync& completer) {
  ZX_DEBUG_ASSERT(request->display_id == 1);  // only supports 1 display for now

  zx::result<> result = hdmi_transmitter_->Reset();

  if (result.is_ok()) {
    completer.ReplySuccess();
  } else {
    zxlogf(ERROR, "Failed to reset HDMI transmitter: %s", result.status_string());
    completer.ReplyError(result.error_value());
  }
}

void AmlHdmiDevice::ModeSet(ModeSetRequestView request, ModeSetCompleter::Sync& completer) {
  ZX_DEBUG_ASSERT(request->display_id == 1);  // only supports 1 display for now
  ZX_DEBUG_ASSERT(request->mode.has_color());
  ZX_DEBUG_ASSERT(request->mode.has_mode());
  const display::DisplayTiming display_timing = display::ToDisplayTiming(request->mode.mode());
  const designware_hdmi::ColorParam color_param =
      designware_hdmi::ToColorParam(request->mode.color());

  zx::result<> modeset_result = hdmi_transmitter_->ModeSet(display_timing, color_param);
  if (modeset_result.is_ok()) {
    completer.ReplySuccess();
  } else {
    zxlogf(ERROR, "Failed to set HDMI mode: %s", modeset_result.status_string());
    completer.ReplyError(modeset_result.error_value());
  }
}

void AmlHdmiDevice::EdidTransfer(EdidTransferRequestView request,
                                 EdidTransferCompleter::Sync& completer) {
  if (request->ops.count() < 1 || request->ops.count() > I2C_IMPL_MAX_RW_OPS) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  fbl::AllocChecker ac;
  fbl::Array<uint8_t> read_buffer(new (&ac) uint8_t[I2C_IMPL_MAX_TOTAL_TRANSFER],
                                  I2C_IMPL_MAX_TOTAL_TRANSFER);
  if (!ac.check()) {
    zxlogf(ERROR, "%s could not allocate read_buffer", __FUNCTION__);
    completer.ReplyError(ZX_ERR_INTERNAL);
    return;
  }
  fbl::Array<uint8_t> write_buffer(new (&ac) uint8_t[I2C_IMPL_MAX_TOTAL_TRANSFER],
                                   I2C_IMPL_MAX_TOTAL_TRANSFER);
  if (!ac.check()) {
    zxlogf(ERROR, "%s could not allocate write_buffer", __FUNCTION__);
    completer.ReplyError(ZX_ERR_INTERNAL);
    return;
  }

  i2c_impl_op_t op_list[I2C_IMPL_MAX_RW_OPS];
  size_t write_cnt = 0;
  size_t read_cnt = 0;
  uint8_t* p_writes = write_buffer.data();
  uint8_t* p_reads = read_buffer.data();
  for (size_t i = 0; i < request->ops.count(); ++i) {
    if (request->ops[i].is_write) {
      if (write_cnt >= request->write_segments_data.count()) {
        completer.ReplyError(ZX_ERR_INVALID_ARGS);
        return;
      }
      op_list[i].address = request->ops[i].address;
      memcpy(p_writes, request->write_segments_data[write_cnt].data(),
             request->write_segments_data[write_cnt].count());
      op_list[i].data_buffer = p_writes;
      op_list[i].data_size = request->write_segments_data[write_cnt].count();
      op_list[i].is_read = false;
      op_list[i].stop = false;
      p_writes += request->write_segments_data[write_cnt].count();
      write_cnt++;
    } else {
      if (read_cnt >= request->read_segments_length.count()) {
        completer.ReplyError(ZX_ERR_INVALID_ARGS);
        return;
      }
      op_list[i].address = request->ops[i].address;
      op_list[i].data_buffer = p_reads;
      op_list[i].data_size = request->read_segments_length[read_cnt];
      op_list[i].is_read = true;
      op_list[i].stop = false;
      p_reads += request->read_segments_length[read_cnt];
      read_cnt++;
    }
  }
  op_list[request->ops.count() - 1].stop = true;

  if (request->write_segments_data.count() != write_cnt) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (request->read_segments_length.count() != read_cnt) {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  zx::result<> i2c_transact_result = hdmi_transmitter_->I2cTransact(op_list, request->ops.count());

  if (i2c_transact_result.is_ok()) {
    fidl::Arena allocator;
    fidl::VectorView<fidl::VectorView<uint8_t>> reads(allocator, read_cnt);
    size_t read_ops_cnt = 0;
    for (size_t i = 0; i < request->ops.count(); ++i) {
      if (!op_list[i].is_read) {
        continue;
      }
      reads[read_ops_cnt] = fidl::VectorView<uint8_t>::FromExternal(
          const_cast<uint8_t*>(op_list[i].data_buffer), op_list[i].data_size);
      read_ops_cnt++;
    }
    completer.ReplySuccess(std::move(reads));
  } else {
    completer.ReplyError(i2c_transact_result.error_value());
  }
}

void AmlHdmiDevice::PrintHdmiRegisters(PrintHdmiRegistersCompleter::Sync& completer) {
  hdmi_transmitter_->PrintRegisters();
  completer.Reply();
}

// static
zx_status_t AmlHdmiDevice::Create(zx_device_t* parent) {
  fbl::AllocChecker alloc_checker;
  auto dev = fbl::make_unique_checked<aml_hdmi::AmlHdmiDevice>(&alloc_checker, parent);
  if (!alloc_checker.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  auto status = dev->Bind();
  if (status == ZX_OK) {
    // devmgr now owns the memory for `dev`.
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

AmlHdmiDevice::AmlHdmiDevice(zx_device_t* parent)
    : DeviceType(parent), pdev_(parent), loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {}

AmlHdmiDevice::AmlHdmiDevice(zx_device_t* parent, fdf::MmioBuffer hdmitx_top_level_mmio,
                             std::unique_ptr<designware_hdmi::HdmiTransmitterController> hdmi_dw,
                             zx::resource smc)
    : DeviceType(parent), pdev_(parent), loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
  fbl::AllocChecker alloc_checker;
  hdmi_transmitter_ = fbl::make_unique_checked<amlogic_display::HdmiTransmitter>(
      &alloc_checker, std::move(hdmi_dw), std::move(hdmitx_top_level_mmio), std::move(smc));
  ZX_ASSERT(alloc_checker.check());
}

AmlHdmiDevice::~AmlHdmiDevice() = default;

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* parent) { return AmlHdmiDevice::Create(parent); },
};

}  // namespace

}  // namespace aml_hdmi

ZIRCON_DRIVER(aml_hdmi, aml_hdmi::kDriverOps, "zircon", "0.1");
