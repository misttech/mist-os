// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_AML_SDMMC_H_
#define SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_AML_SDMMC_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/ddk/phys-iter.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/mmio/mmio.h>
#include <lib/stdcompat/span.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>
#include <threads.h>

#include <array>
#include <limits>
#include <vector>

#include <ddktl/device.h>
#include <fbl/auto_lock.h>
#include <soc/aml-common/aml-sdmmc.h>

#include "src/lib/vmo_store/vmo_store.h"

namespace sdmmc {

class AmlSdmmc;
using AmlSdmmcType = ddk::Device<AmlSdmmc, ddk::Suspendable>;

class AmlSdmmc : public AmlSdmmcType, public ddk::SdmmcProtocol<AmlSdmmc, ddk::base_protocol> {
 public:
  // Limit maximum number of descriptors to 512 for now
  static constexpr size_t kMaxDmaDescriptors = 512;

  AmlSdmmc(zx_device_t* parent, zx::bti bti, fdf::MmioBuffer mmio, aml_sdmmc_config_t config,
           zx::interrupt irq, fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> reset_gpio,
           ddk::IoBuffer descs_buffer)
      : AmlSdmmcType(parent),
        mmio_(std::move(mmio)),
        bti_(std::move(bti)),
        irq_(std::move(irq)),
        board_config_(config),
        descs_buffer_(std::move(descs_buffer)),
        registered_vmos_{
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
        } {
    if (reset_gpio.is_valid()) {
      reset_gpio_.Bind(std::move(reset_gpio));
    }
  }

  virtual ~AmlSdmmc() = default;
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  // Device protocol implementation
  void DdkRelease();
  void DdkSuspend(ddk::SuspendTxn txn);

