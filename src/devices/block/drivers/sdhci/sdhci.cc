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
#include <fidl/fuchsia.hardware.sdmmc/cpp/wire.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/zx/clock.h>
#include <lib/zx/pmt.h>
#include <lib/zx/time.h>
#include <threads.h>

#include <bind/fuchsia/hardware/sdmmc/cpp/bind.h>
#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

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

  // SDHCI Versions 1.00 and 2.00 handle the clock divider slightly
  // differently compared to SDHCI version 3.00. Since this driver doesn't
  // support SDHCI versions < 3.00, we ignore this incongruency for now.
  //
  // V3.00 supports a 10 bit divider where the SD clock frequency is defined
  // as F/(2*D) where F is the base clock frequency and D is the divider.
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
  if (req.cmd_flags & SDMMC_CMD_READ) {
    transfer_mode->set_read(1);
  }
}

zx_status_t Sdhci::WaitForReset(const SoftwareReset mask) {
  const zx::time deadline = zx::clock::get_monotonic() + kResetTime;
  do {
    if ((SoftwareReset::Get().ReadFrom(&*regs_mmio_buffer_).reg_value() & mask.reg_value()) == 0) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  } while (zx::clock::get_monotonic() <= deadline);

  FDF_LOG(ERROR, "sdhci: timed out while waiting for reset");
  return ZX_ERR_TIMED_OUT;
}

void Sdhci::EnableInterrupts() {
  InterruptSignalEnable::Get()
      .FromValue(0)
      .EnableErrorInterrupts()
      .EnableNormalInterrupts()
      .set_card_interrupt(interrupt_cb_.is_valid() ? 1 : 0)
      .WriteTo(&*regs_mmio_buffer_);
  InterruptStatusEnable::Get()
      .FromValue(0)
      .EnableErrorInterrupts()
      .EnableNormalInterrupts()
      .set_card_interrupt((interrupt_cb_.is_valid() && !card_interrupt_masked_) ? 1 : 0)
      .WriteTo(&*regs_mmio_buffer_);
}

void Sdhci::DisableInterrupts() {
  InterruptSignalEnable::Get()
      .FromValue(0)
      .set_card_interrupt(interrupt_cb_.is_valid() ? 1 : 0)
      .WriteTo(&*regs_mmio_buffer_);
  InterruptStatusEnable::Get()
      .FromValue(0)
      .set_card_interrupt((interrupt_cb_.is_valid() && !card_interrupt_masked_) ? 1 : 0)
      .WriteTo(&*regs_mmio_buffer_);
}

zx_status_t Sdhci::WaitForInhibit(const PresentState mask) const {
  const zx::time deadline = zx::clock::get_monotonic() + kInhibitWaitTime;
  do {
    if ((PresentState::Get().ReadFrom(&*regs_mmio_buffer_).reg_value() & mask.reg_value()) == 0) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  } while (zx::clock::get_monotonic() <= deadline);

  FDF_LOG(ERROR, "sdhci: timed out while waiting for command/data inhibit");
  return ZX_ERR_TIMED_OUT;
}

zx_status_t Sdhci::WaitForInternalClockStable() const {
  const zx::time deadline = zx::clock::get_monotonic() + kClockStabilizationTime;
  do {
    if ((ClockControl::Get().ReadFrom(&*regs_mmio_buffer_).internal_clock_stable())) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(kWaitYieldTime));
  } while (zx::clock::get_monotonic() <= deadline);

  FDF_LOG(ERROR, "sdhci: timed out while waiting for internal clock to stabilize");
  return ZX_ERR_TIMED_OUT;
}

bool Sdhci::CmdStageComplete() {
  const uint32_t response_0 = Response::Get(0).ReadFrom(&*regs_mmio_buffer_).reg_value();
  const uint32_t response_1 = Response::Get(1).ReadFrom(&*regs_mmio_buffer_).reg_value();
  const uint32_t response_2 = Response::Get(2).ReadFrom(&*regs_mmio_buffer_).reg_value();
  const uint32_t response_3 = Response::Get(3).ReadFrom(&*regs_mmio_buffer_).reg_value();

  // Read the response data.
  if (pending_request_->cmd_flags & SDMMC_RESP_LEN_136) {
    if (quirks_ & fuchsia_hardware_sdhci::Quirk::kStripResponseCrc) {
      pending_request_->response[0] = (response_3 << 8) | ((response_2 >> 24) & 0xFF);
      pending_request_->response[1] = (response_2 << 8) | ((response_1 >> 24) & 0xFF);
      pending_request_->response[2] = (response_1 << 8) | ((response_0 >> 24) & 0xFF);
      pending_request_->response[3] = (response_0 << 8);
    } else if (quirks_ & fuchsia_hardware_sdhci::Quirk::kStripResponseCrcPreserveOrder) {
      pending_request_->response[0] = (response_0 << 8);
      pending_request_->response[1] = (response_1 << 8) | ((response_0 >> 24) & 0xFF);
      pending_request_->response[2] = (response_2 << 8) | ((response_1 >> 24) & 0xFF);
      pending_request_->response[3] = (response_3 << 8) | ((response_2 >> 24) & 0xFF);
    } else {
      pending_request_->response[0] = response_0;
      pending_request_->response[1] = response_1;
      pending_request_->response[2] = response_2;
      pending_request_->response[3] = response_3;
    }
  } else if (pending_request_->cmd_flags & (SDMMC_RESP_LEN_48 | SDMMC_RESP_LEN_48B)) {
    pending_request_->response[0] = response_0;
  }

  pending_request_->cmd_complete = true;

  if (pending_request_->cmd_flags & (SDMMC_RESP_DATA_PRESENT | SDMMC_RESP_LEN_48B)) {
    return false;
  }

  // We're done if the command has no data or busy stage
  CompleteRequest();
  return true;
}

