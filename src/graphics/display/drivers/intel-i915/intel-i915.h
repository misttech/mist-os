// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_INTEL_I915_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_INTEL_I915_H_

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <fuchsia/hardware/intelgpucore/cpp/banjo.h>
#include <fuchsia/hardware/sysmem/c/banjo.h>
#include <lib/device-protocol/pci.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/mmio/mmio.h>
#include <lib/stdcompat/span.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zx/channel.h>
#include <threads.h>

#include <memory>
#include <optional>

#include <fbl/vector.h>

#include "src/graphics/display/drivers/intel-i915/clock/cdclk.h"
#include "src/graphics/display/drivers/intel-i915/ddi-physical-layer.h"
#include "src/graphics/display/drivers/intel-i915/display-device.h"
#include "src/graphics/display/drivers/intel-i915/dp-display.h"
#include "src/graphics/display/drivers/intel-i915/dpll.h"
#include "src/graphics/display/drivers/intel-i915/gtt.h"
#include "src/graphics/display/drivers/intel-i915/hdmi-display.h"
#include "src/graphics/display/drivers/intel-i915/i2c/gmbus-i2c.h"
#include "src/graphics/display/drivers/intel-i915/igd.h"
#include "src/graphics/display/drivers/intel-i915/interrupts.h"
#include "src/graphics/display/drivers/intel-i915/pipe-manager.h"
#include "src/graphics/display/drivers/intel-i915/pipe.h"
#include "src/graphics/display/drivers/intel-i915/power.h"
#include "src/graphics/display/drivers/intel-i915/registers-ddi.h"
#include "src/graphics/display/drivers/intel-i915/registers-dpll.h"
#include "src/graphics/display/drivers/intel-i915/registers-pipe.h"
#include "src/graphics/display/drivers/intel-i915/registers-transcoder.h"
#include "src/graphics/display/drivers/intel-i915/registers.h"
#include "src/graphics/display/lib/api-types-cpp/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/driver-buffer-collection-id.h"

