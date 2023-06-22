// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "imx8m-sdmmc.h"

#include <fuchsia/hardware/gpio/cpp/banjo.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/io-buffer.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/phys-iter.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/fit/defer.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/mmio/mmio.h>
#include <lib/sdmmc/hw.h>
#include <lib/sync/completion.h>
#include <lib/zx/clock.h>
#include <lib/zx/pmt.h>
#include <lib/zx/time.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <algorithm>
#include <string>

#include <bits/limits.h>
#include <fbl/algorithm.h>

#include "imx8m-sdmmc-regs.h"

namespace {

constexpr uint32_t kSdFreqSetupHz = 400'000;

constexpr uint32_t kMaxTuningCount = 40;

constexpr zx_paddr_t k32BitPhysAddrMask = 0xffff'ffff;

constexpr uint32_t kDefaultWmlRegValue = 0x10401040;

constexpr zx::duration kInhibitWaitTime = zx::msec(1);
constexpr zx::duration kResetTime = zx::sec(1);
constexpr zx::duration kSdClockStableorOffTime = zx::usec(100);
constexpr zx::duration kExeTuneClearTime = zx::usec(50);
constexpr zx::duration kWaitYieldTime = zx::usec(2);
constexpr zx::duration kVoltageStabilizationTime = zx::msec(5);

constexpr uint32_t kDefaultStrobeDelay = 7;
constexpr uint32_t kDefaltStrobeSlaveUpdateInterval = 4;

constexpr uint32_t Lo32(zx_paddr_t val) { return val & 0xffffffff; }

// for 2M max transfer size for fully discontiguous
// also see SDMMC_PAGES_COUNT in fuchsia/hardware/sdmmc/c/banjo.h
constexpr int kDmaDescCount = 512;

}  // namespace