void Sdhci::TransferComplete() {
  if (!pending_request_->cmd_complete) {
    FDF_LOG(ERROR, "Transfer complete interrupt received before command complete");
    pending_request_->status.set_error(1).set_command_timeout_error(1);
    ErrorRecovery();
  } else if (!pending_request_->data_transfer_complete()) {
    FDF_LOG(ERROR, "Transfer complete interrupt received before data transferred");
    pending_request_->status.set_error(1).set_data_timeout_error(1);
    ErrorRecovery();
  } else {
    CompleteRequest();
  }
}

bool Sdhci::DataStageReadReady() {
  if ((pending_request_->cmd_idx == MMC_SEND_TUNING_BLOCK) ||
      (pending_request_->cmd_idx == SD_SEND_TUNING_BLOCK)) {
    // This is the final interrupt expected for tuning transfers.
    CompleteRequest();
    return true;
  }

  if (SupportsAdma2() || pending_request_->data_transfer_complete()) {
    return false;
  }

  for (uint32_t i = 0; i < pending_request_->blocksize; i += sizeof(uint32_t)) {
    const uint32_t data = BufferData::Get().ReadFrom(&*regs_mmio_buffer_).reg_value();
    memcpy(pending_request_->data.data(), &data, sizeof(data));
    pending_request_->data = pending_request_->data.subspan(sizeof(data));
  }

  return false;
}

void Sdhci::DataStageWriteReady() {
  if (SupportsAdma2() || pending_request_->data_transfer_complete()) {
    return;
  }

  for (uint32_t i = 0; i < pending_request_->blocksize; i += sizeof(uint32_t)) {
    uint32_t data;
    memcpy(&data, pending_request_->data.data(), sizeof(data));
    pending_request_->data = pending_request_->data.subspan(sizeof(data));
    BufferData::Get().FromValue(data).WriteTo(&*regs_mmio_buffer_);
  }
}

void Sdhci::ErrorRecovery() {
  // Reset internal state machines
  {
    SoftwareReset::Get()
        .ReadFrom(&*regs_mmio_buffer_)
        .set_reset_cmd(1)
        .WriteTo(&*regs_mmio_buffer_);
    [[maybe_unused]] auto _ = WaitForReset(SoftwareReset::Get().FromValue(0).set_reset_cmd(1));
  }
  {
    SoftwareReset::Get()
        .ReadFrom(&*regs_mmio_buffer_)
        .set_reset_dat(1)
        .WriteTo(&*regs_mmio_buffer_);
    [[maybe_unused]] auto _ = WaitForReset(SoftwareReset::Get().FromValue(0).set_reset_dat(1));
  }

  // Complete any pending txn with error status
  CompleteRequest();
}

void Sdhci::CompleteRequest() {
  pending_request_->request_complete = true;
  DisableInterrupts();
  sync_completion_signal(&req_completion_);
}

int Sdhci::IrqThread() {
  while (true) {
    zx_status_t wait_res = WaitForInterrupt();
    if (wait_res != ZX_OK) {
      if (wait_res != ZX_ERR_CANCELED) {
        FDF_LOG(ERROR, "sdhci: interrupt wait failed with retcode = %d", wait_res);
      }
      break;
    }

    // Acknowledge the IRQs that we stashed. IRQs are cleared by writing
    // 1s into the IRQs that fired.
    auto status = InterruptStatus::Get().ReadFrom(&*regs_mmio_buffer_).WriteTo(&*regs_mmio_buffer_);

    FDF_LOG(DEBUG, "got irq 0x%08x en 0x%08x", status.reg_value(),
            InterruptSignalEnable::Get().ReadFrom(&*regs_mmio_buffer_).reg_value());

    std::lock_guard<std::mutex> lock(mtx_);
    if (pending_request_ && !pending_request_->request_complete) {
      HandleTransferInterrupt(status);
    }

    if (status.card_interrupt()) {
      // Disable the card interrupt and call the callback if there is one.
      InterruptStatusEnable::Get()
          .ReadFrom(&*regs_mmio_buffer_)
          .set_card_interrupt(0)
          .WriteTo(&*regs_mmio_buffer_);
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
    pending_request_->status = status;
    pending_request_->status.set_error(1);
    ErrorRecovery();
    return;
  }

  // Clear the interrupt status to indicate that a normal interrupt was handled.
  pending_request_->status = InterruptStatus::Get().FromValue(0);
  if (status.command_complete() && CmdStageComplete()) {
    return;
  }
  if (status.buffer_read_ready() && DataStageReadReady()) {
    return;
  }
  if (status.buffer_write_ready()) {
    DataStageWriteReady();
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

  if (SupportsAdma2()) {
    const uint32_t read_perm = (vmo_rights & SDMMC_VMO_RIGHT_READ) ? ZX_BTI_PERM_READ : 0;
    const uint32_t write_perm = (vmo_rights & SDMMC_VMO_RIGHT_WRITE) ? ZX_BTI_PERM_WRITE : 0;
    zx_status_t status = stored_vmo.Pin(bti_, read_perm | write_perm, true);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to pin VMO %u for client %u: %s", vmo_id, client_id,
              zx_status_get_string(status));
      return status;
    }
  } else {
    const uint32_t write_perm = (vmo_rights & SDMMC_VMO_RIGHT_WRITE) ? ZX_VM_PERM_WRITE : 0;
    zx_status_t status = stored_vmo.Map(ZX_VM_PERM_READ | write_perm);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to map VMO %u for client %u: %s", vmo_id, client_id,
              zx_status_get_string(status));
      return status;
    }
    if (offset > stored_vmo.data().size() || size > (stored_vmo.data().size() - offset)) {
      FDF_LOG(ERROR, "Invalid size or offset for VMO %u for client %u: %s", vmo_id, client_id,
              zx_status_get_string(status));
      return ZX_ERR_OUT_OF_RANGE;
    }
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

  DmaDescriptorBuilder<OwnedVmoInfo> builder(*req, registered_vmo_stores_[req->client_id],
                                             dma_boundary_alignment_, bti_.borrow());

  {
    std::lock_guard<std::mutex> lock(mtx_);

    // one command at a time
    if (pending_request_) {
      return ZX_ERR_SHOULD_WAIT;
    }

    if (zx::result pending_request = StartRequest(*req, builder); pending_request.is_ok()) {
      pending_request_.emplace(*std::move(pending_request));
    } else {
      return pending_request.status_value();
    }
  }

  sync_completion_wait(&req_completion_, ZX_TIME_INFINITE);
  sync_completion_reset(&req_completion_);

  std::lock_guard<std::mutex> lock(mtx_);

  PendingRequest pending_request = *std::move(pending_request_);
  pending_request_.reset();
  return FinishRequest(*req, out_response, pending_request);
}

