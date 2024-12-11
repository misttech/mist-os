// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_SDHCI_SDHCI_H_
#define SRC_DEVICES_BLOCK_DRIVERS_SDHCI_SDHCI_H_

#include <fidl/fuchsia.hardware.sdhci/cpp/driver/fidl.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/dma-buffer/buffer.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/mmio/mmio.h>
#include <lib/sdmmc/hw.h>
#include <lib/sync/completion.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/bti.h>
#include <lib/zx/interrupt.h>
#include <zircon/threads.h>

#include <mutex>
#include <optional>

#include "dma-descriptor-builder.h"
#include "sdhci-reg.h"
#include "src/lib/vmo_store/vmo_store.h"

namespace sdhci {

class Sdhci : public fdf::DriverBase, public ddk::SdmmcProtocol<Sdhci> {
 public:
  // Visible for testing.
  struct AdmaDescriptor96 {
    uint16_t attr;
    uint16_t length;
    uint64_t address;

    uint64_t get_address() const {
      uint64_t addr;
      memcpy(&addr, &address, sizeof(addr));
      return addr;
    }
  } __PACKED;
  static_assert(sizeof(AdmaDescriptor96) == 12, "unexpected ADMA2 descriptor size");

  struct AdmaDescriptor64 {
    uint16_t attr;
    uint16_t length;
    uint32_t address;
  } __PACKED;
  static_assert(sizeof(AdmaDescriptor64) == 8, "unexpected ADMA2 descriptor size");

  static constexpr char kDriverName[] = "sdhci";

