// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DDK_INCLUDE_LIB_DDK_METADATA_H_
#define SRC_LIB_DDK_INCLUDE_LIB_DDK_METADATA_H_

#include <assert.h>
#include <lib/zbi-format/zbi.h>
#include <stdbool.h>
#include <stdint.h>

// This file contains metadata types for device_get_metadata()
//
// Note: if a metadata type is a FIDL type, then it is always
// serialized using the convention for FIDL data persistence, which
// adds wire format metadata in front of the encoded content.

// MAC Address for Ethernet, Wifi, Bluetooth, etc.
// Content: uint8_t[] (variable length based on type of MAC address)
#define DEVICE_METADATA_MAC_ADDRESS 0x43414D6D  // mMAC
static_assert(DEVICE_METADATA_MAC_ADDRESS == ZBI_TYPE_DRV_MAC_ADDRESS, "");

// Partition map for raw block device.
// Content: bootdata_partition_map_t
#define DEVICE_METADATA_PARTITION_MAP 0x5452506D  // mPRT
static_assert(DEVICE_METADATA_PARTITION_MAP == ZBI_TYPE_DRV_PARTITION_MAP, "");

// maximum size of DEVICE_METADATA_PARTITION_MAP data
#define METADATA_PARTITION_MAP_MAX 4096

// Initial USB mode
// type: usb_mode_t
#define DEVICE_METADATA_USB_MODE 0x4D425355  // USBM

#define DEVICE_METADATA_SERIAL_NUMBER 0x4e4c5253  // SRLN
static_assert(DEVICE_METADATA_SERIAL_NUMBER == ZBI_TYPE_SERIAL_NUMBER, "");

// Serial port info
// type: fuchsia.hardware.serial.SerialPortInfo
#define DEVICE_METADATA_SERIAL_PORT_INFO 0x4D524553  // SERM

// Platform board private data (for board driver)
// type: ???
#define DEVICE_METADATA_BOARD_PRIVATE 0x524F426D  // mBOR
static_assert(DEVICE_METADATA_BOARD_PRIVATE == ZBI_TYPE_DRV_BOARD_PRIVATE, "");

// Interrupt controller type (for sysinfo driver)
// type: uint8_t
#define DEVICE_METADATA_INTERRUPT_CONTROLLER_TYPE 0x43544E49  // INTC

// Partition info (for GPT driver)
// type: fuchsia.hardware.gpt.metadata.GptInfo
#define DEVICE_METADATA_GPT_INFO 0x49545047  // GPTI

// Button Metadata
// type: fuchsia.buttons.Metadata
#define DEVICE_METADATA_BUTTONS 0x534E5442  // BTNS

// list of buttons_button_config_t
#define DEVICE_METADATA_BUTTONS_BUTTONS 0x424E5442  // BTNB

// list of buttons_gpio_config_t
#define DEVICE_METADATA_BUTTONS_GPIOS 0x474E5442  // BTNG

// type: fuchsia_hardware_thermal_ThermalDeviceInfo
#define DEVICE_METADATA_THERMAL_CONFIG 0x54485243  // THRC

// type: array of gpio_pin_t
#define DEVICE_METADATA_GPIO_PINS 0x4F495047  // GPIO

// type: FIDL fuchsia.hardware.pinimpl/Metadata
#define DEVICE_METADATA_GPIO_CONTROLLER 0x43495047  // GPIC

// type: FIDL fuchsia.hardware.clockimpl/InitMetadata
#define DEVICE_METADATA_CLOCK_INIT 0x494B4C43  // CLKI

// type: FIDL fuchsia.hardware.power/DomainMetadata
#define DEVICE_METADATA_POWER_DOMAINS 0x52574F50  // POWR

// type: clock_id_t
#define DEVICE_METADATA_CLOCK_IDS 0x4B4F4C43  // CLOK

// type: FIDL fuchsia.hardware.pwm/PwmChannelsMetadata
#define DEVICE_METADATA_PWM_CHANNELS 0x004D5750  // PWM\0

// type: vendor specific Wifi configuration
#define DEVICE_METADATA_WIFI_CONFIG 0x49464957  // WIFI

// type: FIDL fuchsia.hardware.i2c/I2CBusMetadata
#define DEVICE_METADATA_I2C_CHANNELS 0x43433249  // I2CC

// type: FIDL fuchsia.hardware.spi.SpiBusMetadata
#define DEVICE_METADATA_SPI_CHANNELS 0x43495053  // SPIC

// type: display_panel_t (defined in //src/graphics/display/lib/
// device-protocol-display/include/lib/device-protocol/display-panel.h)
#define DEVICE_METADATA_DISPLAY_PANEL_CONFIG 0x43505344  // DSPC

// Maximum screen brightness in nits. Used by the backlight driver.
// type: double
#define DEVICE_METADATA_BACKLIGHT_MAX_BRIGHTNESS_NITS 0x4C4B4342  // BCKL

// type: FIDL fuchsia.hardware.registers/Metadata
#define DEVICE_METADATA_REGISTERS 0x53474552  // REGS

// type: FIDL fuchsia.hardware.vreg/Metadata
#define DEVICE_METADATA_VREG 0x47455256  // VREG

// type: FIDL fuchsia.hardware.tee/TeeMetadata
#define DEVICE_METADATA_TEE_THREAD_CONFIG 0x43454554  // TEEC

// TODO(b/356905181): Remove once no longer referenced.
// type: FIDL fuchsia.hardware.sdmmc/SdmmcMetadata
#define DEVICE_METADATA_SDMMC 0x4D4D4453  // SDMM

// Type: FIDL fuchsia.hardware.trippoint/TripDeviceMetadata
#define DEVICE_METADATA_TRIP 0x50495254  // TRIP

// type: FIDL fuchsia.scheduler/RoleName
#define DEVICE_METADATA_SCHEDULER_ROLE_NAME 0x454C4F52  // ROLE

// Metadata types that have least significant byte set to lowercase 'd'
// signify private driver data.
// This allows creating metadata types to be defined local to a particular
// driver or driver protocol.
#define DEVICE_METADATA_PRIVATE 0x00000064
#define DEVICE_METADATA_PRIVATE_MASK 0x000000ff

static inline bool is_private_metadata(uint32_t type) {
  return ((type & DEVICE_METADATA_PRIVATE_MASK) == DEVICE_METADATA_PRIVATE);
}

#endif  // SRC_LIB_DDK_INCLUDE_LIB_DDK_METADATA_H_