zx::result<Sdhci::PendingRequest> Sdhci::StartRequest(const sdmmc_req_t& request,
                                                      DmaDescriptorBuilder<OwnedVmoInfo>& builder) {
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
    return zx::error(status);
  }

  TransferMode transfer_mode = TransferMode::Get().FromValue(0);

  const bool is_tuning_request =
      request.cmd_idx == MMC_SEND_TUNING_BLOCK || request.cmd_idx == SD_SEND_TUNING_BLOCK;

  const auto blocksize = static_cast<BlockSizeType>(request.blocksize);

  PendingRequest pending_request(request);

  if (is_tuning_request) {
    // The SDHCI controller has special logic to handle tuning transfers, so there is no need to set
    // up any DMA buffers.
    BlockSize::Get().FromValue(blocksize).WriteTo(&*regs_mmio_buffer_);
    BlockCount::Get().FromValue(0).WriteTo(&*regs_mmio_buffer_);
  } else if (request.cmd_flags & SDMMC_RESP_DATA_PRESENT) {
    if (request.blocksize > std::numeric_limits<BlockSizeType>::max()) {
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }
    if (request.blocksize == 0) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    size_t blockcount = 0;
    if (SupportsAdma2()) {
      if (zx_status_t status = SetUpDma(request, builder); status != ZX_OK) {
        return zx::error(status);
      }

      blockcount = builder.block_count();
      transfer_mode.set_dma_enable(1);
    } else {
      if (zx_status_t status = SetUpBuffer(request, &pending_request)) {
        return zx::error(status);
      }

      blockcount = pending_request.data.size() / blocksize;
      transfer_mode.set_dma_enable(0);
    }

    if (blockcount > std::numeric_limits<BlockCountType>::max()) {
      FDF_LOG(ERROR, "Block count (%lu) exceeds the maximum (%u)", blockcount,
              std::numeric_limits<BlockCountType>::max());
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }

    transfer_mode.set_multi_block(blockcount > 1 ? 1 : 0).set_block_count_enable(1);

    BlockSize::Get().FromValue(blocksize).WriteTo(&*regs_mmio_buffer_);
    BlockCount::Get()
        .FromValue(static_cast<BlockCountType>(blockcount))
        .WriteTo(&*regs_mmio_buffer_);
  } else {
    BlockSize::Get().FromValue(0).WriteTo(&*regs_mmio_buffer_);
    BlockCount::Get().FromValue(0).WriteTo(&*regs_mmio_buffer_);
  }

  Command command = Command::Get().FromValue(0);
  PrepareCmd(request, &transfer_mode, &command);

  Argument::Get().FromValue(request.arg).WriteTo(&*regs_mmio_buffer_);

  // Clear any pending interrupts before starting the transaction.
  auto irq_mask = InterruptSignalEnable::Get().ReadFrom(&*regs_mmio_buffer_);
  InterruptStatus::Get().FromValue(irq_mask.reg_value()).WriteTo(&*regs_mmio_buffer_);

  // Unmask and enable interrupts
  EnableInterrupts();

  // Start command
  transfer_mode.WriteTo(&*regs_mmio_buffer_);
  command.WriteTo(&*regs_mmio_buffer_);

  return zx::ok(std::move(pending_request));
}

zx_status_t Sdhci::SetUpDma(const sdmmc_req_t& request,
                            DmaDescriptorBuilder<OwnedVmoInfo>& builder) {
  const cpp20::span buffers{request.buffers_list, request.buffers_count};
  zx_status_t status;
  for (const auto& buffer : buffers) {
    if (status = builder.ProcessBuffer(buffer); status != ZX_OK) {
      return status;
    }
  }

  size_t descriptor_size;
  if (Capabilities0::Get().ReadFrom(&*regs_mmio_buffer_).v3_64_bit_system_address_support()) {
    const cpp20::span descriptors{reinterpret_cast<AdmaDescriptor96*>(iobuf_->virt()),
                                  kDmaDescCount};
    descriptor_size = sizeof(descriptors[0]);
    status = builder.BuildDmaDescriptors(descriptors);
  } else {
    const cpp20::span descriptors{reinterpret_cast<AdmaDescriptor64*>(iobuf_->virt()),
                                  kDmaDescCount};
    descriptor_size = sizeof(descriptors[0]);
    status = builder.BuildDmaDescriptors(descriptors);
  }

  if (status != ZX_OK) {
    return status;
  }

  status = zx_cache_flush(iobuf_->virt(), builder.descriptor_count() * descriptor_size,
                          ZX_CACHE_FLUSH_DATA);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to clean cache: %s", zx_status_get_string(status));
    return status;
  }

  AdmaSystemAddress::Get(0).FromValue(Lo32(iobuf_->phys())).WriteTo(&*regs_mmio_buffer_);
  AdmaSystemAddress::Get(1).FromValue(Hi32(iobuf_->phys())).WriteTo(&*regs_mmio_buffer_);
  return ZX_OK;
}