  // Sdmmc Protocol implementation
  zx_status_t SdmmcHostInfo(sdmmc_host_info_t* out_info);
  zx_status_t SdmmcSetSignalVoltage(sdmmc_voltage_t voltage);
  zx_status_t SdmmcSetBusWidth(sdmmc_bus_width_t bus_width) TA_EXCL(lock_);
  zx_status_t SdmmcSetBusFreq(uint32_t bus_freq) TA_EXCL(lock_);
  zx_status_t SdmmcSetTiming(sdmmc_timing_t timing) TA_EXCL(lock_);
  zx_status_t SdmmcHwReset() TA_EXCL(lock_);
  zx_status_t SdmmcPerformTuning(uint32_t cmd_idx) TA_EXCL(tuning_lock_);
  zx_status_t SdmmcRegisterInBandInterrupt(const in_band_interrupt_protocol_t* interrupt_cb);
  void SdmmcAckInBandInterrupt() {}
  zx_status_t SdmmcRegisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo vmo, uint64_t offset,
                               uint64_t size, uint32_t vmo_rights) TA_EXCL(lock_);
  zx_status_t SdmmcUnregisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo* out_vmo)
      TA_EXCL(lock_);
  zx_status_t SdmmcRequest(const sdmmc_req_t* req, uint32_t out_response[4]) TA_EXCL(lock_);

  // Visible for tests
  zx_status_t Init(const pdev_device_info_t& device_info) TA_EXCL(lock_);
  void set_board_config(const aml_sdmmc_config_t& board_config) { board_config_ = board_config; }

 protected:
  // Visible for tests
  zx_status_t Bind();

  virtual zx_status_t WaitForInterruptImpl();
  virtual void WaitForBus() const;

  zx::vmo GetInspectVmo() const { return inspect_.inspector.DuplicateVmo(); }

 private:
  constexpr static size_t kResponseCount = 4;

  struct TuneResults {
    uint64_t results = 0;

    std::string ToString(const uint32_t param_max) const {
      char string[param_max + 2];
      for (uint32_t i = 0; i <= param_max; i++) {
        string[i] = (results & (1ULL << i)) ? '|' : '-';
      }
      string[param_max + 1] = '\0';
      return string;
    }
  };

  struct TuneWindow {
    uint32_t start = 0;
    uint32_t size = 0;

    uint32_t middle() const { return start + (size / 2); }
  };

  struct TuneSettings {
    uint32_t adj_delay = 0;
    uint32_t delay = 0;
  };

  // VMO metadata that needs to be stored in accordance with the SDMMC protocol.
  struct OwnedVmoInfo {
    uint64_t offset;
    uint64_t size;
    uint32_t rights;
  };

  struct Inspect {
    inspect::Inspector inspector;
    inspect::Node root;
    inspect::UintProperty bus_clock_frequency;
    inspect::UintProperty adj_delay;
    inspect::UintProperty delay_lines;
    std::vector<inspect::Node> tuning_results_nodes;
    std::vector<inspect::StringProperty> tuning_results;
    inspect::UintProperty max_delay;
    inspect::UintProperty longest_window_start;
    inspect::UintProperty longest_window_size;
    inspect::UintProperty longest_window_adj_delay;
    inspect::UintProperty distance_to_failing_point;

    void Init(const pdev_device_info_t& device_info);
  };

  struct TuneContext {
    zx::unowned_vmo vmo;
    cpp20::span<const uint8_t> expected_block;
    uint32_t cmd;
    TuneSettings new_settings;
    TuneSettings original_settings;
  };

  using SdmmcVmoStore = vmo_store::VmoStore<vmo_store::HashTableStorage<uint32_t, OwnedVmoInfo>>;

  aml_sdmmc_desc_t* descs() const TA_REQ(lock_) {
    return static_cast<aml_sdmmc_desc_t*>(descs_buffer_.virt());
  }

  zx_status_t SdmmcRequestLocked(const sdmmc_req_t* req, uint32_t out_response[4]) TA_REQ(lock_);

  uint32_t DistanceToFailingPoint(TuneSettings point,
                                  cpp20::span<const TuneResults> adj_delay_results);
  zx::result<TuneSettings> PerformTuning(cpp20::span<const TuneResults> adj_delay_results);
  zx_status_t TuningDoTransfer(const TuneContext& context) TA_REQ(tuning_lock_);
  bool TuningTestSettings(const TuneContext& context) TA_REQ(tuning_lock_);
  // Sweeps from zero to the max delay and creates a TuneWindow representing the largest span of
  // delay values that failed.
  TuneWindow GetFailingWindow(TuneResults results);
  TuneResults TuneDelayLines(const TuneContext& context) TA_REQ(tuning_lock_);

  void SetTuneSettings(const TuneSettings& settings) TA_REQ(lock_);
  TuneSettings GetTuneSettings() TA_REQ(lock_);

  uint32_t max_delay() const;

  void ConfigureDefaultRegs() TA_REQ(lock_);
  aml_sdmmc_desc_t* SetupCmdDesc(const sdmmc_req_t& req) TA_REQ(lock_);
  // Returns a pointer to the LAST descriptor used.
  zx::result<std::pair<aml_sdmmc_desc_t*, std::vector<fzl::PinnedVmo>>> SetupDataDescs(
      const sdmmc_req_t& req, aml_sdmmc_desc_t* cur_desc) TA_REQ(lock_);
  // These return pointers to the NEXT descriptor to use.
  zx::result<aml_sdmmc_desc_t*> SetupOwnedVmoDescs(const sdmmc_req_t& req,
                                                   const sdmmc_buffer_region_t& buffer,
                                                   vmo_store::StoredVmo<OwnedVmoInfo>& vmo,
                                                   aml_sdmmc_desc_t* cur_desc) TA_REQ(lock_);
  zx::result<std::pair<aml_sdmmc_desc_t*, fzl::PinnedVmo>> SetupUnownedVmoDescs(
      const sdmmc_req_t& req, const sdmmc_buffer_region_t& buffer, aml_sdmmc_desc_t* cur_desc)
      TA_REQ(lock_);
  zx::result<aml_sdmmc_desc_t*> PopulateDescriptors(const sdmmc_req_t& req,
                                                    aml_sdmmc_desc_t* cur_desc,
                                                    fzl::PinnedVmo::Region region) TA_REQ(lock_);
  static zx_status_t FinishReq(const sdmmc_req_t& req);

  void ClearStatus() TA_REQ(lock_);
  zx::result<std::array<uint32_t, kResponseCount>> WaitForInterrupt(const sdmmc_req_t& req)
      TA_REQ(lock_);

  void ShutDown() TA_EXCL(lock_);

  fdf::MmioBuffer mmio_ TA_GUARDED(lock_);

  zx::bti bti_;

  fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> reset_gpio_;
  zx::interrupt irq_;
  aml_sdmmc_config_t board_config_;

  sdmmc_host_info_t dev_info_;
  ddk::IoBuffer descs_buffer_ TA_GUARDED(lock_);
  uint32_t max_freq_, min_freq_;

  fbl::Mutex lock_ TA_ACQ_AFTER(tuning_lock_);
  fbl::Mutex tuning_lock_ TA_ACQ_BEFORE(lock_);
  bool shutdown_ TA_GUARDED(lock_) = false;
  std::array<SdmmcVmoStore, SDMMC_MAX_CLIENT_ID + 1> registered_vmos_ TA_GUARDED(lock_);

  uint64_t consecutive_cmd_errors_ = 0;
  uint64_t consecutive_data_errors_ = 0;

  Inspect inspect_;
};

}  // namespace sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_AML_SDMMC_H_
