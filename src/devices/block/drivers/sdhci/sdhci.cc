// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Notes and limitations:
// 1. This driver only uses PIO mode.
//
// 2. This driver only supports SDHCv3 and above. Lower versions of SD are not
//    currently supported. The driver should fail gracefully if a lower version
//    card is detected.

#include "sdhci.h"

#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/phys-iter.h>
#include <lib/zx/clock.h>
#include <lib/zx/pmt.h>
#include <lib/zx/time.h>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

#include "sdhci-reg.h"

namespace {

constexpr uint32_t kSdFreqSetupHz = 400'000;

constexpr int kMaxTuningCount = 40;

constexpr zx_paddr_t k32BitPhysAddrMask = 0xffff'ffff;

constexpr zx::duration kResetTime = zx::sec(1);
constexpr zx::duration kClockStabilizationTime = zx::msec(150);
constexpr zx::duration kVoltageStabilizationTime = zx::msec(5);
constexpr zx::duration kInhibitWaitTime = zx::msec(1);
constexpr zx::duration kWaitYieldTime = zx::usec(1);

constexpr uint32_t Hi32(zx_paddr_t val) { return static_cast<uint32_t>((val >> 32) & 0xffffffff); }
constexpr uint32_t Lo32(zx_paddr_t val) { return val & 0xffffffff; }

// for 2M max transfer size for fully discontiguous
// also see SDMMC_PAGES_COUNT in fuchsia/hardware/sdmmc/c/banjo.h
constexpr int kDmaDescCount = 512;

uint16_t GetClockDividerValue(const uint32_t base_clock, const uint32_t target_rate) {
  if (target_rate >= base_clock) {
    // A clock divider of 0 means "don't divide the clock"
    // If the base clock is already slow enough to use as the SD clock then
    // we don't need to divide it any further.
    return 0;
  }

  uint32_t result = base_clock / (2 * target_rate);
  if (result * target_rate * 2 < base_clock)
    result++;

  return std::min(sdhci::ClockControl::kMaxFrequencySelect, static_cast<uint16_t>(result));
}

}  // namespace

