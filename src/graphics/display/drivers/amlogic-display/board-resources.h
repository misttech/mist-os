// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_BOARD_RESOURCES_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_BOARD_RESOURCES_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/bti.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/result.h>

#include <cstdint>
#include <string_view>

// The *ResourceIndex scoped enums define the interface between the board driver
// and the display driver.

namespace amlogic_display {

struct BoardInfo {
  // Values are defined in <lib/ddk/platform-defs.h>.
  uint32_t board_vendor_id;

  // Values are defined in <lib/ddk/platform-defs.h>.
  uint32_t board_product_id;
};

// Typesafe wrapper for [`fuchsia.hardware.platform.device/Device.GetBoardInfo`].
//
// `platform_device` must be valid.
//
// If the result is successful, the fields in BoardInfo are guaranteed to be
// valid.
zx::result<BoardInfo> GetBoardInfo(
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device);

// The resource ordering in the board driver's `display_mmios` table.
enum class MmioResourceIndex : uint8_t {
  kVpu = 0,                // VPU (Video Processing Unit)
  kDsiTop = 1,             // TOP_MIPI_DSI (DSI "top" host controller integration)
  kDsiPhy = 2,             // DSI_PHY
  kDsiHostController = 3,  // DesignWare Cores MIPI DSI Host Controller IP block
  kHhi = 4,                // HIU (Host Interface Unit) / HHI
  kAonRti = 5,             // RTI / AO_RTI / AOBUS_RTI
  kEeReset = 6,            // RESET
  kGpioMux = 7,            // PERIPHS_REGS (GPIO Multiplexing)
  kHdmiTxController = 8,   // HDMITX (HDMI Transmitter Controller IP)
  kHdmiTxTop = 9,          // HDMITX (HDMI Transmitter Top-Level)
};

// Typesafe wrapper for [`fuchsia.hardware.platform.device/Device.GetMmioById`].
//
// `platform_device` must be valid.
//
// If the result is successful, the MmioBuffer is guaranteed to be valid.
zx::result<fdf::MmioBuffer> MapMmio(
    MmioResourceIndex mmio_index,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device);

// The interrupt names are defined in the board driver's `display_irqs` table,
// or in the board devicetree's display node.

// VSync started on VIU1.
constexpr std::string_view kInterruptNameViu1Vsync = "viu1-vsync";

// RDMA transfer done.
constexpr std::string_view kInterruptNameRdmaDone = "rdma-done";

// Display capture done on VDIN1.
constexpr std::string_view kInterruptNameVdin1WriteDone = "vdin1-write-done";

// Typesafe wrapper for [`fuchsia.hardware.platform.device/Device.GetInterruptByName`].
//
// `platform_device` must be valid.
//
// If the result is successful, the zx::interrupt is guaranteed to be valid.
zx::result<zx::interrupt> GetInterrupt(
    std::string_view interrupt_name,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device);

// The resource ordering in the board driver's `display_btis` table.
enum class BtiResourceIndex : uint8_t {
  kDma = 0,  // BTI used for CANVAS / DMA transfers.
};

// Typesafe wrapper for [`fuchsia.hardware.platform.device/Device.GetBtiById`].
//
// `platform_device` must be valid.
//
// If the result is successful, the zx::bti is guaranteed to be valid.
zx::result<zx::bti> GetBti(
    BtiResourceIndex bti_index,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device);

// The resource ordering in the board driver's `kDisplaySmcs` table.
enum class SecureMonitorCallResourceIndex : uint8_t {
  kSiliconProvider = 0,  // SMC used to initialize HDCP.
};

// Typesafe wrapper for [`fuchsia.hardware.platform.device/Device.GetSmcById`].
//
// `platform_device` must be valid.
//
// If the result is successful, the zx::resource is guaranteed to be valid and
// represent a Secure Monitor Call.
zx::result<zx::resource> GetSecureMonitorCall(
    SecureMonitorCallResourceIndex secure_monitor_call_index,
    fidl::UnownedClientEnd<fuchsia_hardware_platform_device::Device> platform_device);

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_BOARD_RESOURCES_H_