  Sdhci(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(dispatcher)),
        registered_vmo_stores_{
            // SdmmcVmoStore does not have a default constructor, so construct each one using an
            // empty Options (do not map or pin automatically upon VMO registration).
            // clang-format off
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            SdmmcVmoStore{vmo_store::Options{}},
            // clang-format on
        } {}

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  zx_status_t SdmmcHostInfo(sdmmc_host_info_t* out_info);
  zx_status_t SdmmcSetSignalVoltage(sdmmc_voltage_t voltage) TA_EXCL(mtx_);
  zx_status_t SdmmcSetBusWidth(sdmmc_bus_width_t bus_width) TA_EXCL(mtx_);
  zx_status_t SdmmcSetBusFreq(uint32_t bus_freq) TA_EXCL(mtx_);
  zx_status_t SdmmcSetTiming(sdmmc_timing_t timing) TA_EXCL(mtx_);
  zx_status_t SdmmcHwReset() TA_EXCL(mtx_);
  zx_status_t SdmmcPerformTuning(uint32_t cmd_idx) TA_EXCL(mtx_);
  zx_status_t SdmmcRequest(sdmmc_req_t* req) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t SdmmcRegisterInBandInterrupt(const in_band_interrupt_protocol_t* interrupt_cb)
      TA_EXCL(mtx_);
  void SdmmcAckInBandInterrupt() TA_EXCL(mtx_);
  zx_status_t SdmmcRegisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo vmo, uint64_t offset,
                               uint64_t size, uint32_t vmo_rights);
  zx_status_t SdmmcUnregisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo* out_vmo);
  zx_status_t SdmmcRequest(const sdmmc_req_t* req, uint32_t out_response[4]) TA_EXCL(mtx_);

  // Visible for testing.
  uint32_t base_clock() const { return base_clock_; }

 protected:
  // All protected members are visible for testing.
  enum class RequestStatus {
    IDLE,
    COMMAND,
    TRANSFER_DATA_DMA,
    READ_DATA_PIO,
    WRITE_DATA_PIO,
    BUSY_RESPONSE,
  };

  RequestStatus GetRequestStatus() TA_EXCL(&mtx_) {
    std::lock_guard<std::mutex> lock(mtx_);
    if (pending_request_ && !pending_request_->request_complete) {
      const bool has_data = pending_request_->cmd_flags & SDMMC_RESP_DATA_PRESENT;
      const bool busy_response = pending_request_->cmd_flags & SDMMC_RESP_LEN_48B;

      if (!pending_request_->cmd_complete) {
        return RequestStatus::COMMAND;
      }
      if (!pending_request_->data.empty()) {
        if (pending_request_->cmd_flags & SDMMC_CMD_READ) {
          return RequestStatus::READ_DATA_PIO;
        }
        return RequestStatus::WRITE_DATA_PIO;
      }
      if (has_data) {
        return RequestStatus::TRANSFER_DATA_DMA;
      }
      if (busy_response) {
        return RequestStatus::BUSY_RESPONSE;
      }
    }
    return RequestStatus::IDLE;
  }

  // Override to inject dependency for unit testing.
  virtual zx_status_t InitMmio();
  virtual zx_status_t WaitForReset(SoftwareReset mask);
  virtual zx_status_t WaitForInterrupt() { return irq_.wait(nullptr); }

  std::optional<fdf::MmioBuffer> regs_mmio_buffer_;

  // DMA descriptors, visible for testing
  std::unique_ptr<dma_buffer::ContiguousBuffer> iobuf_;

 private:
  struct OwnedVmoInfo {
    uint64_t offset;
    uint64_t size;
    uint32_t rights;
  };

  // Used to synchronize the request thread(s) with the interrupt thread for requests through
  // SdmmcRequest. See above for SdmmcRequest requests.
  struct PendingRequest {
    explicit PendingRequest(const sdmmc_req_t& request)
        : cmd_idx(request.cmd_idx),
          cmd_flags(request.cmd_flags),
          blocksize(request.blocksize),
          status(InterruptStatus::Get().FromValue(0).set_error(1)) {}

    bool data_transfer_complete() const {
      return !(cmd_flags & SDMMC_RESP_DATA_PRESENT) || data.empty();
    }

    const uint32_t cmd_idx;
    const uint32_t cmd_flags;
    const uint32_t blocksize;

    // If false, a command is in progress on the bus, and the interrupt thread is waiting for the
    // command complete interrupt.
    bool cmd_complete = false;

    // If true, all stages of the request have completed, and the main thread has been signaled.
    bool request_complete = false;

    // The 0-, 32-, or 128-bit response (unused fields set to zero). Set by the interrupt thread and
    // read by the request thread.
    uint32_t response[4] = {};

    // If an error occurred, the interrupt thread sets this field to the value of the status
    // register (and always sets the general error bit). If no error  occurred the interrupt thread
    // sets this field to zero.
    InterruptStatus status;

    // For a non-DMA request, data points to the buffer to read from/write to. This buffer may be
    // owned by vmo_mapper.
    cpp20::span<uint8_t> data;
    fzl::VmoMapper vmo_mapper;
  };

  using BlockSizeType = decltype(BlockSize::Get().FromValue(0).reg_value());
  using BlockCountType = decltype(BlockCount::Get().FromValue(0).reg_value());
  using SdmmcVmoStore = DmaDescriptorBuilder<OwnedVmoInfo>::VmoStore;

  static void PrepareCmd(const sdmmc_req_t& req, TransferMode* transfer_mode, Command* command);

  zx_status_t Init();

  bool SupportsAdma2() const {
    return (info_.caps & SDMMC_HOST_CAP_DMA) && !(quirks_ & fuchsia_hardware_sdhci::Quirk::kNoDma);
  }

  void EnableInterrupts() TA_REQ(mtx_);
  void DisableInterrupts() TA_REQ(mtx_);

  zx_status_t WaitForInhibit(PresentState mask) const;
  zx_status_t WaitForInternalClockStable() const;

  int IrqThread() TA_EXCL(mtx_);
  void HandleTransferInterrupt(InterruptStatus status) TA_REQ(mtx_);

  zx::result<PendingRequest> StartRequest(const sdmmc_req_t& request,
                                          DmaDescriptorBuilder<OwnedVmoInfo>& builder) TA_REQ(mtx_);
  zx_status_t SetUpDma(const sdmmc_req_t& request, DmaDescriptorBuilder<OwnedVmoInfo>& builder)
      TA_REQ(mtx_);
  zx_status_t SetUpBuffer(const sdmmc_req_t& request, PendingRequest* pending_request) TA_REQ(mtx_);
  zx_status_t FinishRequest(const sdmmc_req_t& request, uint32_t out_response[4],
                            const PendingRequest& pending_request) TA_REQ(mtx_);

  void CompleteRequest() TA_REQ(mtx_);

  // Always signals the main thread.
  void ErrorRecovery() TA_REQ(mtx_);

  // These return true if the main thread was signaled and no further processing is needed.
  bool CmdStageComplete() TA_REQ(mtx_);
  void TransferComplete() TA_REQ(mtx_);
  bool DataStageReadReady() TA_REQ(mtx_);
  void DataStageWriteReady() TA_REQ(mtx_);

  zx_status_t SetBusClock(uint32_t frequency_hz);

  zx::interrupt irq_;
  thrd_t irq_thread_;
  bool irq_thread_started_ = false;

  fdf::WireSyncClient<fuchsia_hardware_sdhci::Device> sdhci_;
  fdf::Arena arena_{'SDHC'};

  zx::bti bti_;

  // Held when a command or action is in progress.
  std::mutex mtx_;

  // used to signal request complete
  sync_completion_t req_completion_;

  // Controller info
  sdmmc_host_info_t info_ = {};

  // Controller specific quirks
  fuchsia_hardware_sdhci::Quirk quirks_;
  uint64_t dma_boundary_alignment_;

  // Base clock rate
  uint32_t base_clock_ = 0;

  ddk::InBandInterruptProtocolClient interrupt_cb_;
  bool card_interrupt_masked_ TA_GUARDED(mtx_) = false;

  // Keep one SdmmcVmoStore for each possible client ID (IDs are in [0, SDMMC_MAX_CLIENT_ID]).
  std::array<SdmmcVmoStore, SDMMC_MAX_CLIENT_ID + 1> registered_vmo_stores_;

  std::optional<PendingRequest> pending_request_ TA_GUARDED(mtx_);

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;

  compat::BanjoServer sdmmc_server_{ZX_PROTOCOL_SDMMC, this, &sdmmc_protocol_ops_};
  compat::SyncInitializedDeviceServer compat_server_;
};

}  // namespace sdhci

#endif  // SRC_DEVICES_BLOCK_DRIVERS_SDHCI_SDHCI_H_
