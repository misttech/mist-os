// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_SDHCI_SDHCI_H_
#define SRC_DEVICES_BLOCK_DRIVERS_SDHCI_SDHCI_H_

#include <fuchsia/hardware/sdhci/cpp/banjo.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/ddk/io-buffer.h>
#include <lib/mmio/mmio.h>
#include <lib/sdmmc/hw.h>
#include <lib/sync/completion.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/bti.h>
#include <lib/zx/interrupt.h>
#include <zircon/threads.h>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <fbl/mutex.h>

#include "dma-descriptor-builder.h"
#include "sdhci-reg.h"
#include "src/lib/vmo_store/vmo_store.h"

namespace sdhci {

class Sdhci;
using DeviceType = ddk::Device<Sdhci, ddk::Unbindable>;

class Sdhci : public DeviceType, public ddk::SdmmcProtocol<Sdhci, ddk::base_protocol> {
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

  Sdhci(zx_device_t* parent, fdf::MmioBuffer regs_mmio_buffer, zx::bti bti, zx::interrupt irq,
        const ddk::SdhciProtocolClient sdhci, uint64_t quirks, uint64_t dma_boundary_alignment)
      : DeviceType(parent),
        regs_mmio_buffer_(std::move(regs_mmio_buffer)),
        irq_(std::move(irq)),
        sdhci_(sdhci),
        bti_(std::move(bti)),
        quirks_(quirks),
        dma_boundary_alignment_(dma_boundary_alignment),
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

  virtual ~Sdhci() = default;

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  void DdkRelease();
  void DdkUnbind(ddk::UnbindTxn txn);

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
  zx_status_t Init();

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
    fbl::AutoLock lock(&mtx_);
    if (pending_request_.is_pending()) {
      const bool has_data = pending_request_.cmd_flags & SDMMC_RESP_DATA_PRESENT;
      const bool busy_response = pending_request_.cmd_flags & SDMMC_RESP_LEN_48B;

      if (!pending_request_.cmd_done) {
        return RequestStatus::COMMAND;
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

  virtual zx_status_t WaitForReset(SoftwareReset mask);
  virtual zx_status_t WaitForInterrupt() { return irq_.wait(nullptr); }

  fdf::MmioBuffer regs_mmio_buffer_;

  // DMA descriptors, visible for testing
  ddk::IoBuffer iobuf_ = {};

 private:
  struct OwnedVmoInfo {
    uint64_t offset;
    uint64_t size;
    uint32_t rights;
  };

  using SdmmcVmoStore = DmaDescriptorBuilder<OwnedVmoInfo>::VmoStore;

  static void PrepareCmd(const sdmmc_req_t& req, TransferMode* transfer_mode, Command* command);

  bool SupportsAdma2() const {
    return (info_.caps & SDMMC_HOST_CAP_DMA) && !(quirks_ & SDHCI_QUIRK_NO_DMA);
  }

  void EnableInterrupts() TA_REQ(mtx_);
  void DisableInterrupts() TA_REQ(mtx_);

  zx_status_t WaitForInhibit(const PresentState mask) const;
  zx_status_t WaitForInternalClockStable() const;

  int IrqThread() TA_EXCL(mtx_);
  void HandleTransferInterrupt(InterruptStatus status) TA_REQ(mtx_);

  zx_status_t StartRequest(const sdmmc_req_t& request, DmaDescriptorBuilder<OwnedVmoInfo>& builder)
      TA_REQ(mtx_);
  zx_status_t SetUpDma(const sdmmc_req_t& request, DmaDescriptorBuilder<OwnedVmoInfo>& builder)
      TA_REQ(mtx_);
  zx_status_t FinishRequest(const sdmmc_req_t& request, uint32_t out_response[4]) TA_REQ(mtx_);

  void CompleteRequest() TA_REQ(mtx_);

  // Always signals the main thread.
  void ErrorRecovery() TA_REQ(mtx_);

  // These return true if the main thread was signaled and no further processing is needed.
  bool CmdStageComplete() TA_REQ(mtx_);
  bool TransferComplete() TA_REQ(mtx_);
  bool DataStageReadReady() TA_REQ(mtx_);

  zx::interrupt irq_;
  thrd_t irq_thread_;

  const ddk::SdhciProtocolClient sdhci_;

  zx::bti bti_;

  // Held when a command or action is in progress.
  fbl::Mutex mtx_;

  // used to signal request complete
  sync_completion_t req_completion_;

  // Controller info
  sdmmc_host_info_t info_ = {};

  // Controller specific quirks
  const uint64_t quirks_;
  const uint64_t dma_boundary_alignment_;

  // Base clock rate
  uint32_t base_clock_ = 0;

  ddk::InBandInterruptProtocolClient interrupt_cb_;
  bool card_interrupt_masked_ TA_GUARDED(mtx_) = false;

  // Keep one SdmmcVmoStore for each possible client ID (IDs are in [0, SDMMC_MAX_CLIENT_ID]).
  std::array<SdmmcVmoStore, SDMMC_MAX_CLIENT_ID + 1> registered_vmo_stores_;

  // Used to synchronize the request thread(s) with the interrupt thread for requests through
  // SdmmcRequest. See above for SdmmcRequest requests.
  struct PendingRequest {
    PendingRequest() { Reset(); }

    // Initializes the PendingRequest based on the command index and flags. cmd_done is set to false
    // to indicate that there is now a request pending.
    void Init(const sdmmc_req_t& request) {
      cmd_idx = request.cmd_idx;
      cmd_flags = request.cmd_flags;
      cmd_done = false;
      // No data phase if there is no data present and no busy response.
      data_done = !(cmd_flags & (SDMMC_RESP_DATA_PRESENT | SDMMC_RESP_LEN_48B));
      status = InterruptStatus::Get().FromValue(0).set_error(1);
    }

    uint32_t cmd_idx;
    // If false, a command is in progress on the bus, and the interrupt thread is waiting for the
    // command complete interrupt.
    bool cmd_done;
    // If false, data is being transferred on the bus, and the interrupt thread is waiting for the
    // transfer complete interrupt. Set to true for requests that have no data transfer.
    bool data_done;
    // The flags for the current request, used to determine what response (if any) is expected from
    // this command.
    uint32_t cmd_flags;
    // The 0-, 32-, or 128-bit response (unused fields set to zero). Set by the interrupt thread and
    // read by the request thread.
    uint32_t response[4];
    // If an error occurred, the interrupt thread sets this field to the value of the status
    // register (and always sets the general error bit). If no error  occurred the interrupt thread
    // sets this field to zero.
    InterruptStatus status;

    bool is_pending() const { return !cmd_done || !data_done; }

    void Reset() {
      cmd_done = true;
      data_done = true;
      cmd_idx = 0;
      cmd_flags = 0;
      memset(response, 0, sizeof(response));
    }
  };

  PendingRequest pending_request_ TA_GUARDED(mtx_);
};

}  // namespace sdhci

#endif  // SRC_DEVICES_BLOCK_DRIVERS_SDHCI_SDHCI_H_