namespace imx8m_sdmmc {

void Imx8mSdmmc::DumpRegs() const {
  zxlogf(INFO, "BlockAttributes: 0x%x", BlockAttributes::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "CommandArgument: 0x%x", CommandArgument::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "CommandTransferType: 0x%x",
         CommandTransferType::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "PresentState: 0x%x", PresentState::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "ProtocolControl: 0x%x", ProtocolControl::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "SystemControl: 0x%x", SystemControl::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "InterruptStatus: 0x%x", InterruptStatus::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "InterruptStatusEnable: 0x%x",
         InterruptStatusEnable::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "InterruptSignalEnable: 0x%x",
         InterruptSignalEnable::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "AutoCmd12ErrorStatus: 0x%x",
         AutoCmd12ErrorStatus::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "MixerControl: 0x%x", MixerControl::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "VendorSpecificRegister: 0x%x",
         VendorSpecificRegister::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "TuningControl: 0x%x", TuningControl::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "ClockTuningControlStatus: 0x%x",
         ClockTuningControlStatus::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "DelayLineControl: 0x%x", DelayLineControl::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "DelayLineStatus: 0x%x", DelayLineStatus::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "StrobeDelayLineControl: 0x%x",
         StrobeDelayLineControl::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "StrobeDelayLineStatus: 0x%x",
         StrobeDelayLineStatus::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "AdmaErrorStatus: 0x%x", AdmaErrorStatus::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "AdmaSystemAddress: 0x%x", AdmaSystemAddress::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "WatermarkLevel: 0x%x\n", WatermarkLevel::Get().ReadFrom(&mmio_).reg_value());
}

void Imx8mSdmmc::PrepareCmd(const sdmmc_req_t& req, MixerControl* mix_ctrl,
                            CommandTransferType* command) {
  command->set_cmd_index(static_cast<uint16_t>(req.cmd_idx));

  if (req.cmd_flags & SDMMC_RESP_LEN_EMPTY) {
    command->set_response_type(CommandTransferType::kResponseTypeNone);
  } else if (req.cmd_flags & SDMMC_RESP_LEN_136) {
    command->set_response_type(CommandTransferType::kResponseType136Bits);
  } else if (req.cmd_flags & SDMMC_RESP_LEN_48) {
    command->set_response_type(CommandTransferType::kResponseType48Bits);
  } else if (req.cmd_flags & SDMMC_RESP_LEN_48B) {
    command->set_response_type(CommandTransferType::kResponseType48BitsWithBusy);
  }

  if (req.cmd_flags & SDMMC_CMD_TYPE_NORMAL) {
    command->set_cmd_type(CommandTransferType::kCommandTypeNormal);
  } else if (req.cmd_flags & SDMMC_CMD_TYPE_SUSPEND) {
    command->set_cmd_type(CommandTransferType::kCommandTypeSuspend);
  } else if (req.cmd_flags & SDMMC_CMD_TYPE_RESUME) {
    command->set_cmd_type(CommandTransferType::kCommandTypeResume);
  } else if (req.cmd_flags & SDMMC_CMD_TYPE_ABORT) {
    command->set_cmd_type(CommandTransferType::kCommandTypeAbort);
  }

  if (req.cmd_flags & SDMMC_RESP_CRC_CHECK) {
    command->set_cmd_crc_check(1);
  }
  if (req.cmd_flags & SDMMC_RESP_CMD_IDX_CHECK) {
    command->set_cmd_index_check(1);
  }
  if (req.cmd_flags & SDMMC_RESP_DATA_PRESENT) {
    command->set_data_present(1);
  }

  if (req.cmd_flags & SDMMC_CMD_AUTO12) {
    mix_ctrl->set_auto_cmd12_enable(1);
    mix_ctrl->set_auto_cmd23_enable(0);
  } else if (req.cmd_flags & SDMMC_CMD_AUTO23) {
    mix_ctrl->set_auto_cmd12_enable(0);
    mix_ctrl->set_auto_cmd23_enable(1);
  }

  if (req.cmd_flags & SDMMC_CMD_DMA_EN) {
    mix_ctrl->set_dma_enable(1);
  }

  if (req.cmd_flags & SDMMC_CMD_BLKCNT_EN) {
    mix_ctrl->set_block_count_enable(1);
  }
  if (req.cmd_flags & SDMMC_CMD_READ) {
    mix_ctrl->set_data_transfer_dir_select(1);
  }
  if (req.cmd_flags & SDMMC_CMD_MULTI_BLK) {
    mix_ctrl->set_multi_single_block_select(1);
  }
}

zx_status_t Imx8mSdmmc::WaitForReset(const SystemControl mask) {
  const zx::time deadline = zx::clock::get_monotonic() + kResetTime;
  do {
    if ((SystemControl::Get().ReadFrom(&mmio_).reg_value() & mask.reg_value()) == 0) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  } while (zx::clock::get_monotonic() <= deadline);

  zxlogf(ERROR, "timed out while waiting for reset");
  return ZX_ERR_TIMED_OUT;
}

zx_status_t Imx8mSdmmc::WaitForSdClockStableOrOff(const PresentState mask) const {
  const zx::time deadline = zx::clock::get_monotonic() + kSdClockStableorOffTime;
  do {
    if ((PresentState::Get().ReadFrom(&mmio_).reg_value() & mask.reg_value()) == mask.reg_value()) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  } while (zx::clock::get_monotonic() <= deadline);

  zxlogf(WARNING, "timed out while waiting for card clock stable or off");
  return ZX_ERR_TIMED_OUT;
}

zx_status_t Imx8mSdmmc::WaitForInhibit(const PresentState mask) const {
  const zx::time deadline = zx::clock::get_monotonic() + kInhibitWaitTime;
  do {
    if ((PresentState::Get().ReadFrom(&mmio_).reg_value() & mask.reg_value()) == 0) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  } while (zx::clock::get_monotonic() <= deadline);

  zxlogf(ERROR, "timed out while waiting for command/data inhibit");
  return ZX_ERR_TIMED_OUT;
}

void Imx8mSdmmc::EnableInterrupts() {
  InterruptSignalEnable::Get()
      .FromValue(0)
      .EnableErrorInterrupts()
      .EnableNormalInterrupts()
      .set_card_interrupt(interrupt_cb_.is_valid() ? 1 : 0)
      .WriteTo(&mmio_);
  InterruptStatusEnable::Get()
      .FromValue(0)
      .EnableErrorInterrupts()
      .EnableNormalInterrupts()
      .set_card_interrupt((interrupt_cb_.is_valid() && !card_interrupt_masked_) ? 1 : 0)
      .WriteTo(&mmio_);
}

void Imx8mSdmmc::DisableInterrupts() {
  InterruptSignalEnable::Get()
      .FromValue(0)
      .set_card_interrupt(interrupt_cb_.is_valid() ? 1 : 0)
      .WriteTo(&mmio_);
  InterruptStatusEnable::Get()
      .FromValue(0)
      .set_card_interrupt((interrupt_cb_.is_valid() && !card_interrupt_masked_) ? 1 : 0)
      .WriteTo(&mmio_);
}

bool Imx8mSdmmc::CmdStageComplete() {
  const uint32_t response_0 = Response::Get(0).ReadFrom(&mmio_).reg_value();
  const uint32_t response_1 = Response::Get(1).ReadFrom(&mmio_).reg_value();
  const uint32_t response_2 = Response::Get(2).ReadFrom(&mmio_).reg_value();
  const uint32_t response_3 = Response::Get(3).ReadFrom(&mmio_).reg_value();

  // Read the response data.
  if (pending_request_.cmd_flags & SDMMC_RESP_LEN_136) {
    pending_request_.response[0] = (response_0 << 8);
    pending_request_.response[1] = (response_1 << 8) | ((response_0 >> 24) & 0xFF);
    pending_request_.response[2] = (response_2 << 8) | ((response_1 >> 24) & 0xFF);
    pending_request_.response[3] = (response_3 << 8) | ((response_2 >> 24) & 0xFF);
  } else if (pending_request_.cmd_flags & (SDMMC_RESP_LEN_48 | SDMMC_RESP_LEN_48B)) {
    pending_request_.response[0] = response_0;
  }

  pending_request_.cmd_done = true;

  // We're done if the command has no data stage or if the data stage completed early
  if (pending_request_.data_done) {
    CompleteRequest();
  }

  return pending_request_.data_done;
}

bool Imx8mSdmmc::TransferComplete() {
  pending_request_.data_done = true;
  if (pending_request_.cmd_done) {
    CompleteRequest();
  }

  return pending_request_.cmd_done;
}

bool Imx8mSdmmc::DataStageReadReady() {
  if ((pending_request_.cmd_idx == MMC_SEND_TUNING_BLOCK) ||
      (pending_request_.cmd_idx == SD_SEND_TUNING_BLOCK)) {
    // This is the final interrupt expected for tuning transfers, so mark both command and data
    // phases complete.
    pending_request_.cmd_done = true;
    pending_request_.data_done = true;
    CompleteRequest();
    return true;
  }

  return false;
}

void Imx8mSdmmc::ErrorRecovery() {
  // Reset internal state machines
  {
    SystemControl::Get().ReadFrom(&mmio_).set_reset_cmd(1).WriteTo(&mmio_);
    [[maybe_unused]] auto _ = WaitForReset(SystemControl::Get().FromValue(0).set_reset_cmd(1));
  }
  {
    auto protocol_ctrl = ProtocolControl::Get().ReadFrom(&mmio_);
    SystemControl::Get().ReadFrom(&mmio_).set_reset_data(1).WriteTo(&mmio_);
    [[maybe_unused]] auto _ = WaitForReset(SystemControl::Get().FromValue(0).set_reset_data(1));
    ProtocolControl::Get().FromValue(0).set_reg_value(protocol_ctrl.reg_value()).WriteTo(&mmio_);
  }

  // Complete any pending txn with error status
  CompleteRequest();
}

void Imx8mSdmmc::CompleteRequest() {
  DisableInterrupts();
  sync_completion_signal(&req_completion_);
}

void Imx8mSdmmc::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                           const zx_packet_interrupt_t* interrupt) {
  if (status != ZX_OK) {
    if (status != ZX_ERR_CANCELED) {
      zxlogf(ERROR, "IRQ wait: %s", zx_status_get_string(status));
    }
    return;
  }

  fbl::AutoLock lock(&mtx_);
  // Acknowledge the IRQs that we stashed. IRQs are cleared by writing
  // 1s into the IRQs that fired.
  auto interrupt_status = InterruptStatus::Get().ReadFrom(&mmio_).WriteTo(&mmio_);

  zxlogf(DEBUG, "got irq 0x%08x en 0x%08x", interrupt_status.reg_value(),
         InterruptSignalEnable::Get().ReadFrom(&mmio_).reg_value());

  if (pending_request_.is_pending()) {
    HandleTransferInterrupt(interrupt_status);
  }

  if (interrupt_status.card_interrupt()) {
    // Disable the card interrupt and call the callback if there is one.
    InterruptStatusEnable::Get().ReadFrom(&mmio_).set_card_interrupt(0).WriteTo(&mmio_);
    card_interrupt_masked_ = true;
    if (interrupt_cb_.is_valid()) {
      interrupt_cb_.Callback();
    }
  }

  irq_.ack();
}