namespace i915 {

typedef struct buffer_allocation {
  uint16_t start;
  uint16_t end;
} buffer_allocation_t;

class Controller;
using DeviceType = ddk::Device<Controller, ddk::Initializable, ddk::Unbindable, ddk::Suspendable,
                               ddk::Resumable, ddk::ChildPreReleaseable>;

class Controller : public DeviceType,
                   public ddk::DisplayControllerImplProtocol<Controller, ddk::base_protocol>,
                   public ddk::IntelGpuCoreProtocol<Controller> {
 public:
  explicit Controller(zx_device_t* parent);
  ~Controller();

  // Perform short-running initialization of all subcomponents and instruct the DDK to publish the
  // device. On success, returns ZX_OK and the owernship of the Controller instance is claimed by
  // the DDK.
  //
  // Long-running initialization is performed in the DdkInit hook.
  static zx_status_t Create(zx_device_t* parent);

  // DDK ops
  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();
  void DdkSuspend(ddk::SuspendTxn txn);
  void DdkResume(ddk::ResumeTxn txn);
  void DdkChildPreRelease(void* child_ctx) {
    fbl::AutoLock lock(&display_lock_);
    if (dc_intf_.is_valid()) {
      display_controller_interface_protocol_t proto;
      dc_intf_.GetProto(&proto);
      if (proto.ctx == child_ctx) {
        dc_intf_ = ddk::DisplayControllerInterfaceProtocolClient();
      }
    }
  }

  // display controller protocol ops
  void DisplayControllerImplSetDisplayControllerInterface(
      const display_controller_interface_protocol* intf);
  zx_status_t DisplayControllerImplSetDisplayCaptureInterface(
      const display_capture_interface_protocol_t* intf) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplImportBufferCollection(
      uint64_t banjo_driver_buffer_collection_id, zx::channel collection_token);
  zx_status_t DisplayControllerImplReleaseBufferCollection(
      uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplImportImage(image_t* image,
                                               uint64_t banjo_driver_buffer_collection_id,
                                               uint32_t index);
  zx_status_t DisplayControllerImplImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                         uint32_t index,
                                                         uint64_t* out_capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  void DisplayControllerImplReleaseImage(image_t* image);
  config_check_result_t DisplayControllerImplCheckConfiguration(
      const display_config_t** banjo_display_configs, size_t display_config_count,
      uint32_t** layer_cfg_result, size_t* layer_cfg_result_count);
  void DisplayControllerImplApplyConfiguration(const display_config_t** banjo_display_configs,
                                               size_t display_config_count,
                                               const config_stamp_t* banjo_config_stamp);
  void DisplayControllerImplSetEld(uint64_t banjo_display_id, const uint8_t* raw_eld_list,
                                   size_t raw_eld_count);
  zx_status_t DisplayControllerImplGetSysmemConnection(zx::channel connection);
  zx_status_t DisplayControllerImplSetBufferCollectionConstraints(
      const image_t* config, uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t banjo_display_id, bool power_on) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  bool DisplayControllerImplIsCaptureCompleted() { return false; }

  // gpu core ops
  zx_status_t IntelGpuCoreReadPciConfig16(uint16_t addr, uint16_t* value_out);
  zx_status_t IntelGpuCoreMapPciMmio(uint32_t pci_bar, uint8_t** addr_out, uint64_t* size_out);
  zx_status_t IntelGpuCoreUnmapPciMmio(uint32_t pci_bar);
  zx_status_t IntelGpuCoreGetPciBti(uint32_t index, zx::bti* bti_out);
  zx_status_t IntelGpuCoreRegisterInterruptCallback(const intel_gpu_core_interrupt_t* callback,
                                                    uint32_t interrupt_mask);
  zx_status_t IntelGpuCoreUnregisterInterruptCallback();
  uint64_t IntelGpuCoreGttGetSize();
  zx_status_t IntelGpuCoreGttAlloc(uint64_t page_count, uint64_t* addr_out);
  zx_status_t IntelGpuCoreGttFree(uint64_t addr);
  zx_status_t IntelGpuCoreGttClear(uint64_t addr);
  zx_status_t IntelGpuCoreGttInsert(uint64_t addr, zx::vmo buffer, uint64_t page_offset,
                                    uint64_t page_count);
  void GpuRelease();

  fdf::MmioBuffer* mmio_space() { return mmio_space_.has_value() ? &*mmio_space_ : nullptr; }
  Interrupts* interrupts() { return &interrupts_; }
  uint16_t device_id() const { return device_id_; }
  const IgdOpRegion& igd_opregion() const { return igd_opregion_; }
  Power* power() { return power_.get(); }
  PipeManager* pipe_manager() { return pipe_manager_.get(); }
  DisplayPllManager* dpll_manager() { return dpll_manager_.get(); }

  // Non-const getter to allow unit tests to modify the IGD.
  // TODO(fxbug.dev/83998): Consider making a fake IGD object injectable as allowing mutable access
  // to internal state that is intended to be externally immutable can be source of bugs if used
  // incorrectly. The various "ForTesting" methods are a typical anti-pattern that exposes internal
  // state and makes the class state machine harder to reason about.
  IgdOpRegion* igd_opregion_for_testing() { return &igd_opregion_; }

  void HandleHotplug(DdiId ddi_id, bool long_pulse);
  void HandlePipeVsync(PipeId pipe_id_num, zx_time_t timestamp);

  void ResetPipePlaneBuffers(PipeId pipe_id);
  bool ResetDdi(DdiId ddi_id, std::optional<TranscoderId> transcoder_id);

  void SetDpllManagerForTesting(std::unique_ptr<DisplayPllManager> dpll_manager) {
    dpll_manager_ = std::move(dpll_manager);
  }
  void SetPipeManagerForTesting(std::unique_ptr<PipeManager> pipe_manager) {
    pipe_manager_ = std::move(pipe_manager);
  }
  void SetPowerWellForTesting(std::unique_ptr<Power> power_well) { power_ = std::move(power_well); }
  void SetMmioForTesting(fdf::MmioBuffer mmio_space) { mmio_space_ = std::move(mmio_space); }

  void ResetMmioSpaceForTesting() { mmio_space_.reset(); }

  zx_status_t SetAndInitSysmemForTesting(
      fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem) {
    sysmem_ = std::move(sysmem);
    return InitSysmemAllocatorClient();
  }

  zx_status_t InitGttForTesting(const ddk::Pci& pci, fdf::MmioBuffer buffer, uint32_t fb_offset);

  // For every frame, in order to use the imported image, it is required to set
  // up the image based on given rotation in GTT and use the handle offset in
  // GTT. Returns the Gtt region representing the image.
  const GttRegion& SetupGttImage(const image_t* image, uint32_t rotation);

  // Returns the pixel format negotiated by sysmem for an imported `image`.
  // `image` must be successfully imported and not yet released.
  PixelFormatAndModifier GetImportedImagePixelFormat(const image_t* image) const;

 private:
  // Perform short-running initialization of all subcomponents and instruct the DDK to publish the
  // device. On success, returns ZX_OK and the ownership of the Controller instance is claimed by
  // the DDK.
  //
  // Long-running initialization is performed in the DdkInit hook.
  zx_status_t Init();

  // Initializes the sysmem Allocator client used to import incoming buffer
  // collection tokens.
  //
  // On success, returns ZX_OK and the sysmem allocator client will be open
  // until the device is released.
  zx_status_t InitSysmemAllocatorClient();

  const std::unique_ptr<GttRegionImpl>& GetGttRegionImpl(uint64_t handle);
  void InitDisplays();

  // Reads the memory latency information needed to confiugre pipes and planes.
  //
  // Returns false if a catastrophic error occurred, and pipes and planes cannot
  // be safely configured.
  bool ReadMemoryLatencyInfo();

  // Disables the PCU (power controller)'s automated voltage adjustments.
  void DisableSystemAgentGeyserville();

  std::unique_ptr<DisplayDevice> QueryDisplay(DdiId ddi_id, display::DisplayId display_id)
      __TA_REQUIRES(display_lock_);
  bool LoadHardwareState(DdiId ddi_id, DisplayDevice* device) __TA_REQUIRES(display_lock_);
  zx_status_t AddDisplay(std::unique_ptr<DisplayDevice> display) __TA_REQUIRES(display_lock_);
  void RemoveDisplay(std::unique_ptr<DisplayDevice> display) __TA_REQUIRES(display_lock_);
  bool BringUpDisplayEngine(bool resume) __TA_REQUIRES(display_lock_);
  void InitDisplayBuffers();
  DisplayDevice* FindDevice(display::DisplayId display_id) __TA_REQUIRES(display_lock_);

  void CallOnDisplaysChanged(cpp20::span<DisplayDevice*> added,
                             cpp20::span<const display::DisplayId> removed)
      __TA_REQUIRES(display_lock_);

  // Gets the layer_t* config for the given pipe/plane. Return false if there is no layer.
  bool GetPlaneLayer(Pipe* pipe, uint32_t plane,
                     cpp20::span<const display_config_t*> banjo_display_configs,
                     const layer_t** layer_out) __TA_REQUIRES(display_lock_);
  uint16_t CalculateBuffersPerPipe(size_t active_pipe_count);
  // Returns false if no allocation is possible. When that happens,
  // plane 0 of the failing displays will be set to UINT16_MAX.
  bool CalculateMinimumAllocations(
      cpp20::span<const display_config_t*> banjo_display_configs,
      uint16_t min_allocs[PipeIds<registers::Platform::kKabyLake>().size()]
                         [registers::kImagePlaneCount]) __TA_REQUIRES(display_lock_);
  // Updates plane_buffers_ based pipe_buffers_ and the given parameters
  void UpdateAllocations(
      const uint16_t min_allocs[PipeIds<registers::Platform::kKabyLake>().size()]
                               [registers::kImagePlaneCount],
      const uint64_t display_rate[PipeIds<registers::Platform::kKabyLake>().size()]
                                 [registers::kImagePlaneCount]) __TA_REQUIRES(display_lock_);
  // Reallocates the pipe buffers when a pipe comes online/goes offline. This is a
  // long-running operation, as shifting allocations between pipes requires waiting
  // for vsync.
  void DoPipeBufferReallocation(
      buffer_allocation_t active_allocation[PipeIds<registers::Platform::kKabyLake>().size()])
      __TA_REQUIRES(display_lock_);
  // Reallocates plane buffers based on the given layer config.
  void ReallocatePlaneBuffers(cpp20::span<const display_config_t*> banjo_display_configs,
                              bool reallocate_pipes) __TA_REQUIRES(display_lock_);

  // Validates that a basic layer configuration can be supported for the
  // given modes of the displays.
  bool CheckDisplayLimits(cpp20::span<const display_config_t*> banjo_display_configs,
                          uint32_t** layer_cfg_results) __TA_REQUIRES(display_lock_);

  bool CalculatePipeAllocation(cpp20::span<const display_config_t*> banjo_display_configs,
                               cpp20::span<display::DisplayId> display_allocated_to_pipe)
      __TA_REQUIRES(display_lock_);

  // The number of DBUF (Data Buffer) blocks that can be allocated to planes.
  //
  // This number depends on the display engine and the number of DBUF slices
  // that are powered up.
  uint16_t DataBufferBlockCount() const;

  zx_device_t* zx_gpu_dev_ = nullptr;
  zx_device_t* display_controller_dev_ = nullptr;
  bool gpu_released_ = false;
  bool display_released_ = false;

  fidl::WireSyncClient<fuchsia_hardware_sysmem::Sysmem> sysmem_;

  // The sysmem allocator client used to bind incoming buffer collection tokens.
  fidl::WireSyncClient<fuchsia_sysmem::Allocator> sysmem_allocator_client_;

  // Imported sysmem buffer collections.
  std::unordered_map<display::DriverBufferCollectionId,
                     fidl::WireSyncClient<fuchsia_sysmem::BufferCollection>>
      buffer_collections_;

  ddk::DisplayControllerInterfaceProtocolClient dc_intf_ __TA_GUARDED(display_lock_);
  bool ready_for_callback_ __TA_GUARDED(display_lock_) = false;

  Gtt gtt_ __TA_GUARDED(gtt_lock_);
  mutable mtx_t gtt_lock_;
  // These regions' VMOs are not owned
  fbl::Vector<std::unique_ptr<GttRegionImpl>> imported_images_ __TA_GUARDED(gtt_lock_);
  // These regions' VMOs are owned
  fbl::Vector<std::unique_ptr<GttRegionImpl>> imported_gtt_regions_ __TA_GUARDED(gtt_lock_);

  // Pixel formats of imported images.
  std::unordered_map</*handle*/ uint64_t, PixelFormatAndModifier> imported_image_pixel_formats_
      __TA_GUARDED(gtt_lock_);

  IgdOpRegion igd_opregion_;  // Read only, no locking
  Interrupts interrupts_;     // Internal locking

  ddk::Pci pci_;
  struct {
    mmio_buffer_t mmio;
    int32_t count = 0;
  } mapped_bars_[fuchsia_hardware_pci::wire::kMaxBarCount] __TA_GUARDED(bar_lock_);
  mtx_t bar_lock_;
  // The mmio_space_ is read only. The internal registers are guarded by various locks where
  // appropriate.
  std::optional<fdf::MmioBuffer> mmio_space_;

  std::optional<PchEngine> pch_engine_;
  std::unique_ptr<Power> power_;

  // References to displays. References are owned by devmgr, but will always
  // be valid while they are in this vector.
  fbl::Vector<std::unique_ptr<DisplayDevice>> display_devices_ __TA_GUARDED(display_lock_);
  // Display ID can't be kInvalidDisplayId.
  display::DisplayId next_id_ __TA_GUARDED(display_lock_) = display::DisplayId{1};
  mtx_t display_lock_;

  std::unique_ptr<DdiManager> ddi_manager_;
  std::unique_ptr<PipeManager> pipe_manager_;

  PowerWellRef cd_clk_power_well_;
  std::unique_ptr<CoreDisplayClock> cd_clk_;

  std::unique_ptr<DisplayPllManager> dpll_manager_;

  cpp20::span<const DdiId> ddis_;
  fbl::Vector<GMBusI2c> gmbus_i2cs_;
  fbl::Vector<DpAux> dp_auxs_;

  // Plane buffer allocation. If no alloc, start == end == registers::PlaneBufCfg::kBufferCount.
  buffer_allocation_t plane_buffers_[PipeIds<registers::Platform::kKabyLake>().size()]
                                    [registers::kImagePlaneCount] __TA_GUARDED(
                                        plane_buffers_lock_) = {};
  mtx_t plane_buffers_lock_;

  // Buffer allocations for pipes
  buffer_allocation_t pipe_buffers_[PipeIds<registers::Platform::kKabyLake>().size()] __TA_GUARDED(
      display_lock_) = {};
  bool initial_alloc_ = true;

  uint16_t device_id_;
  uint32_t flags_;

  // Various configuration values set by the BIOS which need to be carried across suspend.
  bool ddi_e_disabled_ = true;

  std::optional<display::DisplayId> eld_display_id_;

  // Debug
  inspect::Inspector inspector_;
  inspect::Node root_node_;
};

}  // namespace i915

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_INTEL_I915_H_