namespace sdhci {

void Sdhci::PrepareCmd(const sdmmc_req_t& req, TransferMode* transfer_mode, Command* command) {
  command->set_command_index(static_cast<uint16_t>(req.cmd_idx));

  if (req.cmd_flags & SDMMC_RESP_LEN_EMPTY) {
    command->set_response_type(Command::kResponseTypeNone);
  } else if (req.cmd_flags & SDMMC_RESP_LEN_136) {
    command->set_response_type(Command::kResponseType136Bits);
  } else if (req.cmd_flags & SDMMC_RESP_LEN_48) {
    command->set_response_type(Command::kResponseType48Bits);
  } else if (req.cmd_flags & SDMMC_RESP_LEN_48B) {
    command->set_response_type(Command::kResponseType48BitsWithBusy);
  }

  if (req.cmd_flags & SDMMC_CMD_TYPE_NORMAL) {
    command->set_command_type(Command::kCommandTypeNormal);
  } else if (req.cmd_flags & SDMMC_CMD_TYPE_SUSPEND) {
    command->set_command_type(Command::kCommandTypeSuspend);
  } else if (req.cmd_flags & SDMMC_CMD_TYPE_RESUME) {
    command->set_command_type(Command::kCommandTypeResume);
  } else if (req.cmd_flags & SDMMC_CMD_TYPE_ABORT) {
    command->set_command_type(Command::kCommandTypeAbort);
  }

  if (req.cmd_flags & SDMMC_CMD_AUTO12) {
    transfer_mode->set_auto_cmd_enable(TransferMode::kAutoCmd12);
  } else if (req.cmd_flags & SDMMC_CMD_AUTO23) {
    transfer_mode->set_auto_cmd_enable(TransferMode::kAutoCmd23);
  }

  if (req.cmd_flags & SDMMC_RESP_CRC_CHECK) {
    command->set_command_crc_check(1);
  }
  if (req.cmd_flags & SDMMC_RESP_CMD_IDX_CHECK) {
    command->set_command_index_check(1);
  }
  if (req.cmd_flags & SDMMC_RESP_DATA_PRESENT) {
    command->set_data_present(1);
  }
  if (req.cmd_flags & SDMMC_CMD_DMA_EN) {
    transfer_mode->set_dma_enable(1);
  }
  if (req.cmd_flags & SDMMC_CMD_BLKCNT_EN) {
    transfer_mode->set_block_count_enable(1);
  }
  if (req.cmd_flags & SDMMC_CMD_READ) {
    transfer_mode->set_read(1);
  }
  if (req.cmd_flags & SDMMC_CMD_MULTI_BLK) {
    transfer_mode->set_multi_block(1);
  }
}

zx_status_t Sdhci::WaitForReset(const SoftwareReset mask) {
  const zx::time deadline = zx::clock::get_monotonic() + kResetTime;
  do {
    if ((SoftwareReset::Get().ReadFrom(&regs_mmio_buffer_).reg_value() & mask.reg_value()) == 0) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  } while (zx::clock::get_monotonic() <= deadline);

  zxlogf(ERROR, "sdhci: timed out while waiting for reset");
  return ZX_ERR_TIMED_OUT;
}

void Sdhci::EnableInterrupts() {
  InterruptSignalEnable::Get()
      .FromValue(0)
      .EnableErrorInterrupts()
      .EnableNormalInterrupts()
      .set_card_interrupt(interrupt_cb_.is_valid() ? 1 : 0)
      .WriteTo(&regs_mmio_buffer_);
  InterruptStatusEnable::Get()
      .FromValue(0)
      .EnableErrorInterrupts()
      .EnableNormalInterrupts()
      .set_card_interrupt((interrupt_cb_.is_valid() && !card_interrupt_masked_) ? 1 : 0)
      .WriteTo(&regs_mmio_buffer_);
}

void Sdhci::DisableInterrupts() {
  InterruptSignalEnable::Get()
      .FromValue(0)
      .set_card_interrupt(interrupt_cb_.is_valid() ? 1 : 0)
      .WriteTo(&regs_mmio_buffer_);
  InterruptStatusEnable::Get()
      .FromValue(0)
      .set_card_interrupt((interrupt_cb_.is_valid() && !card_interrupt_masked_) ? 1 : 0)
      .WriteTo(&regs_mmio_buffer_);
}

zx_status_t Sdhci::WaitForInhibit(const PresentState mask) const {
  const zx::time deadline = zx::clock::get_monotonic() + kInhibitWaitTime;
  do {
    if ((PresentState::Get().ReadFrom(&regs_mmio_buffer_).reg_value() & mask.reg_value()) == 0) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  } while (zx::clock::get_monotonic() <= deadline);

  zxlogf(ERROR, "sdhci: timed out while waiting for command/data inhibit");
  return ZX_ERR_TIMED_OUT;
}

zx_status_t Sdhci::WaitForInternalClockStable() const {
  const zx::time deadline = zx::clock::get_monotonic() + kClockStabilizationTime;
  do {
    if ((ClockControl::Get().ReadFrom(&regs_mmio_buffer_).internal_clock_stable())) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  } while (zx::clock::get_monotonic() <= deadline);

  zxlogf(ERROR, "sdhci: timed out while waiting for internal clock to stabilize");
  return ZX_ERR_TIMED_OUT;
}

bool Sdhci::CmdStageComplete() {
  const uint32_t response_0 = Response::Get(0).ReadFrom(&regs_mmio_buffer_).reg_value();
  const uint32_t response_1 = Response::Get(1).ReadFrom(&regs_mmio_buffer_).reg_value();
  const uint32_t response_2 = Response::Get(2).ReadFrom(&regs_mmio_buffer_).reg_value();
  const uint32_t response_3 = Response::Get(3).ReadFrom(&regs_mmio_buffer_).reg_value();

  // Read the response data.
  if (pending_request_.cmd_flags & SDMMC_RESP_LEN_136) {
    if (quirks_ & SDHCI_QUIRK_STRIP_RESPONSE_CRC) {
      pending_request_.response[0] = (response_3 << 8) | ((response_2 >> 24) & 0xFF);
      pending_request_.response[1] = (response_2 << 8) | ((response_1 >> 24) & 0xFF);
      pending_request_.response[2] = (response_1 << 8) | ((response_0 >> 24) & 0xFF);
      pending_request_.response[3] = (response_0 << 8);
    } else if (quirks_ & SDHCI_QUIRK_STRIP_RESPONSE_CRC_PRESERVE_ORDER) {
      pending_request_.response[0] = (response_0 << 8);
      pending_request_.response[1] = (response_1 << 8) | ((response_0 >> 24) & 0xFF);
      pending_request_.response[2] = (response_2 << 8) | ((response_1 >> 24) & 0xFF);
      pending_request_.response[3] = (response_3 << 8) | ((response_2 >> 24) & 0xFF);
    } else {
      pending_request_.response[0] = response_0;
      pending_request_.response[1] = response_1;
      pending_request_.response[2] = response_2;
      pending_request_.response[3] = response_3;
    }
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

bool Sdhci::TransferComplete() {
  pending_request_.data_done = true;
  if (pending_request_.cmd_done) {
    CompleteRequest();
  }

  return pending_request_.cmd_done;
}

bool Sdhci::DataStageReadReady() {
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

void Sdhci::ErrorRecovery() {
  // Reset internal state machines
  {
    SoftwareReset::Get().ReadFrom(&regs_mmio_buffer_).set_reset_cmd(1).WriteTo(&regs_mmio_buffer_);
    [[maybe_unused]] auto _ = WaitForReset(SoftwareReset::Get().FromValue(0).set_reset_cmd(1));
  }
  {
    SoftwareReset::Get().ReadFrom(&regs_mmio_buffer_).set_reset_dat(1).WriteTo(&regs_mmio_buffer_);
    [[maybe_unused]] auto _ = WaitForReset(SoftwareReset::Get().FromValue(0).set_reset_dat(1));
  }

  // Complete any pending txn with error status
  CompleteRequest();
}

void Sdhci::CompleteRequest() {
  DisableInterrupts();
  sync_completion_signal(&req_completion_);
}

int Sdhci::IrqThread() {
  while (true) {
    zx_status_t wait_res = WaitForInterrupt();
    if (wait_res != ZX_OK) {
      if (wait_res != ZX_ERR_CANCELED) {
        zxlogf(ERROR, "sdhci: interrupt wait failed with retcode = %d", wait_res);
      }
      break;
    }

    // Acknowledge the IRQs that we stashed. IRQs are cleared by writing
    // 1s into the IRQs that fired.
    auto status = InterruptStatus::Get().ReadFrom(&regs_mmio_buffer_).WriteTo(&regs_mmio_buffer_);

    zxlogf(DEBUG, "got irq 0x%08x en 0x%08x", status.reg_value(),
           InterruptSignalEnable::Get().ReadFrom(&regs_mmio_buffer_).reg_value());

    fbl::AutoLock lock(&mtx_);
    if (pending_request_.is_pending()) {
      HandleTransferInterrupt(status);
    }

    if (status.card_interrupt()) {
      // Disable the card interrupt and call the callback if there is one.
      InterruptStatusEnable::Get()
          .ReadFrom(&regs_mmio_buffer_)
          .set_card_interrupt(0)
          .WriteTo(&regs_mmio_buffer_);
      card_interrupt_masked_ = true;
      if (interrupt_cb_.is_valid()) {
        interrupt_cb_.Callback();
      }
    }
  }
  return thrd_success;
}

void Sdhci::HandleTransferInterrupt(const InterruptStatus status) {
  if (status.ErrorInterrupt()) {
    pending_request_.status = status;
    pending_request_.status.set_error(1);
    ErrorRecovery();
    return;
  }

  // Clear the interrupt status to indicate that a normal interrupt was handled.
  pending_request_.status = InterruptStatus::Get().FromValue(0);
  if (status.buffer_read_ready() && DataStageReadReady()) {
    return;
  }
  if (status.command_complete() && CmdStageComplete()) {
    return;
  }
  if (status.transfer_complete()) {
    TransferComplete();
  }
}

zx_status_t Sdhci::SdmmcRegisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo vmo,
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

zx_status_t Sdhci::SdmmcUnregisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo* out_vmo) {
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

zx_status_t Sdhci::SdmmcRequest(const sdmmc_req_t* req, uint32_t out_response[4]) {
  if (req->client_id >= std::size(registered_vmo_stores_)) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (!SupportsAdma2()) {
    // TODO(fxbug.dev/106851): Add support for PIO requests.
    return ZX_ERR_NOT_SUPPORTED;
  }

  DmaDescriptorBuilder<OwnedVmoInfo> builder(*req, registered_vmo_stores_[req->client_id],
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

zx_status_t Sdhci::StartRequest(const sdmmc_req_t& request,
                                DmaDescriptorBuilder<OwnedVmoInfo>& builder) {
  using BlockSizeType = decltype(BlockSize::Get().FromValue(0).reg_value());
  using BlockCountType = decltype(BlockCount::Get().FromValue(0).reg_value());

  // Every command requires that the Command Inhibit is unset.
  auto inhibit_mask = PresentState::Get().FromValue(0).set_command_inhibit_cmd(1);

  // Busy type commands must also wait for the DATA Inhibit to be 0 UNLESS
  // it's an abort command which can be issued with the data lines active.
  if ((request.cmd_flags & SDMMC_RESP_LEN_48B) && (request.cmd_flags & SDMMC_CMD_TYPE_ABORT)) {
    inhibit_mask.set_command_inhibit_dat(1);
  }

  // Wait for the inhibit masks from above to become 0 before issuing the command.
  zx_status_t status = WaitForInhibit(inhibit_mask);
  if (status != ZX_OK) {
    return status;
  }

  TransferMode transfer_mode = TransferMode::Get().FromValue(0);

  const bool is_tuning_request =
      request.cmd_idx == MMC_SEND_TUNING_BLOCK || request.cmd_idx == SD_SEND_TUNING_BLOCK;

  const auto blocksize = static_cast<BlockSizeType>(request.blocksize);

  if (is_tuning_request) {
    // The SDHCI controller has special logic to handle tuning transfers, so there is no need to set
    // up any DMA buffers.
    BlockSize::Get().FromValue(blocksize).WriteTo(&regs_mmio_buffer_);
    BlockCount::Get().FromValue(0).WriteTo(&regs_mmio_buffer_);
  } else if (request.cmd_flags & SDMMC_RESP_DATA_PRESENT) {
    if (request.blocksize > std::numeric_limits<BlockSizeType>::max()) {
      return ZX_ERR_OUT_OF_RANGE;
    }
    if (request.blocksize == 0) {
      return ZX_ERR_INVALID_ARGS;
    }

    if (const zx_status_t status = SetUpDma(request, builder); status != ZX_OK) {
      return status;
    }

    if (builder.block_count() > std::numeric_limits<BlockCountType>::max()) {
      zxlogf(ERROR, "Block count (%lu) exceeds the maximum (%u)", builder.block_count(),
             std::numeric_limits<BlockCountType>::max());
      return ZX_ERR_OUT_OF_RANGE;
    }

    transfer_mode.set_dma_enable(1).set_multi_block(builder.block_count() > 1 ? 1 : 0);

    const auto blockcount = static_cast<BlockCountType>(builder.block_count());

    BlockSize::Get().FromValue(blocksize).WriteTo(&regs_mmio_buffer_);
    BlockCount::Get().FromValue(blockcount).WriteTo(&regs_mmio_buffer_);
  } else {
    BlockSize::Get().FromValue(0).WriteTo(&regs_mmio_buffer_);
    BlockCount::Get().FromValue(0).WriteTo(&regs_mmio_buffer_);
  }

  Command command = Command::Get().FromValue(0);
  PrepareCmd(request, &transfer_mode, &command);

  Argument::Get().FromValue(request.arg).WriteTo(&regs_mmio_buffer_);

  // Clear any pending interrupts before starting the transaction.
  auto irq_mask = InterruptSignalEnable::Get().ReadFrom(&regs_mmio_buffer_);
  InterruptStatus::Get().FromValue(irq_mask.reg_value()).WriteTo(&regs_mmio_buffer_);

  pending_request_.Init(request);

  // Unmask and enable interrupts
  EnableInterrupts();

  // Start command
  transfer_mode.WriteTo(&regs_mmio_buffer_);
  command.WriteTo(&regs_mmio_buffer_);

  return ZX_OK;
}

zx_status_t Sdhci::SetUpDma(const sdmmc_req_t& request,
                            DmaDescriptorBuilder<OwnedVmoInfo>& builder) {
  const cpp20::span buffers{request.buffers_list, request.buffers_count};
  zx_status_t status;
  for (const auto& buffer : buffers) {
    if ((status = builder.ProcessBuffer(buffer)) != ZX_OK) {
      return status;
    }
  }

  size_t descriptor_size;
  if (Capabilities0::Get().ReadFrom(&regs_mmio_buffer_).v3_64_bit_system_address_support()) {
    const cpp20::span descriptors{reinterpret_cast<AdmaDescriptor96*>(iobuf_.virt()),
                                  kDmaDescCount};
    descriptor_size = sizeof(descriptors[0]);
    status = builder.BuildDmaDescriptors(descriptors);
  } else {
    const cpp20::span descriptors{reinterpret_cast<AdmaDescriptor64*>(iobuf_.virt()),
                                  kDmaDescCount};
    descriptor_size = sizeof(descriptors[0]);
    status = builder.BuildDmaDescriptors(descriptors);
  }

  if (status != ZX_OK) {
    return status;
  }

  status = iobuf_.CacheOp(ZX_VMO_OP_CACHE_CLEAN, 0, builder.descriptor_count() * descriptor_size);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to clean cache: %s", zx_status_get_string(status));
    return status;
  }

  AdmaSystemAddress::Get(0).FromValue(Lo32(iobuf_.phys())).WriteTo(&regs_mmio_buffer_);
  AdmaSystemAddress::Get(1).FromValue(Hi32(iobuf_.phys())).WriteTo(&regs_mmio_buffer_);
  return ZX_OK;
}

zx_status_t Sdhci::FinishRequest(const sdmmc_req_t& request, uint32_t out_response[4]) {
  if (pending_request_.cmd_done) {
    memcpy(out_response, pending_request_.response, sizeof(uint32_t) * 4);
  }

  if (request.cmd_flags & SDMMC_CMD_TYPE_ABORT) {
    // SDHCI spec section 3.8.2: reset the data line after an abort to discard data in the buffer.
    [[maybe_unused]] auto _ =
        WaitForReset(SoftwareReset::Get().FromValue(0).set_reset_cmd(1).set_reset_dat(1));
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

  if (!interrupt_status.error()) {
    return ZX_OK;
  }

  if (interrupt_status.tuning_error()) {
    zxlogf(ERROR, "Tuning error");
  }
  if (interrupt_status.adma_error()) {
    zxlogf(ERROR, "ADMA error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.auto_cmd_error()) {
    zxlogf(ERROR, "Auto cmd error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.current_limit_error()) {
    zxlogf(ERROR, "Current limit error cmd%u", request.cmd_idx);
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
  if (interrupt_status.command_index_error()) {
    zxlogf(ERROR, "Command index error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.command_end_bit_error()) {
    zxlogf(ERROR, "Command end bit error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.command_crc_error()) {
    if (request.suppress_error_messages) {
      zxlogf(DEBUG, "Command CRC error cmd%u", request.cmd_idx);
    } else {
      zxlogf(ERROR, "Command CRC error cmd%u", request.cmd_idx);
    }
  }
  if (interrupt_status.command_timeout_error()) {
    if (request.suppress_error_messages) {
      zxlogf(DEBUG, "Command timeout error cmd%u", request.cmd_idx);
    } else {
      zxlogf(ERROR, "Command timeout error cmd%u", request.cmd_idx);
    }
  }
  if (interrupt_status.reg_value() ==
      InterruptStatusEnable::Get().FromValue(0).set_error(1).reg_value()) {
    // Log an unknown error only if no other bits were set.
    zxlogf(ERROR, "Unknown error cmd%u", request.cmd_idx);
  }

  return ZX_ERR_IO;
}

zx_status_t Sdhci::SdmmcHostInfo(sdmmc_host_info_t* out_info) {
  memcpy(out_info, &info_, sizeof(info_));
  return ZX_OK;
}

zx_status_t Sdhci::SdmmcSetSignalVoltage(sdmmc_voltage_t voltage) {
  fbl::AutoLock lock(&mtx_);

  // Validate the controller supports the requested voltage
  if ((voltage == SDMMC_VOLTAGE_V330) && !(info_.caps & SDMMC_HOST_CAP_VOLTAGE_330)) {
    zxlogf(DEBUG, "sdhci: 3.3V signal voltage not supported");
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto ctrl2 = HostControl2::Get().ReadFrom(&regs_mmio_buffer_);
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
      zxlogf(ERROR, "sdhci: unknown signal voltage value %u", voltage);
      return ZX_ERR_INVALID_ARGS;
  }

  // Note: the SDHCI spec indicates that the data lines should be checked to see if the card is
  // ready for a voltage switch, however that doesn't seem to work for one of our devices.

  ctrl2.set_voltage_1v8_signalling_enable(voltage_1v8_value).WriteTo(&regs_mmio_buffer_);

  // Wait 5ms for the regulator to stabilize.
  zx::nanosleep(zx::deadline_after(kVoltageStabilizationTime));

  if (ctrl2.ReadFrom(&regs_mmio_buffer_).voltage_1v8_signalling_enable() != voltage_1v8_value) {
    zxlogf(ERROR, "sdhci: voltage regulator output did not become stable");
    // Cut power to the card if the voltage switch failed.
    PowerControl::Get()
        .ReadFrom(&regs_mmio_buffer_)
        .set_sd_bus_power_vdd1(0)
        .WriteTo(&regs_mmio_buffer_);
    return ZX_ERR_INTERNAL;
  }

  zxlogf(DEBUG, "sdhci: switch signal voltage to %d", voltage);

  return ZX_OK;
}

zx_status_t Sdhci::SdmmcSetBusWidth(sdmmc_bus_width_t bus_width) {
  fbl::AutoLock lock(&mtx_);

  if ((bus_width == SDMMC_BUS_WIDTH_EIGHT) && !(info_.caps & SDMMC_HOST_CAP_BUS_WIDTH_8)) {
    zxlogf(DEBUG, "sdhci: 8-bit bus width not supported");
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto ctrl1 = HostControl1::Get().ReadFrom(&regs_mmio_buffer_);

  switch (bus_width) {
    case SDMMC_BUS_WIDTH_ONE:
      ctrl1.set_extended_data_transfer_width(0).set_data_transfer_width_4bit(0);
      break;
    case SDMMC_BUS_WIDTH_FOUR:
      ctrl1.set_extended_data_transfer_width(0).set_data_transfer_width_4bit(1);
      break;
    case SDMMC_BUS_WIDTH_EIGHT:
      ctrl1.set_extended_data_transfer_width(1).set_data_transfer_width_4bit(0);
      break;
    default:
      zxlogf(ERROR, "sdhci: unknown bus width value %u", bus_width);
      return ZX_ERR_INVALID_ARGS;
  }

  ctrl1.WriteTo(&regs_mmio_buffer_);

  zxlogf(DEBUG, "sdhci: set bus width to %d", bus_width);

  return ZX_OK;
}

zx_status_t Sdhci::SdmmcSetBusFreq(uint32_t bus_freq) {
  fbl::AutoLock lock(&mtx_);

  zx_status_t st = WaitForInhibit(
      PresentState::Get().FromValue(0).set_command_inhibit_cmd(1).set_command_inhibit_dat(1));
  if (st != ZX_OK) {
    return st;
  }

  // Turn off the SD clock before messing with the clock rate.
  auto clock = ClockControl::Get().ReadFrom(&regs_mmio_buffer_).set_sd_clock_enable(0);
  if (bus_freq == 0) {
    clock.WriteTo(&regs_mmio_buffer_);
    return ZX_OK;
  }
  clock.set_internal_clock_enable(0).WriteTo(&regs_mmio_buffer_);

  // Write the new divider into the control register.
  clock.set_frequency_select(GetClockDividerValue(base_clock_, bus_freq))
      .set_internal_clock_enable(1)
      .WriteTo(&regs_mmio_buffer_);

  if ((st = WaitForInternalClockStable()) != ZX_OK) {
    return st;
  }

  // Turn the SD clock back on.
  clock.set_sd_clock_enable(1).WriteTo(&regs_mmio_buffer_);

  zxlogf(DEBUG, "sdhci: set bus frequency to %u", bus_freq);

  return ZX_OK;
}

zx_status_t Sdhci::SdmmcSetTiming(sdmmc_timing_t timing) {
  fbl::AutoLock lock(&mtx_);

  auto ctrl1 = HostControl1::Get().ReadFrom(&regs_mmio_buffer_);

  // Toggle high-speed
  if (timing != SDMMC_TIMING_LEGACY) {
    ctrl1.set_high_speed_enable(1).WriteTo(&regs_mmio_buffer_);
  } else {
    ctrl1.set_high_speed_enable(0).WriteTo(&regs_mmio_buffer_);
  }

  auto ctrl2 = HostControl2::Get().ReadFrom(&regs_mmio_buffer_);
  switch (timing) {
    case SDMMC_TIMING_LEGACY:
    case SDMMC_TIMING_SDR12:
      ctrl2.set_uhs_mode_select(HostControl2::kUhsModeSdr12);
      break;
    case SDMMC_TIMING_HS:
    case SDMMC_TIMING_SDR25:
      ctrl2.set_uhs_mode_select(HostControl2::kUhsModeSdr25);
      break;
    case SDMMC_TIMING_HSDDR:
    case SDMMC_TIMING_DDR50:
      ctrl2.set_uhs_mode_select(HostControl2::kUhsModeDdr50);
      break;
    case SDMMC_TIMING_HS200:
    case SDMMC_TIMING_SDR104:
      ctrl2.set_uhs_mode_select(HostControl2::kUhsModeSdr104);
      break;
    case SDMMC_TIMING_HS400:
      ctrl2.set_uhs_mode_select(HostControl2::kUhsModeHs400);
      break;
    case SDMMC_TIMING_SDR50:
      ctrl2.set_uhs_mode_select(HostControl2::kUhsModeSdr50);
      break;
    default:
      zxlogf(ERROR, "sdhci: unknown timing value %u", timing);
      return ZX_ERR_INVALID_ARGS;
  }
  ctrl2.WriteTo(&regs_mmio_buffer_);

  zxlogf(DEBUG, "sdhci: set bus timing to %d", timing);

  return ZX_OK;
}

zx_status_t Sdhci::SdmmcHwReset() {
  fbl::AutoLock lock(&mtx_);
  sdhci_.HwReset();
  return ZX_OK;
}

zx_status_t Sdhci::SdmmcPerformTuning(uint32_t cmd_idx) {
  zxlogf(DEBUG, "sdhci: perform tuning");

  uint16_t blocksize;
  auto ctrl2 = HostControl2::Get().FromValue(0);

  {
    fbl::AutoLock lock(&mtx_);
    blocksize = static_cast<uint16_t>(
        HostControl1::Get().ReadFrom(&regs_mmio_buffer_).extended_data_transfer_width() ? 128 : 64);
    ctrl2.ReadFrom(&regs_mmio_buffer_).set_execute_tuning(1).WriteTo(&regs_mmio_buffer_);
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

  for (int count = 0; (count < kMaxTuningCount) && ctrl2.execute_tuning(); count++) {
    zx_status_t st = SdmmcRequest(&req, unused_response);
    if (st != ZX_OK) {
      zxlogf(ERROR, "sdhci: MMC_SEND_TUNING_BLOCK error, retcode = %d", st);
      return st;
    }

    fbl::AutoLock lock(&mtx_);
    ctrl2.ReadFrom(&regs_mmio_buffer_);
  }

  {
    fbl::AutoLock lock(&mtx_);
    ctrl2.ReadFrom(&regs_mmio_buffer_);
  }

  const bool fail = ctrl2.execute_tuning() || !ctrl2.use_tuned_clock();

  zxlogf(DEBUG, "sdhci: tuning fail %d", fail);

  return fail ? ZX_ERR_IO : ZX_OK;
}

zx_status_t Sdhci::SdmmcRegisterInBandInterrupt(const in_band_interrupt_protocol_t* interrupt_cb) {
  fbl::AutoLock lock(&mtx_);

  interrupt_cb_ = ddk::InBandInterruptProtocolClient(interrupt_cb);

  InterruptSignalEnable::Get()
      .ReadFrom(&regs_mmio_buffer_)
      .set_card_interrupt(1)
      .WriteTo(&regs_mmio_buffer_);
  InterruptStatusEnable::Get()
      .ReadFrom(&regs_mmio_buffer_)
      .set_card_interrupt(card_interrupt_masked_ ? 0 : 1)
      .WriteTo(&regs_mmio_buffer_);

  // Call the callback if an interrupt was raised before it was registered.
  if (card_interrupt_masked_) {
    interrupt_cb_.Callback();
  }

  return ZX_OK;
}

void Sdhci::SdmmcAckInBandInterrupt() {
  fbl::AutoLock lock(&mtx_);
  InterruptStatusEnable::Get()
      .ReadFrom(&regs_mmio_buffer_)
      .set_card_interrupt(1)
      .WriteTo(&regs_mmio_buffer_);
  card_interrupt_masked_ = false;
}

void Sdhci::DdkUnbind(ddk::UnbindTxn txn) {
  // stop irq thread
  irq_.destroy();
  thrd_join(irq_thread_, nullptr);

  txn.Reply();
}

void Sdhci::DdkRelease() { delete this; }

zx_status_t Sdhci::Init() {
  // Perform a software reset against both the DAT and CMD interface.
  SoftwareReset::Get().ReadFrom(&regs_mmio_buffer_).set_reset_all(1).WriteTo(&regs_mmio_buffer_);

  // Disable both clocks.
  auto clock = ClockControl::Get().ReadFrom(&regs_mmio_buffer_);
  clock.set_internal_clock_enable(0).set_sd_clock_enable(0).WriteTo(&regs_mmio_buffer_);

  // Wait for reset to take place. The reset is completed when all three
  // of the following flags are reset.
  const SoftwareReset target_mask =
      SoftwareReset::Get().FromValue(0).set_reset_all(1).set_reset_cmd(1).set_reset_dat(1);
  zx_status_t status = ZX_OK;
  if ((status = WaitForReset(target_mask)) != ZX_OK) {
    return status;
  }

  // The core has been reset, which should have stopped any DMAs that were happening when the driver
  // started. It is now safe to release quarantined pages.
  if ((status = bti_.release_quarantine()) != ZX_OK) {
    zxlogf(ERROR, "Failed to release quarantined pages: %d", status);
    return status;
  }

  // Ensure that we're SDv3.
  const uint16_t vrsn =
      HostControllerVersion::Get().ReadFrom(&regs_mmio_buffer_).specification_version();
  if (vrsn < HostControllerVersion::kSpecificationVersion300) {
    zxlogf(ERROR, "sdhci: SD version is %u, only version %u is supported", vrsn,
           HostControllerVersion::kSpecificationVersion300);
    return ZX_ERR_NOT_SUPPORTED;
  }
  zxlogf(DEBUG, "sdhci: controller version %d", vrsn);

  auto caps0 = Capabilities0::Get().ReadFrom(&regs_mmio_buffer_);
  auto caps1 = Capabilities1::Get().ReadFrom(&regs_mmio_buffer_);

  base_clock_ = caps0.base_clock_frequency_hz();
  if (base_clock_ == 0) {
    // try to get controller specific base clock
    base_clock_ = sdhci_.GetBaseClock();
  }
  if (base_clock_ == 0) {
    zxlogf(ERROR, "sdhci: base clock is 0!");
    return ZX_ERR_INTERNAL;
  }

  // Get controller capabilities
  if (caps0.bus_width_8_support()) {
    info_.caps |= SDMMC_HOST_CAP_BUS_WIDTH_8;
  }
  if (caps0.adma2_support() && !(quirks_ & SDHCI_QUIRK_NO_DMA)) {
    info_.caps |= SDMMC_HOST_CAP_DMA;
  }
  if (caps0.voltage_3v3_support()) {
    info_.caps |= SDMMC_HOST_CAP_VOLTAGE_330;
  }
  if (caps1.sdr50_support()) {
    info_.caps |= SDMMC_HOST_CAP_SDR50;
  }
  if (caps1.ddr50_support() && !(quirks_ & SDHCI_QUIRK_NO_DDR)) {
    info_.caps |= SDMMC_HOST_CAP_DDR50;
  }
  if (caps1.sdr104_support()) {
    info_.caps |= SDMMC_HOST_CAP_SDR104;
  }
  if (!caps1.use_tuning_for_sdr50()) {
    info_.caps |= SDMMC_HOST_CAP_NO_TUNING_SDR50;
  }
  info_.caps |= SDMMC_HOST_CAP_AUTO_CMD12;

  // Set controller preferences
  if (quirks_ & SDHCI_QUIRK_NON_STANDARD_TUNING) {
    // Disable HS200 and HS400 if tuning cannot be performed as per the spec.
    info_.prefs |= SDMMC_HOST_PREFS_DISABLE_HS200 | SDMMC_HOST_PREFS_DISABLE_HS400;
  }
  if (quirks_ & SDHCI_QUIRK_NO_DDR) {
    info_.prefs |= SDMMC_HOST_PREFS_DISABLE_HSDDR | SDMMC_HOST_PREFS_DISABLE_HS400;
  }

  // allocate and setup DMA descriptor
  if (SupportsAdma2()) {
    auto host_control1 = HostControl1::Get().ReadFrom(&regs_mmio_buffer_);
    if (caps0.v3_64_bit_system_address_support()) {
      status = iobuf_.Init(bti_.get(), kDmaDescCount * sizeof(AdmaDescriptor96),
                           IO_BUFFER_RW | IO_BUFFER_CONTIG);
      host_control1.set_dma_select(HostControl1::kDmaSelect64BitAdma2);
    } else {
      status = iobuf_.Init(bti_.get(), kDmaDescCount * sizeof(AdmaDescriptor64),
                           IO_BUFFER_RW | IO_BUFFER_CONTIG);
      host_control1.set_dma_select(HostControl1::kDmaSelect32BitAdma2);

      if ((iobuf_.phys() & k32BitPhysAddrMask) != iobuf_.phys()) {
        zxlogf(ERROR, "Got 64-bit physical address, only 32-bit DMA is supported");
        return ZX_ERR_NOT_SUPPORTED;
      }
    }

    if (status != ZX_OK) {
      zxlogf(ERROR, "sdhci: error allocating DMA descriptors");
      return status;
    }
    info_.max_transfer_size = kDmaDescCount * zx_system_get_page_size();

    host_control1.WriteTo(&regs_mmio_buffer_);
  } else {
    // no maximum if only PIO supported
    info_.max_transfer_size = fuchsia_hardware_block::wire::kMaxTransferUnbounded;
  }
  info_.max_transfer_size_non_dma = fuchsia_hardware_block::wire::kMaxTransferUnbounded;

  // Configure the clock.
  clock.ReadFrom(&regs_mmio_buffer_).set_internal_clock_enable(1);

  // SDHCI Versions 1.00 and 2.00 handle the clock divider slightly
  // differently compared to SDHCI version 3.00. Since this driver doesn't
  // support SDHCI versions < 3.00, we ignore this incongruency for now.
  //
  // V3.00 supports a 10 bit divider where the SD clock frequency is defined
  // as F/(2*D) where F is the base clock frequency and D is the divider.
  clock.set_frequency_select(GetClockDividerValue(base_clock_, kSdFreqSetupHz))
      .WriteTo(&regs_mmio_buffer_);

  // Wait for the clock to stabilize.
  status = WaitForInternalClockStable();
  if (status != ZX_OK) {
    return ZX_ERR_TIMED_OUT;
  }

  // Set the command timeout.
  TimeoutControl::Get()
      .ReadFrom(&regs_mmio_buffer_)
      .set_data_timeout_counter(TimeoutControl::kDataTimeoutMax)
      .WriteTo(&regs_mmio_buffer_);

  // Set SD bus voltage to maximum supported by the host controller
  auto power = PowerControl::Get().ReadFrom(&regs_mmio_buffer_).set_sd_bus_power_vdd1(1);
  if (info_.caps & SDMMC_HOST_CAP_VOLTAGE_330) {
    power.set_sd_bus_voltage_vdd1(PowerControl::kBusVoltage3V3);
  } else {
    power.set_sd_bus_voltage_vdd1(PowerControl::kBusVoltage1V8);
  }
  power.WriteTo(&regs_mmio_buffer_);

  // Enable the SD clock.
  clock.ReadFrom(&regs_mmio_buffer_).set_sd_clock_enable(1).WriteTo(&regs_mmio_buffer_);

  // Disable all interrupts
  {
    fbl::AutoLock lock(&mtx_);
    DisableInterrupts();
  }

  if (thrd_create_with_name(
          &irq_thread_, [](void* arg) -> int { return reinterpret_cast<Sdhci*>(arg)->IrqThread(); },
          this, "sdhci_irq_thread") != thrd_success) {
    zxlogf(ERROR, "sdhci: failed to create irq thread");
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t Sdhci::Create(void* ctx, zx_device_t* parent) {
  ddk::SdhciProtocolClient sdhci(parent);
  if (!sdhci.is_valid()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Map the Device Registers so that we can perform MMIO against the device.
  zx::vmo vmo;
  zx_off_t vmo_offset = 0;
  zx_status_t status = sdhci.GetMmio(&vmo, &vmo_offset);
  if (status != ZX_OK) {
    zxlogf(ERROR, "sdhci: error %d in get_mmio", status);
    return status;
  }
  std::optional<fdf::MmioBuffer> regs_mmio_buffer;
  status = fdf::MmioBuffer::Create(vmo_offset, kRegisterSetSize, std::move(vmo),
                                   ZX_CACHE_POLICY_UNCACHED_DEVICE, &regs_mmio_buffer);
  if (status != ZX_OK) {
    zxlogf(ERROR, "sdhci: error %d in mmio_buffer_init", status);
    return status;
  }
  zx::bti bti;
  status = sdhci.GetBti(0, &bti);
  if (status != ZX_OK) {
    zxlogf(ERROR, "sdhci: error %d in get_bti", status);
    return status;
  }

  zx::interrupt irq;
  status = sdhci.GetInterrupt(&irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "sdhci: error %d in get_interrupt", status);
    return status;
  }

  uint64_t dma_boundary_alignment = 0;
  uint64_t quirks = sdhci.GetQuirks(&dma_boundary_alignment);

  if (!(quirks & SDHCI_QUIRK_USE_DMA_BOUNDARY_ALIGNMENT)) {
    dma_boundary_alignment = 0;
  } else if (dma_boundary_alignment == 0) {
    zxlogf(ERROR, "sdhci: DMA boundary alignment is zero");
    return ZX_ERR_OUT_OF_RANGE;
  }

  fbl::AllocChecker ac;
  auto dev =
      fbl::make_unique_checked<Sdhci>(&ac, parent, *std::move(regs_mmio_buffer), std::move(bti),
                                      std::move(irq), sdhci, quirks, dma_boundary_alignment);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  // initialize the controller
  status = dev->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: SDHCI Controller init failed", __func__);
    return status;
  }

  status = dev->DdkAdd(ddk::DeviceAddArgs("sdhci").forward_metadata(parent, DEVICE_METADATA_SDMMC));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: SDMMC device_add failed.", __func__);
    dev->irq_.destroy();
    thrd_join(dev->irq_thread_, nullptr);
    return status;
  }

  [[maybe_unused]] auto _ = dev.release();
  return ZX_OK;
}

}  // namespace sdhci

static constexpr zx_driver_ops_t sdhci_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = sdhci::Sdhci::Create;
  return ops;
}();

ZIRCON_DRIVER(sdhci, sdhci_driver_ops, "zircon", "0.1");