void Imx8mSdmmc::HandleTransferInterrupt(const InterruptStatus status) {
  if (status.ErrorInterrupt()) {
    pending_request_.status = status;
    ErrorRecovery();
    return;
  }

  // Clear the interrupt status to indicate that a normal interrupt was handled.
  pending_request_.status = InterruptStatus::Get().FromValue(0);
  if (status.buffer_read_ready() && DataStageReadReady()) {
    return;
  }
  if (status.cmd_complete() && CmdStageComplete()) {
    return;
  }
  if (status.transfer_complete()) {
    TransferComplete();
  }
}

zx_status_t Imx8mSdmmc::SdmmcRegisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo vmo,
                                         uint64_t offset, uint64_t size, uint32_t vmo_rights) {
  if (client_id >= std::size(registered_vmo_stores_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (vmo_rights == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  vmo_store::StoredVmo<OwnedVmoInfo> stored_vmo(std::move(vmo), OwnedVmoInfo{
                                                                    .offset = offset,
                                                                    .size = size,
                                                                    .rights = vmo_rights,
                                                                });
  const uint32_t read_perm = (vmo_rights & SDMMC_VMO_RIGHT_READ) ? ZX_BTI_PERM_READ : 0;
  const uint32_t write_perm = (vmo_rights & SDMMC_VMO_RIGHT_WRITE) ? ZX_BTI_PERM_WRITE : 0;
  zx_status_t status = stored_vmo.Pin(bti_, read_perm | write_perm, true);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to pin VMO %u for client %u: %s", vmo_id, client_id,
           zx_status_get_string(status));
    return status;
  }

  return registered_vmo_stores_[client_id].RegisterWithKey(vmo_id, std::move(stored_vmo));
}

zx_status_t Imx8mSdmmc::SdmmcUnregisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo* out_vmo) {
  if (client_id >= std::size(registered_vmo_stores_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  vmo_store::StoredVmo<OwnedVmoInfo>* const vmo_info =
      registered_vmo_stores_[client_id].GetVmo(vmo_id);
  if (!vmo_info) {
    return ZX_ERR_NOT_FOUND;
  }

  const zx_status_t status = vmo_info->vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, out_vmo);
  if (status != ZX_OK) {
    return status;
  }

  return registered_vmo_stores_[client_id].Unregister(vmo_id).status_value();
}

zx_status_t Imx8mSdmmc::SdmmcRequest(const sdmmc_req_t* req, uint32_t out_response[4]) {
  if (req->client_id >= std::size(registered_vmo_stores_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  zxlogf(DEBUG, "Sdmmc request cmd: %u, arg: 0x%x, flags: 0x%x", req->cmd_idx, req->arg,
         req->cmd_flags);

  sdhci::DmaDescriptorBuilder<OwnedVmoInfo> builder(*req, registered_vmo_stores_[req->client_id],
                                                    dma_boundary_alignment_, bti_.borrow());

  {
    fbl::AutoLock lock(&mtx_);

    // one command at a time
    if (pending_request_.is_pending()) {
      return ZX_ERR_SHOULD_WAIT;
    }

    if (zx_status_t status = StartRequest(*req, builder); status != ZX_OK) {
      return status;
    }
  }

  sync_completion_wait(&req_completion_, ZX_TIME_INFINITE);
  sync_completion_reset(&req_completion_);

  fbl::AutoLock lock(&mtx_);
  return FinishRequest(*req, out_response);
}

zx_status_t Imx8mSdmmc::StartRequest(const sdmmc_req_t& request,
                                     sdhci::DmaDescriptorBuilder<OwnedVmoInfo>& builder) {
  // Every command requires that the Command Inhibit is unset.
  auto inhibit_mask = PresentState::Get().FromValue(0).set_cmd_inhibit_cmd(1);

  // Busy type commands must also wait for the DATA Inhibit to be 0 UNLESS
  // it's an abort command which can be issued with the data lines active.
  if ((request.cmd_flags & SDMMC_RESP_LEN_48B) && (request.cmd_flags & SDMMC_CMD_TYPE_ABORT)) {
    inhibit_mask.set_cmd_inhibit_data(1);
  }

  // Wait for the inhibit masks from above to become 0 before issuing the command.
  zx_status_t status = WaitForInhibit(inhibit_mask);
  if (status != ZX_OK) {
    return status;
  }

  MixerControl mix_ctrl = MixerControl::Get()
                              .ReadFrom(&mmio_)
                              .set_dma_enable(0)
                              .set_block_count_enable(0)
                              .set_data_transfer_dir_select(0)
                              .set_multi_single_block_select(0)
                              .set_auto_cmd12_enable(0)
                              .set_auto_cmd23_enable(0);

  const bool is_tuning_request =
      request.cmd_idx == MMC_SEND_TUNING_BLOCK || request.cmd_idx == SD_SEND_TUNING_BLOCK;

  if (is_tuning_request) {
    // The SDHCI controller has special logic to handle tuning transfers, so there is no need to set
    // up any DMA buffers.
    BlockAttributes::Get()
        .FromValue(0)
        .set_block_count(0)
        .set_block_size(request.blocksize)
        .WriteTo(&mmio_);
  } else if (request.cmd_flags & SDMMC_RESP_DATA_PRESENT) {
    if (request.blocksize > BlockAttributes::kMaxBlockSize) {
      return ZX_ERR_OUT_OF_RANGE;
    }
    if (request.blocksize == 0) {
      return ZX_ERR_INVALID_ARGS;
    }

    if (const zx_status_t status = SetUpDma(request, builder); status != ZX_OK) {
      return status;
    }

    if (builder.block_count() > BlockAttributes::kMaxBlockCount) {
      zxlogf(ERROR, "Block count (%lu) exceeds the maximum (%u)", builder.block_count(),
             BlockAttributes::kMaxBlockCount);
      return ZX_ERR_OUT_OF_RANGE;
    }

    mix_ctrl.set_dma_enable(1).set_multi_single_block_select(builder.block_count() > 1 ? 1 : 0);

    BlockAttributes::Get()
        .FromValue(0)
        .set_block_count(static_cast<uint32_t>(builder.block_count()))
        .set_block_size(request.blocksize)
        .WriteTo(&mmio_);
  } else {
    BlockAttributes::Get().FromValue(0).set_block_count(0).set_block_size(0).WriteTo(&mmio_);
  }

  CommandTransferType command = CommandTransferType::Get().FromValue(0);
  PrepareCmd(request, &mix_ctrl, &command);

  CommandArgument::Get().FromValue(request.arg).WriteTo(&mmio_);

  // Clear any pending interrupts before starting the transaction.
  auto irq_mask = InterruptSignalEnable::Get().ReadFrom(&mmio_);
  InterruptStatus::Get().FromValue(irq_mask.reg_value()).WriteTo(&mmio_);

  pending_request_.Init(request);

  // Unmask and enable interrupts
  EnableInterrupts();

  // Start command
  mix_ctrl.WriteTo(&mmio_);
  command.WriteTo(&mmio_);

  return ZX_OK;
}

zx_status_t Imx8mSdmmc::SetUpDma(const sdmmc_req_t& request,
                                 sdhci::DmaDescriptorBuilder<OwnedVmoInfo>& builder) {
  const cpp20::span buffers{request.buffers_list, request.buffers_count};
  zx_status_t status;
  for (const auto& buffer : buffers) {
    if ((status = builder.ProcessBuffer(buffer)) != ZX_OK) {
      return status;
    }
  }

  size_t descriptor_size;
  const cpp20::span descriptors{reinterpret_cast<AdmaDescriptor64*>(iobuf_.virt()), kDmaDescCount};
  descriptor_size = sizeof(descriptors[0]);
  status = builder.BuildDmaDescriptors(descriptors);

  if (status != ZX_OK) {
    return status;
  }

  status = iobuf_.CacheOp(ZX_VMO_OP_CACHE_CLEAN, 0, builder.descriptor_count() * descriptor_size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to clean cache: %s", zx_status_get_string(status));
    return status;
  }

  AdmaSystemAddress::Get().FromValue(Lo32(iobuf_.phys())).WriteTo(&mmio_);
  return ZX_OK;
}

zx_status_t Imx8mSdmmc::FinishRequest(const sdmmc_req_t& request, uint32_t out_response[4]) {
  if (pending_request_.cmd_done) {
    memcpy(out_response, pending_request_.response, sizeof(uint32_t) * 4);
  }

  if (request.cmd_flags & SDMMC_CMD_TYPE_ABORT) {
    // SDHCI spec section 3.8.2: reset the data line after an abort to discard data in the buffer.
    [[maybe_unused]] auto _ =
        WaitForReset(SystemControl::Get().FromValue(0).set_reset_cmd(1).set_reset_data(1));
  }

  if ((request.cmd_flags & SDMMC_RESP_DATA_PRESENT) && (request.cmd_flags & SDMMC_CMD_READ)) {
    const cpp20::span<const sdmmc_buffer_region_t> regions{request.buffers_list,
                                                           request.buffers_count};
    for (const auto& region : regions) {
      if (region.type != SDMMC_BUFFER_TYPE_VMO_HANDLE) {
        continue;
      }

      // Invalidate the cache so that the next CPU read will pick up data that was written to main
      // memory by the controller.
      zx_status_t status = zx_vmo_op_range(region.buffer.vmo, ZX_VMO_OP_CACHE_CLEAN_INVALIDATE,
                                           region.offset, region.size, nullptr, 0);
      if (status != ZX_OK) {
        zxlogf(ERROR, "Failed to clean/invalidate cache: %s", zx_status_get_string(status));
        return status;
      }
    }
  }

  const InterruptStatus interrupt_status = pending_request_.status;
  pending_request_.Reset();

  if (!interrupt_status.ErrorInterrupt()) {
    return ZX_OK;
  }

  if (interrupt_status.tuning_error()) {
    zxlogf(ERROR, "Tuning error");
  }
  if (interrupt_status.dma_error()) {
    zxlogf(ERROR, "ADMA error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.auto_cmd_error()) {
    zxlogf(ERROR, "Auto cmd error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.data_end_bit_error()) {
    zxlogf(ERROR, "Data end bit error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.data_crc_error()) {
    if (request.suppress_error_messages) {
      zxlogf(DEBUG, "Data CRC error cmd%u", request.cmd_idx);
    } else {
      zxlogf(ERROR, "Data CRC error cmd%u", request.cmd_idx);
    }
  }
  if (interrupt_status.data_timeout_error()) {
    zxlogf(ERROR, "Data timeout error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.cmd_index_error()) {
    zxlogf(ERROR, "Command index error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.cmd_end_bit_error()) {
    zxlogf(ERROR, "Command end bit error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.cmd_crc_error()) {
    if (request.suppress_error_messages) {
      zxlogf(DEBUG, "Command CRC error cmd%u", request.cmd_idx);
    } else {
      zxlogf(ERROR, "Command CRC error cmd%u", request.cmd_idx);
    }
  }
  if (interrupt_status.cmd_timeout_error()) {
    if (request.suppress_error_messages) {
      zxlogf(DEBUG, "Command timeout error cmd%u", request.cmd_idx);
    } else {
      zxlogf(ERROR, "Command timeout error cmd%u", request.cmd_idx);
    }
  }

  return ZX_ERR_IO;
}

zx_status_t Imx8mSdmmc::SdmmcHostInfo(sdmmc_host_info_t* out_info) {
  memcpy(out_info, &info_, sizeof(info_));
  return ZX_OK;
}

zx_status_t Imx8mSdmmc::SdmmcSetSignalVoltage(sdmmc_voltage_t voltage) {
  fbl::AutoLock lock(&mtx_);

  // Validate the controller supports the requested voltage
  if ((voltage == SDMMC_VOLTAGE_V330) && !(info_.caps & SDMMC_HOST_CAP_VOLTAGE_330)) {
    zxlogf(DEBUG, "3.3V signal voltage not supported");
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto vendor_reg1 = VendorSpecificRegister::Get().ReadFrom(&mmio_);
  uint16_t voltage_1v8_value = 0;
  switch (voltage) {
    case SDMMC_VOLTAGE_V180: {
      voltage_1v8_value = 1;
      break;
    }
    case SDMMC_VOLTAGE_V330: {
      voltage_1v8_value = 0;
      break;
    }
    default:
      zxlogf(ERROR, "unknown signal voltage value %u", voltage);
      return ZX_ERR_INVALID_ARGS;
  }

  vendor_reg1.set_voltage_select(voltage_1v8_value).WriteTo(&mmio_);

  // Wait 5ms for the regulator to stabilize.
  zx::nanosleep(zx::deadline_after(kVoltageStabilizationTime));

  if (vendor_reg1.ReadFrom(&mmio_).voltage_select() != voltage_1v8_value) {
    zxlogf(ERROR, "voltage regulator output did not become stable");
    // Cut power to the card if the voltage switch failed.
    if (power_gpio_.is_valid()) {
      power_gpio_.ConfigOut(0);
    }
    return ZX_ERR_INTERNAL;
  }

  zxlogf(DEBUG, "switch signal voltage to %d", voltage);

  return ZX_OK;
}

zx_status_t Imx8mSdmmc::SdmmcSetBusWidth(sdmmc_bus_width_t bus_width) {
  fbl::AutoLock lock(&mtx_);

  auto protocol_ctrl = ProtocolControl::Get().ReadFrom(&mmio_);

  if ((bus_width == SDMMC_BUS_WIDTH_EIGHT) && !(info_.caps & SDMMC_HOST_CAP_BUS_WIDTH_8)) {
    zxlogf(DEBUG, "8-bit bus width not supported");
    return ZX_ERR_NOT_SUPPORTED;
  }

  switch (bus_width) {
    case SDMMC_BUS_WIDTH_ONE:
      protocol_ctrl.set_data_transfer_width(static_cast<uint8_t>(ProtocolControl::kBusWidth1Bit));
      break;
    case SDMMC_BUS_WIDTH_FOUR:
      protocol_ctrl.set_data_transfer_width(static_cast<uint8_t>(ProtocolControl::kBusWidth4Bit));
      break;
    case SDMMC_BUS_WIDTH_EIGHT:
      protocol_ctrl.set_data_transfer_width(static_cast<uint8_t>(ProtocolControl::kBusWidth8Bit));
      break;
    default:
      zxlogf(ERROR, "unknown bus width value %u", bus_width);
      return ZX_ERR_INVALID_ARGS;
  }

  protocol_ctrl.WriteTo(&mmio_);

  return ZX_OK;
}

uint32_t Imx8mSdmmc::GetBaseClock() const {
  // TODO(fxbug.dev/121201): Get clock rate from clock fragment
  return 400000000;
}

zx_status_t Imx8mSdmmc::SetUsdhcClock(uint32_t base_clock, uint32_t bus_freq) {
  VendorSpecificRegister::Get().ReadFrom(&mmio_).set_force_clk(0).WriteTo(&mmio_);
  {
    [[maybe_unused]] auto _ =
        WaitForSdClockStableOrOff(PresentState::Get().FromValue(0).set_sd_clock_gated_off(1));
  }

  if (bus_freq == 0) {
    return ZX_OK;
  }

  uint32_t ddr_prescaler_mul = is_ddr_ ? 2 : 1;
  uint32_t prescaler = 1;
  uint32_t divisor = 1;

  while (((base_clock / (16 * prescaler * ddr_prescaler_mul)) > bus_freq) && (prescaler < 256)) {
    prescaler *= 2;
  }

  while (((base_clock / (divisor * prescaler * ddr_prescaler_mul)) > bus_freq) && (divisor < 16)) {
    divisor++;
  }

  prescaler >>= 1;
  divisor -= 1;

  SystemControl::Get()
      .ReadFrom(&mmio_)
      .set_sdclk_prescaler(prescaler)
      .set_sdclk_divisor(divisor)
      .WriteTo(&mmio_);

  {
    [[maybe_unused]] auto _ =
        WaitForSdClockStableOrOff(PresentState::Get().FromValue(0).set_sd_clock_stable(1));
  }

  if (timing_ == SDMMC_TIMING_HS400) {
    SetStrobeDll();
  }

  VendorSpecificRegister::Get().ReadFrom(&mmio_).set_force_clk(1).WriteTo(&mmio_);

  return ZX_OK;
}

zx_status_t Imx8mSdmmc::SdmmcSetBusFreq(uint32_t bus_freq) {
  fbl::AutoLock lock(&mtx_);

  uint32_t base_clock = GetBaseClock();

  if (bus_freq > base_clock) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto status = SetUsdhcClock(base_clock, bus_freq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Can't set imx8m-sdmmc(base clock %u) bus freq to %u error: %d", base_clock,
           bus_freq, status);
    return status;
  }

  bus_freq_ = bus_freq;

  return ZX_OK;
}

void Imx8mSdmmc::ResetTuning() {
  AutoCmd12ErrorStatus::Get()
      .ReadFrom(&mmio_)
      .set_execute_tuning(0)
      .set_sample_clock_select(0)
      .WriteTo(&mmio_);
  const zx::time deadline = zx::clock::get_monotonic() + kExeTuneClearTime;
  while (AutoCmd12ErrorStatus::Get().ReadFrom(&mmio_).execute_tuning()) {
    if (zx::clock::get_monotonic() > deadline) {
      zxlogf(WARNING, "execute_tuning bit clear failed");
      break;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  }

  // To reset standard tuning circuit completely, after clearing execute_tuning
  // bit, also need to clear bit buffer_read_ready, this operation finally clears
  // the USDHC IP internal logic flag execute_tuning_with_clr_buf, which make sure
  // the following normal data transfer are not impacted by standard tuning logic
  // used before.
  InterruptStatus::Get().ReadFrom(&mmio_).set_buffer_read_ready(1).WriteTo(&mmio_);
}

void Imx8mSdmmc::SetStrobeDll() {
  {
    // Disable clock before enabling stobe dll
    VendorSpecificRegister::Get().ReadFrom(&mmio_).set_force_clk(0).WriteTo(&mmio_);
    [[maybe_unused]] auto _ =
        WaitForSdClockStableOrOff(PresentState::Get().FromValue(0).set_sd_clock_gated_off(1));
  }

  // Write and then clear reset bit
  StrobeDelayLineControl::Get().FromValue(0).set_reset(1).WriteTo(&mmio_);
  StrobeDelayLineControl::Get().FromValue(0).WriteTo(&mmio_);

  StrobeDelayLineControl::Get()
      .FromValue(0)
      .set_enable(1)
      .set_slave_update_interval(kDefaltStrobeSlaveUpdateInterval)
      .set_slave_delay_target(kDefaultStrobeDelay)
      .WriteTo(&mmio_);

  uint32_t timeout = 0;
  auto strobe_dll_status = StrobeDelayLineStatus::Get().ReadFrom(&mmio_);
  while ((strobe_dll_status.reference_dll_lock_status() == 0) ||
         (strobe_dll_status.slave_dll_lock_status() == 0)) {
    if (timeout > 10) {
      zxlogf(WARNING, "Stobe DLL REF/SLV not locked in 50us");
      break;
    }
    zx::nanosleep(zx::deadline_after(zx::usec(5)));
    timeout++;
    strobe_dll_status = StrobeDelayLineStatus::Get().ReadFrom(&mmio_);
  }
}

zx_status_t Imx8mSdmmc::SdmmcSetTiming(sdmmc_timing_t timing) {
  {
    fbl::AutoLock lock(&mtx_);

    VendorSpecificRegister::Get().ReadFrom(&mmio_).set_force_clk(0).WriteTo(&mmio_);
    {
      [[maybe_unused]] auto _ =
          WaitForSdClockStableOrOff(PresentState::Get().FromValue(0).set_sd_clock_gated_off(1));
    }

    auto mix_ctrl = MixerControl::Get().ReadFrom(&mmio_);

    mix_ctrl.set_ddr_mode_select(0).set_hs400_enable(0);
    is_ddr_ = false;

    switch (timing) {
      case SDMMC_TIMING_LEGACY:
        ResetTuning();
        break;
      case SDMMC_TIMING_SDR12:
      case SDMMC_TIMING_SDR25:
      case SDMMC_TIMING_SDR50:
      case SDMMC_TIMING_SDR104:
      case SDMMC_TIMING_HS:
      case SDMMC_TIMING_HS200:
        break;
      case SDMMC_TIMING_HSDDR:
      case SDMMC_TIMING_DDR50:
        mix_ctrl.set_ddr_mode_select(1);
        is_ddr_ = true;
        break;
      case SDMMC_TIMING_HS400:
        mix_ctrl.set_ddr_mode_select(1).set_hs400_enable(1);
        is_ddr_ = true;
        break;
      default:
        zxlogf(ERROR, "unknown timing value %u", timing);
        return ZX_ERR_INVALID_ARGS;
    }

    mix_ctrl.WriteTo(&mmio_);
    timing_ = timing;
  }

  return SdmmcSetBusFreq(bus_freq_);
}

zx_status_t Imx8mSdmmc::SdmmcHwReset() { return ZX_OK; }

void Imx8mSdmmc::AutoTuningModeSelect(uint32_t buswidth) {
  auto vendor_spec2_reg = VendorSpecificRegister2::Get().ReadFrom(&mmio_);
  switch (buswidth) {
    case ProtocolControl::kBusWidth1Bit:
      vendor_spec2_reg.set_tuning_data_enable(VendorSpecificRegister2::kTuning1Bit);
      break;
    case ProtocolControl::kBusWidth4Bit:
      vendor_spec2_reg.set_tuning_data_enable(VendorSpecificRegister2::kTuning4Bit);
      break;
    case ProtocolControl::kBusWidth8Bit:
      vendor_spec2_reg.set_tuning_data_enable(VendorSpecificRegister2::kTuning8Bit);
      break;
  }
  vendor_spec2_reg.set_tuning_cmd_enable(1);
  vendor_spec2_reg.WriteTo(&mmio_);
}

zx_status_t Imx8mSdmmc::SdmmcPerformTuning(uint32_t cmd_idx) {
  uint32_t blocksize;

  {
    fbl::AutoLock lock(&mtx_);
    uint32_t buswidth = ProtocolControl::Get().ReadFrom(&mmio_).data_transfer_width();
    blocksize = (buswidth == ProtocolControl::kBusWidth8Bit) ? 128 : 64;
    AutoTuningModeSelect(buswidth);
    AutoCmd12ErrorStatus::Get()
        .ReadFrom(&mmio_)
        .set_sample_clock_select(0)
        .set_execute_tuning(1)
        .WriteTo(&mmio_);
    MixerControl::Get()
        .ReadFrom(&mmio_)
        .set_feedback_clk_src_select(1)
        .set_auto_tuning_enable(1)
        .WriteTo(&mmio_);
  }

  const sdmmc_req_t req = {
      .cmd_idx = cmd_idx,
      .cmd_flags = MMC_SEND_TUNING_BLOCK_FLAGS,
      .arg = 0,
      .blocksize = blocksize,
      .suppress_error_messages = true,
      .client_id = 0,
      .buffers_count = 0,
  };
  uint32_t unused_response[4];

  auto auto_cmd_status = AutoCmd12ErrorStatus::Get().FromValue(0);
  {
    fbl::AutoLock lock(&mtx_);
    auto_cmd_status.ReadFrom(&mmio_);
  }
  for (uint32_t count = 0; (count < kMaxTuningCount) && (auto_cmd_status.execute_tuning());
       count++) {
    zx_status_t status = SdmmcRequest(&req, unused_response);
    if (status != ZX_OK) {
      zxlogf(ERROR, "MMC_SEND_TUNING_BLOCK error %s", zx_status_get_string(status));
      return status;
    }

    fbl::AutoLock lock(&mtx_);
    auto_cmd_status.ReadFrom(&mmio_);
  }

  {
    fbl::AutoLock lock(&mtx_);
    auto_cmd_status.ReadFrom(&mmio_);
  }

  const bool fail = auto_cmd_status.execute_tuning() || !auto_cmd_status.sample_clock_select();

  zxlogf(DEBUG, "tuning result %d", fail);

  return fail ? ZX_ERR_IO : ZX_OK;
}

zx_status_t Imx8mSdmmc::SdmmcRegisterInBandInterrupt(
    const in_band_interrupt_protocol_t* interrupt_cb) {
  fbl::AutoLock lock(&mtx_);

  interrupt_cb_ = ddk::InBandInterruptProtocolClient(interrupt_cb);

  InterruptSignalEnable::Get().ReadFrom(&mmio_).set_card_interrupt(1).WriteTo(&mmio_);
  InterruptStatusEnable::Get()
      .ReadFrom(&mmio_)
      .set_card_interrupt(card_interrupt_masked_ ? 0 : 1)
      .WriteTo(&mmio_);

  // Call the callback if an interrupt was raised before it was registered.
  if (card_interrupt_masked_) {
    interrupt_cb_.Callback();
  }

  return ZX_OK;
}

void Imx8mSdmmc::SdmmcAckInBandInterrupt() {
  fbl::AutoLock lock(&mtx_);
  InterruptStatusEnable::Get().ReadFrom(&mmio_).set_card_interrupt(1).WriteTo(&mmio_);
  card_interrupt_masked_ = false;
}

void Imx8mSdmmc::DdkUnbind(ddk::UnbindTxn txn) {
  irq_handler_.Cancel();
  irq_.destroy();

  txn.Reply();
}

void Imx8mSdmmc::DdkRelease() { delete this; }

zx_status_t Imx8mSdmmc::Init(const fuchsia_nxp_sdmmc::wire::SdmmcMetadata& metadata) {
  zx_status_t status;

  // Power on the card
  if (power_gpio_.is_valid()) {
    power_gpio_.ConfigOut(1);
  }

  {
    SystemControl::Get().ReadFrom(&mmio_).set_reset_all(1).WriteTo(&mmio_);
    [[maybe_unused]] auto _ = WaitForReset(
        SystemControl::Get().FromValue(0).set_reset_cmd(1).set_reset_data(1).set_reset_all(1));
  }

  // imx8m* RM says to set clock bits(3-0) to 1 without giving
  // any description regarding those bits, so set them here.
  SystemControl::Get().ReadFrom(&mmio_).set_clocks(0xf).WriteTo(&mmio_);
  MixerControl::Get().FromValue(0).set_enable(1).WriteTo(&mmio_);
  AutoCmd12ErrorStatus::Get().FromValue(0).WriteTo(&mmio_);
  ClockTuningControlStatus::Get().FromValue(0).WriteTo(&mmio_);
  DelayLineControl::Get().FromValue(0).WriteTo(&mmio_);

  // Restore default watermark level
  WatermarkLevel::Get().FromValue(kDefaultWmlRegValue).WriteTo(&mmio_);

  // Restore burst length default value
  ProtocolControl::Get().ReadFrom(&mmio_).set_burst_length_enable(1).WriteTo(&mmio_);

  // Enable standard tuning
  TuningControl::Get()
      .ReadFrom(&mmio_)
      .set_tuning_start_tap(metadata.tuning_start_tap)
      .set_tuning_step(metadata.tuning_step)
      .set_std_tuning_enable(1)
      .set_tuning_cmd_crc_check_disable(1)
      .WriteTo(&mmio_);

  {
    SystemControl::Get().ReadFrom(&mmio_).set_reset_all(1).WriteTo(&mmio_);
    [[maybe_unused]] auto _ = WaitForReset(
        SystemControl::Get().FromValue(0).set_reset_cmd(1).set_reset_data(1).set_reset_all(1));
  }

  auto caps = HostControllerCapabilities::Get().ReadFrom(&mmio_);

  // Get controller capabilities
  info_.caps |= ((metadata.bus_width == 8) ? SDMMC_HOST_CAP_BUS_WIDTH_8 : 0);
  if (caps.adma_support()) {
    info_.caps |= SDMMC_HOST_CAP_DMA;
  }
  if (caps.voltage_3v3_support()) {
    info_.caps |= SDMMC_HOST_CAP_VOLTAGE_330;
  }
  if (caps.sdr50_support()) {
    info_.caps |= SDMMC_HOST_CAP_SDR50;
  }
  if (caps.ddr50_support()) {
    info_.caps |= SDMMC_HOST_CAP_DDR50;
  }
  if (caps.sdr104_support()) {
    info_.caps |= SDMMC_HOST_CAP_SDR104;
  }
  if (!caps.use_tuning_for_sdr50()) {
    info_.caps |= SDMMC_HOST_CAP_NO_TUNING_SDR50;
  }
  info_.caps |= SDMMC_HOST_CAP_AUTO_CMD12;

  // The core has been reset, which should have stopped any DMAs that were happening when the driver
  // started. It is now safe to release quarantined pages.
  if ((status = bti_.release_quarantine()) != ZX_OK) {
    zxlogf(ERROR, "Failed to release quarantined pages: %d", status);
    return status;
  }

  status = iobuf_.Init(bti_.get(), kDmaDescCount * sizeof(AdmaDescriptor64),
                       IO_BUFFER_RW | IO_BUFFER_CONTIG);
  if ((iobuf_.phys() & k32BitPhysAddrMask) != iobuf_.phys()) {
    zxlogf(ERROR, "Got 64-bit physical address, only 32-bit DMA is supported");
    return ZX_ERR_NOT_SUPPORTED;
  }
  if (status != ZX_OK) {
    zxlogf(ERROR, "Error allocating DMA descriptors");
    return status;
  }
  info_.max_transfer_size = kDmaDescCount * zx_system_get_page_size();

  // SdmmcSetTiming() calls SdmmcSetBusFreq(bus_freq_) internally so
  // initialize bus_freq_ with kSdFreqSetupHz.
  bus_freq_ = kSdFreqSetupHz;

  status = SdmmcSetTiming(SDMMC_TIMING_LEGACY);
  if (status != ZX_OK) {
    return status;
  }

  status = SdmmcSetBusWidth(SDMMC_BUS_WIDTH_ONE);
  if (status != ZX_OK) {
    return status;
  }

  status = SdmmcSetSignalVoltage(SDMMC_VOLTAGE_V330);
  if (status != ZX_OK) {
    return status;
  }

  {
    fbl::AutoLock lock(&mtx_);

    SystemControl::Get().ReadFrom(&mmio_).set_data_timeout_counter_value(0xe).WriteTo(&mmio_);
    VendorSpecificRegister2::Get().ReadFrom(&mmio_).set_busy_rsp_irq(1).WriteTo(&mmio_);
    ProtocolControl::Get()
        .ReadFrom(&mmio_)
        .set_dma_select(ProtocolControl::kDmaSelAdma2)
        .WriteTo(&mmio_);
    MixerControl::Get().FromValue(0).set_enable(1).WriteTo(&mmio_);

    DisableInterrupts();
  }

  irq_handler_.set_object(irq_.get());
  irq_handler_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());

  return ZX_OK;
}

zx_status_t Imx8mSdmmc::Create(void* ctx, zx_device_t* parent) {
  auto pdev = ddk::PDevFidl::FromFragment(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "Could not get pdev.");
    return ZX_ERR_NO_RESOURCES;
  }

  zx::bti bti;
  zx_status_t status = ZX_OK;
  if ((status = pdev.GetBti(0, &bti)) != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI: %s", zx_status_get_string(status));
    return status;
  }

  std::optional<fdf::MmioBuffer> mmio;
  status = pdev.MapMmio(0, &mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get mmio: %s", zx_status_get_string(status));
    return status;
  }

  auto decoded = ddk::GetEncodedMetadata<fuchsia_nxp_sdmmc::wire::SdmmcMetadata>(
      parent, DEVICE_METADATA_PRIVATE);
  if (!decoded.is_ok()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      zxlogf(INFO, "No init metadata provided");
    } else {
      zxlogf(ERROR, "Failed to decode metadata: %s", decoded.status_string());
    }
    return decoded.status_value();
  }

  zx::interrupt irq;
  if ((status = pdev.GetInterrupt(0, 0, &irq)) != ZX_OK) {
    zxlogf(ERROR, "Failed to get interrupt: %s", zx_status_get_string(status));
    return status;
  }

  pdev_device_info_t dev_info;
  if ((status = pdev.GetDeviceInfo(&dev_info)) != ZX_OK) {
    zxlogf(ERROR, "Failed to get device info: %s", zx_status_get_string(status));
    return status;
  }

  ddk::GpioProtocolClient power_gpio(parent, "power-gpio");

  auto dev = std::make_unique<Imx8mSdmmc>(parent, *std::move(mmio), std::move(bti), std::move(irq),
                                          0, power_gpio);

  if ((status = dev->Init(*decoded.value())) != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize imx8m-sdmmc device");
    return status;
  }

  status = dev->DdkAdd("imx8m-sdmmc");
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: SDMMC device_add failed.", __func__);
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* placeholder = dev.release();
  return ZX_OK;
}

static constexpr zx_driver_ops_t imx8m_sdmmc_driver_ops = []() {
  zx_driver_ops_t driver_ops = {};
  driver_ops.version = DRIVER_OPS_VERSION;
  driver_ops.bind = Imx8mSdmmc::Create;
  return driver_ops;
}();

}  // namespace imx8m_sdmmc

ZIRCON_DRIVER(imx8m_sdmmc, imx8m_sdmmc::imx8m_sdmmc_driver_ops, "zircon", "0.1");