zx_status_t Sdhci::SetUpBuffer(const sdmmc_req_t& request, PendingRequest* const pending_request) {
  if (request.buffers_count != 1) {
    FDF_LOG(ERROR, "Only one buffer is supported without DMA");
    return ZX_ERR_NOT_SUPPORTED;
  }
  auto& buffer = request.buffers_list[0];
  if (buffer.size % request.blocksize != 0) {
    FDF_LOG(ERROR, "Total buffer size (%lu) is not a multiple of the request block size (%u)",
            buffer.size, request.blocksize);
    return ZX_ERR_INVALID_ARGS;
  }
  if (request.blocksize % sizeof(uint32_t) != 0) {
    FDF_LOG(ERROR, "Block size (%u) is not a multiple of %zu", request.blocksize, sizeof(uint32_t));
    return ZX_ERR_INVALID_ARGS;
  }

  if (buffer.type == SDMMC_BUFFER_TYPE_VMO_HANDLE) {
    const uint32_t write_perm = (request.cmd_flags & SDMMC_CMD_READ) ? ZX_VM_PERM_WRITE : 0;

    zx::unowned_vmo buffer_vmo(buffer.buffer.vmo);
    zx_status_t status =
        pending_request->vmo_mapper.Map(*buffer_vmo, 0, 0, ZX_VM_PERM_READ | write_perm);
    if (status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to map request VMO: %s", zx_status_get_string(status));
      return status;
    }

    if (buffer.offset > pending_request->vmo_mapper.size() ||
        buffer.size > (pending_request->vmo_mapper.size() - buffer.offset)) {
      FDF_LOG(ERROR, "Buffer size and offset out of range of the VMO");
      return ZX_ERR_OUT_OF_RANGE;
    }

    pending_request->data = {
        reinterpret_cast<uint8_t*>(pending_request->vmo_mapper.start()) + buffer.offset,
        buffer.size};
  } else if (buffer.type == SDMMC_BUFFER_TYPE_VMO_ID) {
    vmo_store::StoredVmo<OwnedVmoInfo>* vmo =
        registered_vmo_stores_[request.client_id].GetVmo(buffer.buffer.vmo_id);
    if (vmo == nullptr) {
      FDF_LOG(ERROR, "Unknown VMO ID %u for client %u", buffer.buffer.vmo_id, request.client_id);
      return ZX_ERR_NOT_FOUND;
    }

    // Size and offset were previously validated by RegisterVmo().
    const cpp20::span<uint8_t> data = vmo->data().subspan(vmo->meta().offset, vmo->meta().size);
    if (buffer.offset > data.size() || buffer.size > (data.size() - buffer.offset)) {
      FDF_LOG(ERROR, "Buffer size and offset out of range of the VMO");
      return ZX_ERR_OUT_OF_RANGE;
    }
    pending_request->data = data.subspan(buffer.offset, buffer.size);
  } else {
    FDF_LOG(ERROR, "Unknown buffer type %u", buffer.type);
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

zx_status_t Sdhci::FinishRequest(const sdmmc_req_t& request, uint32_t out_response[4],
                                 const PendingRequest& pending_request) {
  constexpr uint32_t kResponseMask = SDMMC_RESP_LEN_136 | SDMMC_RESP_LEN_48 | SDMMC_RESP_LEN_48B;
  if (pending_request.cmd_complete && request.cmd_flags & kResponseMask) {
    memcpy(out_response, pending_request.response, sizeof(uint32_t) * 4);
  }

  if (request.cmd_flags & SDMMC_CMD_TYPE_ABORT) {
    // SDHCI spec section 3.8.2: reset the data line after an abort to discard data in the buffer.
    [[maybe_unused]] auto _ =
        WaitForReset(SoftwareReset::Get().FromValue(0).set_reset_cmd(1).set_reset_dat(1));
  }

  if ((request.cmd_flags & SDMMC_RESP_DATA_PRESENT) && (request.cmd_flags & SDMMC_CMD_READ) &&
      SupportsAdma2()) {
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
        FDF_LOG(ERROR, "Failed to clean/invalidate cache: %s", zx_status_get_string(status));
        return status;
      }
    }
  }

  const InterruptStatus interrupt_status = pending_request.status;

  if (!interrupt_status.error()) {
    return ZX_OK;
  }

  if (interrupt_status.tuning_error()) {
    FDF_LOG(ERROR, "Tuning error");
  }
  if (interrupt_status.adma_error()) {
    FDF_LOG(ERROR, "ADMA error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.auto_cmd_error()) {
    FDF_LOG(ERROR, "Auto cmd error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.current_limit_error()) {
    FDF_LOG(ERROR, "Current limit error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.data_end_bit_error()) {
    FDF_LOG(ERROR, "Data end bit error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.data_crc_error()) {
    if (request.suppress_error_messages) {
      FDF_LOG(DEBUG, "Data CRC error cmd%u", request.cmd_idx);
    } else {
      FDF_LOG(ERROR, "Data CRC error cmd%u", request.cmd_idx);
    }
  }
  if (interrupt_status.data_timeout_error()) {
    FDF_LOG(ERROR, "Data timeout error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.command_index_error()) {
    FDF_LOG(ERROR, "Command index error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.command_end_bit_error()) {
    FDF_LOG(ERROR, "Command end bit error cmd%u", request.cmd_idx);
  }
  if (interrupt_status.command_crc_error()) {
    if (request.suppress_error_messages) {
      FDF_LOG(DEBUG, "Command CRC error cmd%u", request.cmd_idx);
    } else {
      FDF_LOG(ERROR, "Command CRC error cmd%u", request.cmd_idx);
    }
  }
  if (interrupt_status.command_timeout_error()) {
    if (request.suppress_error_messages) {
      FDF_LOG(DEBUG, "Command timeout error cmd%u", request.cmd_idx);
    } else {
      FDF_LOG(ERROR, "Command timeout error cmd%u", request.cmd_idx);
    }
  }
  if (interrupt_status.reg_value() ==
      InterruptStatusEnable::Get().FromValue(0).set_error(1).reg_value()) {
    // Log an unknown error only if no other bits were set.
    FDF_LOG(ERROR, "Unknown error cmd%u", request.cmd_idx);
  }

  return ZX_ERR_IO;
}

zx_status_t Sdhci::SdmmcHostInfo(sdmmc_host_info_t* out_info) {
  memcpy(out_info, &info_, sizeof(info_));
  return ZX_OK;
}

zx_status_t Sdhci::SdmmcSetSignalVoltage(sdmmc_voltage_t voltage) {
  std::lock_guard<std::mutex> lock(mtx_);

  // Validate the controller supports the requested voltage
  if ((voltage == SDMMC_VOLTAGE_V330) && !(info_.caps & SDMMC_HOST_CAP_VOLTAGE_330)) {
    FDF_LOG(DEBUG, "sdhci: 3.3V signal voltage not supported");
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto ctrl2 = HostControl2::Get().ReadFrom(&*regs_mmio_buffer_);
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
      FDF_LOG(ERROR, "sdhci: unknown signal voltage value %u", voltage);
      return ZX_ERR_INVALID_ARGS;
  }

  // Note: the SDHCI spec indicates that the data lines should be checked to see if the card is
  // ready for a voltage switch, however that doesn't seem to work for one of our devices.

  ctrl2.set_voltage_1v8_signalling_enable(voltage_1v8_value).WriteTo(&*regs_mmio_buffer_);

  // Wait 5ms for the regulator to stabilize.
  zx::nanosleep(zx::deadline_after(kVoltageStabilizationTime));

  if (ctrl2.ReadFrom(&*regs_mmio_buffer_).voltage_1v8_signalling_enable() != voltage_1v8_value) {
    FDF_LOG(ERROR, "sdhci: voltage regulator output did not become stable");
    // Cut power to the card if the voltage switch failed.
    PowerControl::Get()
        .ReadFrom(&*regs_mmio_buffer_)
        .set_sd_bus_power_vdd1(0)
        .WriteTo(&*regs_mmio_buffer_);
    return ZX_ERR_INTERNAL;
  }

  FDF_LOG(DEBUG, "sdhci: switch signal voltage to %d", voltage);

  return ZX_OK;
}

zx_status_t Sdhci::SdmmcSetBusWidth(sdmmc_bus_width_t bus_width) {
  std::lock_guard<std::mutex> lock(mtx_);

  if ((bus_width == SDMMC_BUS_WIDTH_EIGHT) && !(info_.caps & SDMMC_HOST_CAP_BUS_WIDTH_8)) {
    FDF_LOG(DEBUG, "sdhci: 8-bit bus width not supported");
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto ctrl1 = HostControl1::Get().ReadFrom(&*regs_mmio_buffer_);

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
      FDF_LOG(ERROR, "sdhci: unknown bus width value %u", bus_width);
      return ZX_ERR_INVALID_ARGS;
  }

  ctrl1.WriteTo(&*regs_mmio_buffer_);

  FDF_LOG(DEBUG, "sdhci: set bus width to %d", bus_width);

  return ZX_OK;
}

zx_status_t Sdhci::SdmmcSetBusFreq(uint32_t bus_freq) {
  std::lock_guard<std::mutex> lock(mtx_);

  zx_status_t st = WaitForInhibit(
      PresentState::Get().FromValue(0).set_command_inhibit_cmd(1).set_command_inhibit_dat(1));
  if (st != ZX_OK) {
    return st;
  }

  return SetBusClock(bus_freq);
}

zx_status_t Sdhci::SetBusClock(uint32_t frequency_hz) {
  fdf::WireUnownedResult result = sdhci_.buffer(arena_)->VendorSetBusClock(frequency_hz);
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send VendorSetBusClock request: %s", result.status_string());
    return result.status();
  }
  if (result->is_ok()) {
    return ZX_OK;
  }
  if (result->error_value() != ZX_ERR_STOP) {
    FDF_LOG(ERROR, "Failed to set bus clock: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // Turn off the SD clock before messing with the clock rate.
  auto clock = ClockControl::Get()
                   .ReadFrom(&*regs_mmio_buffer_)
                   .set_sd_clock_enable(0)
                   .WriteTo(&*regs_mmio_buffer_)
                   .set_internal_clock_enable(1);

  if (frequency_hz > 0) {
    // Write the new divider into the control register.
    clock.set_frequency_select(GetClockDividerValue(base_clock_, frequency_hz));
  }

  clock.WriteTo(&*regs_mmio_buffer_);

  if (zx_status_t status = WaitForInternalClockStable(); status != ZX_OK) {
    return status;
  }

  if (frequency_hz > 0) {
    // Turn the SD clock back on.
    clock.set_sd_clock_enable(1).WriteTo(&*regs_mmio_buffer_);
  }

  return ZX_OK;
}

zx_status_t Sdhci::SdmmcSetTiming(sdmmc_timing_t timing) {
  std::lock_guard<std::mutex> lock(mtx_);

  auto ctrl1 = HostControl1::Get().ReadFrom(&*regs_mmio_buffer_);

  // Toggle high-speed
  if (timing != SDMMC_TIMING_LEGACY) {
    ctrl1.set_high_speed_enable(1).WriteTo(&*regs_mmio_buffer_);
  } else {
    ctrl1.set_high_speed_enable(0).WriteTo(&*regs_mmio_buffer_);
  }

  auto ctrl2 = HostControl2::Get().ReadFrom(&*regs_mmio_buffer_);
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
      FDF_LOG(ERROR, "sdhci: unknown timing value %u", timing);
      return ZX_ERR_INVALID_ARGS;
  }
  ctrl2.WriteTo(&*regs_mmio_buffer_);

  FDF_LOG(DEBUG, "sdhci: set bus timing to %d", timing);

  return ZX_OK;
}

zx_status_t Sdhci::SdmmcHwReset() {
  std::lock_guard<std::mutex> lock(mtx_);

  fdf::WireUnownedResult result = sdhci_.buffer(arena_)->HwReset();
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send HwReset request: %s", result.status_string());
    return result.status();
  }

  return ZX_OK;
}

zx_status_t Sdhci::SdmmcPerformTuning(uint32_t cmd_idx) {
  FDF_LOG(DEBUG, "sdhci: perform tuning");

  uint16_t blocksize;
  auto ctrl2 = HostControl2::Get().FromValue(0);

  {
    std::lock_guard<std::mutex> lock(mtx_);
    blocksize = static_cast<uint16_t>(
        HostControl1::Get().ReadFrom(&*regs_mmio_buffer_).extended_data_transfer_width() ? 128
                                                                                         : 64);
    ctrl2.ReadFrom(&*regs_mmio_buffer_).set_execute_tuning(1).WriteTo(&*regs_mmio_buffer_);
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
      FDF_LOG(ERROR, "sdhci: MMC_SEND_TUNING_BLOCK error, retcode = %d", st);
      return st;
    }

    std::lock_guard<std::mutex> lock(mtx_);
    ctrl2.ReadFrom(&*regs_mmio_buffer_);
  }

  {
    std::lock_guard<std::mutex> lock(mtx_);
    ctrl2.ReadFrom(&*regs_mmio_buffer_);
  }

  const bool fail = ctrl2.execute_tuning() || !ctrl2.use_tuned_clock();

  FDF_LOG(DEBUG, "sdhci: tuning fail %d", fail);

  return fail ? ZX_ERR_IO : ZX_OK;
}

zx_status_t Sdhci::SdmmcRegisterInBandInterrupt(const in_band_interrupt_protocol_t* interrupt_cb) {
  std::lock_guard<std::mutex> lock(mtx_);

  interrupt_cb_ = ddk::InBandInterruptProtocolClient(interrupt_cb);

  InterruptSignalEnable::Get()
      .ReadFrom(&*regs_mmio_buffer_)
      .set_card_interrupt(1)
      .WriteTo(&*regs_mmio_buffer_);
  InterruptStatusEnable::Get()
      .ReadFrom(&*regs_mmio_buffer_)
      .set_card_interrupt(card_interrupt_masked_ ? 0 : 1)
      .WriteTo(&*regs_mmio_buffer_);

  // Call the callback if an interrupt was raised before it was registered.
  if (card_interrupt_masked_) {
    interrupt_cb_.Callback();
  }

  return ZX_OK;
}

void Sdhci::SdmmcAckInBandInterrupt() {
  std::lock_guard<std::mutex> lock(mtx_);
  InterruptStatusEnable::Get()
      .ReadFrom(&*regs_mmio_buffer_)
      .set_card_interrupt(1)
      .WriteTo(&*regs_mmio_buffer_);
  card_interrupt_masked_ = false;
}

void Sdhci::PrepareStop(fdf::PrepareStopCompleter completer) {
  // stop irq thread
  irq_.destroy();
  if (irq_thread_started_) {
    thrd_join(irq_thread_, nullptr);
  }

  completer(zx::ok());
}

zx_status_t Sdhci::Init() {
  SetBusClock(0);

  // Perform a software reset against both the DAT and CMD interface.
  SoftwareReset::Get().ReadFrom(&*regs_mmio_buffer_).set_reset_all(1).WriteTo(&*regs_mmio_buffer_);

  // Wait for reset to take place. The reset is completed when all three
  // of the following flags are reset.
  const SoftwareReset target_mask =
      SoftwareReset::Get().FromValue(0).set_reset_all(1).set_reset_cmd(1).set_reset_dat(1);
  zx_status_t status = ZX_OK;
  if (status = WaitForReset(target_mask); status != ZX_OK) {
    return status;
  }

  // The core has been reset, which should have stopped any DMAs that were happening when the driver
  // started. It is now safe to release quarantined pages.
  if (status = bti_.release_quarantine(); status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to release quarantined pages: %d", status);
    return status;
  }

  // Ensure that we're SDv3.
  const uint16_t vrsn =
      HostControllerVersion::Get().ReadFrom(&*regs_mmio_buffer_).specification_version();
  if (vrsn < HostControllerVersion::kSpecificationVersion300) {
    FDF_LOG(ERROR, "sdhci: SD version is %u, only version %u is supported", vrsn,
            HostControllerVersion::kSpecificationVersion300);
    return ZX_ERR_NOT_SUPPORTED;
  }
  FDF_LOG(DEBUG, "sdhci: controller version %d", vrsn);

  auto caps0 = Capabilities0::Get().ReadFrom(&*regs_mmio_buffer_);
  auto caps1 = Capabilities1::Get().ReadFrom(&*regs_mmio_buffer_);

  base_clock_ = caps0.base_clock_frequency_hz();
  if (base_clock_ == 0) {
    // try to get controller specific base clock
    fdf::WireUnownedResult base_clock = sdhci_.buffer(arena_)->GetBaseClock();
    if (!base_clock.ok()) {
      FDF_LOG(ERROR, "Failed to send GetBaseClock request: %s", base_clock.status_string());
      return base_clock.status();
    }
    base_clock_ = base_clock->clock;
  }
  if (base_clock_ == 0) {
    FDF_LOG(ERROR, "sdhci: base clock is 0!");
    return ZX_ERR_INTERNAL;
  }

  const bool non_standard_tuning =
      static_cast<bool>(quirks_ & fuchsia_hardware_sdhci::Quirk::kNonStandardTuning);
  const bool tuning_for_sdr50 = caps1.use_tuning_for_sdr50();

  // Get controller capabilities
  if (caps0.bus_width_8_support()) {
    info_.caps |= SDMMC_HOST_CAP_BUS_WIDTH_8;
  }
  if (caps0.adma2_support() && !(quirks_ & fuchsia_hardware_sdhci::Quirk::kNoDma)) {
    info_.caps |= SDMMC_HOST_CAP_DMA;
  }
  if (caps0.voltage_3v3_support()) {
    info_.caps |= SDMMC_HOST_CAP_VOLTAGE_330;
  }
  if (caps1.sdr50_support() && (!non_standard_tuning || !tuning_for_sdr50)) {
    info_.caps |= SDMMC_HOST_CAP_SDR50;
  }
  if (caps1.ddr50_support() && !(quirks_ & fuchsia_hardware_sdhci::Quirk::kNoDdr)) {
    info_.caps |= SDMMC_HOST_CAP_DDR50;
  }
  if (caps1.sdr104_support() && !non_standard_tuning) {
    info_.caps |= SDMMC_HOST_CAP_SDR104;
  }
  if (!tuning_for_sdr50) {
    info_.caps |= SDMMC_HOST_CAP_NO_TUNING_SDR50;
  }
  info_.caps |= SDMMC_HOST_CAP_AUTO_CMD12;

  // allocate and setup DMA descriptor
  if (SupportsAdma2()) {
    auto buffer_factory = dma_buffer::CreateBufferFactory();
    auto host_control1 = HostControl1::Get().ReadFrom(&*regs_mmio_buffer_);
    if (caps0.v3_64_bit_system_address_support()) {
      const size_t buffer_size =
          fbl::round_up(kDmaDescCount * sizeof(AdmaDescriptor96), zx_system_get_page_size());
      status = buffer_factory->CreateContiguous(bti_, buffer_size, 0, &iobuf_);
      host_control1.set_dma_select(HostControl1::kDmaSelect64BitAdma2);
    } else {
      const size_t buffer_size =
          fbl::round_up(kDmaDescCount * sizeof(AdmaDescriptor64), zx_system_get_page_size());
      status = buffer_factory->CreateContiguous(bti_, buffer_size, 0, &iobuf_);
      host_control1.set_dma_select(HostControl1::kDmaSelect32BitAdma2);

      if ((iobuf_->phys() & k32BitPhysAddrMask) != iobuf_->phys()) {
        FDF_LOG(ERROR, "Got 64-bit physical address, only 32-bit DMA is supported");
        return ZX_ERR_NOT_SUPPORTED;
      }
    }

    if (status != ZX_OK) {
      FDF_LOG(ERROR, "sdhci: error allocating DMA descriptors");
      return status;
    }
    info_.max_transfer_size = kDmaDescCount * zx_system_get_page_size();

    host_control1.WriteTo(&*regs_mmio_buffer_);
  } else {
    // Assumes typical block size of 512 bytes, though in theory the request could specify a
    // different value.
    constexpr size_t kBlockSize = 512;
    info_.max_transfer_size = std::numeric_limits<BlockCountType>::max() * kBlockSize;
  }

  // Set the command timeout.
  TimeoutControl::Get()
      .ReadFrom(&*regs_mmio_buffer_)
      .set_data_timeout_counter(TimeoutControl::kDataTimeoutMax)
      .WriteTo(&*regs_mmio_buffer_);

  // Set SD bus voltage to maximum supported by the host controller
  auto power = PowerControl::Get().ReadFrom(&*regs_mmio_buffer_).set_sd_bus_power_vdd1(1);
  if (info_.caps & SDMMC_HOST_CAP_VOLTAGE_330) {
    power.set_sd_bus_voltage_vdd1(PowerControl::kBusVoltage3V3);
  } else {
    power.set_sd_bus_voltage_vdd1(PowerControl::kBusVoltage1V8);
  }
  power.WriteTo(&*regs_mmio_buffer_);

  // Enable the SD clock.
  status = SetBusClock(kSdFreqSetupHz);
  if (status != ZX_OK) {
    return status;
  }

  // Disable all interrupts
  {
    std::lock_guard<std::mutex> lock(mtx_);
    DisableInterrupts();
  }

  // TODO: Switch to use dispatcher thread instead.
  if (thrd_create_with_name(
          &irq_thread_, [](void* arg) -> int { return reinterpret_cast<Sdhci*>(arg)->IrqThread(); },
          this, "sdhci_irq_thread") != thrd_success) {
    FDF_LOG(ERROR, "sdhci: failed to create irq thread");
    return ZX_ERR_INTERNAL;
  }
  irq_thread_started_ = true;

  // Set controller preferences
  fuchsia_hardware_sdmmc::SdmmcHostPrefs speed_capabilities{};
  if (quirks_ & fuchsia_hardware_sdhci::Quirk::kNonStandardTuning) {
    // Disable HS200 and HS400 if tuning cannot be performed as per the spec.
    speed_capabilities |= fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs200 |
                          fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs400;
  }
  if (quirks_ & fuchsia_hardware_sdhci::Quirk::kNoDdr) {
    speed_capabilities |= fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHsddr |
                          fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs400;
  }

  fidl::Arena<> fidl_arena;
  auto builder = fuchsia_hardware_sdmmc::wire::SdmmcMetadata::Builder(fidl_arena).use_fidl(false);

  zx::result existing_metadata = compat::GetMetadata<fuchsia_hardware_sdmmc::wire::SdmmcMetadata>(
      incoming(), fidl_arena, DEVICE_METADATA_SDMMC);
  if (existing_metadata.is_error()) {
    builder.speed_capabilities(speed_capabilities);
  } else {
    if (existing_metadata->has_max_frequency()) {
      builder.max_frequency(existing_metadata->max_frequency());
    }

    if (existing_metadata->has_speed_capabilities()) {
      // OR the speed capabilities reported by the parent with the ones reported by the host
      // controller, which limits us to speed modes supported by both.
      builder.speed_capabilities(existing_metadata->speed_capabilities() | speed_capabilities);
    } else {
      builder.speed_capabilities(speed_capabilities);
    }

    if (existing_metadata->has_enable_cache()) {
      builder.enable_cache(existing_metadata->enable_cache());
    }

    if (existing_metadata->has_removable()) {
      builder.removable(existing_metadata->removable());
    }

    if (existing_metadata->has_max_command_packing()) {
      builder.max_command_packing(existing_metadata->max_command_packing());
    }
  }

  if (!SupportsAdma2()) {
    // Non-DMA requests are only allowed to use a single buffer, so tell the core driver to disable
    // command packing. This limitation could be removed in the future.
    builder.max_command_packing(0);
  }

  fit::result metadata = fidl::Persist(builder.Build());
  if (!metadata.is_ok()) {
    FDF_LOG(ERROR, "Failed to encode SDMMC metadata: %s",
            metadata.error_value().FormatDescription().c_str());
    return metadata.error_value().status();
  }
  status = compat_server_.inner().AddMetadata(DEVICE_METADATA_SDMMC, metadata.value().data(),
                                              metadata.value().size());
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to add SDMMC metadata: %s", zx_status_get_string(status));
  }
  return status;
}

zx_status_t Sdhci::InitMmio() {
  // Map the Device Registers so that we can perform MMIO against the device.
  zx::vmo vmo;
  zx_off_t vmo_offset;
  {
    fdf::WireUnownedResult mmio = sdhci_.buffer(arena_)->GetMmio();
    if (!mmio.ok()) {
      FDF_LOG(ERROR, "Failed to send GetMmio request: %s", mmio.status_string());
      return mmio.status();
    }
    if (mmio->is_error()) {
      FDF_LOG(ERROR, "Failed to get mmio: %s", zx_status_get_string(mmio->error_value()));
      return mmio->error_value();
    }
    vmo = std::move(mmio.value()->mmio);
    vmo_offset = mmio.value()->offset;
  }

  zx::result<fdf::MmioBuffer> regs_mmio_buffer = fdf::MmioBuffer::Create(
      vmo_offset, kRegisterSetSize, std::move(vmo), ZX_CACHE_POLICY_UNCACHED_DEVICE);
  if (regs_mmio_buffer.is_error()) {
    FDF_LOG(ERROR, "sdhci: error %s in mmio_buffer_init", regs_mmio_buffer.status_string());
    return regs_mmio_buffer.status_value();
  }
  regs_mmio_buffer_ = *std::move(regs_mmio_buffer);

  return ZX_OK;
}

zx::result<> Sdhci::Start() {
  {
    zx::result sdhci = incoming()->Connect<fuchsia_hardware_sdhci::Service::Device>();
    if (sdhci.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to sdhci: %s", sdhci.status_string());
      return sdhci.take_error();
    }
    sdhci_.Bind(std::move(sdhci.value()));
  }

  zx_status_t status = InitMmio();
  if (status != ZX_OK) {
    return zx::error(status);
  }

  {
    fdf::WireUnownedResult bti = sdhci_.buffer(arena_)->GetBti(0);
    if (!bti.ok()) {
      FDF_LOG(ERROR, "Failed to send GetBti request: %s", bti.status_string());
      return zx::error(bti.status());
    }
    if (bti->is_error()) {
      FDF_LOG(ERROR, "Failed to get bti: %s", zx_status_get_string(bti->error_value()));
      return zx::error(bti->error_value());
    }
    bti_ = std::move(bti.value()->bti);
  }

  {
    fdf::WireUnownedResult irq = sdhci_.buffer(arena_)->GetInterrupt();
    if (!irq.ok()) {
      FDF_LOG(ERROR, "Failed to send GetInterrupt request: %s", irq.status_string());
      return zx::error(irq.status());
    }
    if (irq->is_error()) {
      FDF_LOG(ERROR, "Failed to get interrupt: %s", zx_status_get_string(irq->error_value()));
      return zx::error(irq->error_value());
    }
    irq_ = std::move(irq.value()->irq);
  }

  dma_boundary_alignment_ = 0;

  {
    fdf::WireUnownedResult quirks = sdhci_.buffer(arena_)->GetQuirks();
    if (!quirks.ok()) {
      FDF_LOG(ERROR, "Failed to send GetQuirks request: %s", quirks.status_string());
      return zx::error(quirks.status());
    }
    quirks_ = quirks.value().quirks;
    dma_boundary_alignment_ = quirks.value().dma_boundary_alignment;
  }

  if (!(quirks_ & fuchsia_hardware_sdhci::Quirk::kUseDmaBoundaryAlignment)) {
    dma_boundary_alignment_ = 0;
  } else if (dma_boundary_alignment_ == 0) {
    FDF_LOG(ERROR, "sdhci: DMA boundary alignment is zero");
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  {
    compat::DeviceServer::BanjoConfig banjo_config;
    banjo_config.callbacks[ZX_PROTOCOL_SDMMC] = sdmmc_server_.callback();
    zx::result<> result = compat_server_.Initialize(
        incoming(), outgoing(), node_name(), name(),
        compat::ForwardMetadata::Some({DEVICE_METADATA_SDMMC}), std::move(banjo_config));
    if (result.is_error()) {
      return result.take_error();
    }
  }

  // initialize the controller
  status = Init();
  auto cleanup = fit::defer([this] {
    irq_.destroy();
    if (irq_thread_started_) {
      thrd_join(irq_thread_, nullptr);
    }
  });
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "%s: SDHCI Controller init failed", __func__);
    return zx::error(status);
  }

  parent_node_.Bind(std::move(node()));

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  node_controller_.Bind(std::move(controller_client_end));

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia_hardware_sdmmc::SDMMCSERVICE,
                                    bind_fuchsia_hardware_sdmmc::SDMMCSERVICE_DRIVERTRANSPORT);

  std::vector<fuchsia_driver_framework::wire::Offer> offers = compat_server_.CreateOffers2(arena);

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, name())
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  auto result = parent_node_->AddChild(args, std::move(controller_server_end), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }

  cleanup.cancel();
  return zx::ok();
}

}  // namespace sdhci
