// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_MIPI_DSI_INCLUDE_LIB_MIPI_DSI_MIPI_DSI_H_
#define SRC_GRAPHICS_DISPLAY_LIB_MIPI_DSI_INCLUDE_LIB_MIPI_DSI_MIPI_DSI_H_

#include <cstddef>
#include <cstdint>

// Some of the constants here are from the MIPI Alliance Specification for
// Display Serial Interface (DSI), which can be obtained from
// https://www.mipi.org/specifications/dsi
//
// mipi_dsi1 is Version 1.3.2, adopted on 23 September 2021.

// TODO(https://fxbug.dev/328078798): Move all the constants below to the
// `mipi_dsi` namespace.

// Assigned Virtual Channel ID
// TODO(payamm): Will need to generate and maintain VCID for multi-display
// solutions
constexpr uint8_t kMipiDsiVirtualChanId = 0;

// Data types for DSI Processor-to-Peripheral packets.
//
// mipi_dsi1 8.7.1 "Processor-sourced Data Type Summary" Table 16 "Data Types
// for Processor-Sourced Packets"

constexpr uint8_t kMipiDsiDtVsyncStart = 0x01;
constexpr uint8_t kMipiDsiDtVsyncEnd = 0x11;
constexpr uint8_t kMipiDsiDtHsyncStart = 0x21;
constexpr uint8_t kMipiDsiDtHsyncEnd = 0x31;
constexpr uint8_t kMipiDsiDtEotp = 0x08;
constexpr uint8_t kMipiDsiDtColorModeOff = 0x02;
constexpr uint8_t kMipiDsiDtColorModeOn = 0x12;
constexpr uint8_t kMipiDsiDtPeriCmdOff = 0x22;
constexpr uint8_t kMipiDsiDtPeriCmdOn = 0x32;
constexpr uint8_t kMipiDsiDtGenShortWrite0 = 0x03;
constexpr uint8_t kMipiDsiDtGenShortWrite1 = 0x13;
constexpr uint8_t kMipiDsiDtGenShortWrite2 = 0x23;
constexpr uint8_t kMipiDsiDtGenShortRead0 = 0x04;
constexpr uint8_t kMipiDsiDtGenShortRead1 = 0x14;
constexpr uint8_t kMipiDsiDtGenShortRead2 = 0x24;
constexpr uint8_t kMipiDsiDtDcsShortWrite0 = 0x05;
constexpr uint8_t kMipiDsiDtDcsShortWrite1 = 0x15;
constexpr uint8_t kMipiDsiDtDcsRead0 = 0x06;
constexpr uint8_t kMipiDsiDtSetMaxRetPkt = 0x37;
constexpr uint8_t kMipiDsiDtNullPkt = 0x09;
constexpr uint8_t kMipiDsiDtBlakingPkt = 0x19;
constexpr uint8_t kMipiDsiDtGenLongWrite = 0x29;
constexpr uint8_t kMipiDsiDtDcsLongWrite = 0x39;
constexpr uint8_t kMipiDsiDtYcbcr42220bit = 0x0C;
constexpr uint8_t kMipiDsiDtYcbcr42224bit = 0x1C;
constexpr uint8_t kMipiDsiDtYcbcr42216bit = 0x2C;
constexpr uint8_t kMipiDsiDtRgb101010 = 0x0D;
constexpr uint8_t kMipiDsiDtRgb121212 = 0x1D;
constexpr uint8_t kMipiDsiDtYcbcr42012bit = 0x3D;
constexpr uint8_t kMipiDsiDtRgb565 = 0x0E;
constexpr uint8_t kMipiDsiDtRgb666 = 0x1E;
constexpr uint8_t kMipiDsiDtRgb666L = 0x2E;
constexpr uint8_t kMipiDsiDtRgb888 = 0x3E;
constexpr uint8_t kMipiDsiDtUnknown = 0xFF;

// Data types for DSI Peripheral-to-Processor (response) packets.
//
// mipi_dsi1 8.10 "Peripheral-to-Processor Transactions – Detailed Format
// Description" Table 22 "Data Types for Peripheral-Sourced Packets"

constexpr uint8_t kMipiDsiRspGenShort1 = 0x11;
constexpr uint8_t kMipiDsiRspGenShort2 = 0x12;
constexpr uint8_t kMipiDsiRspGenLong = 0x1A;
constexpr uint8_t kMipiDsiRspDcsLong = 0x1C;
constexpr uint8_t kMipiDsiRspDcsShort1 = 0x21;
constexpr uint8_t kMipiDsiRspDcsShort2 = 0x22;

namespace mipi_dsi {

// mipi_dsi1 4.2 "Command And Video Modes", page 12.
enum class DsiOperationMode : uint8_t {
  kVideo = 0,
  kCommand = 1,
};

// Packet sequences for Video Mode data transmission.
//
// mipi_dsi1 8.11.1 "Transmission Packet Sequences", pages 76-77.
enum class DsiVideoModePacketSequence : uint8_t {
  // The transmission rate of pixel packets over the DSI serial link matches
  // the DPI pixel transmission rate.
  //
  // Synchronization pulses (vertical syncs and horizontal syncs) are defined
  // using packets transmitting both start and end of sync pulses.
  //
  // mipi_dsi1 8.11.2 "Non-Burst Mode with Sync Pulses", pages 77-78.
  kNonBurstModeWithSyncPulses = 0,

  // The transmission rate of pixel packets over the DSI serial link matches
  // the DPI pixel transmission rate.
  //
  // Synchronization pulses (vertical syncs and horizontal syncs) are defined
  // using only the "start" packets.
  //
  // mipi_dsi1 8.11.3 "Non-Burst Mode with Sync Events", pages 78-79.
  kNonBurstModeWithSyncEvents = 1,

  // Pixel packets are transferred in "bursts" using a time-compressed format
  // for each line, so its transmission rate may be higher than the DPI pixel
  // transmission rate.
  //
  // Synchronization pulses (vertical syncs and horizontal syncs) are defined
  // using only the "start" packets.
  //
  // mipi_dsi1 8.11.4 "Burst Mode", pages 79-80.
  kBurstMode = 2,
};

// Layout and pixel format of a DSI pixel stream packet.
//
// The enum value is the Data Type field (bits 5-0) in the Data Identifier byte
// of the DSI packet.
enum class DsiPacketPixelFormat : uint8_t {
  // mipi_dsi1 8.8.14 "Loosely Packed Pixel Stream, 20-bit YCbCr 4:2:2 Format,
  // Data Type = 00 1100 (0x0C)", pages 55-56.
  k20BitYcbcr422LooselyPacked = 0x0c,

  // mipi_dsi1 8.8.15 "Packed Pixel Stream, 24-bit YCbCr 4:2:2 Format, Data
  // Type = 01 1100 (0x1C)", pages 56-57.
  k24BitYcbcr422Packed = 0x1c,

  // mipi_dsi1 8.8.16 "Packed Pixel Stream, 16-bit YCbCr 4:2:2 Format, Data
  // Type = 10 1100 (0x2C)", pages 57-58.
  k16BitYcbcr422Packed = 0x2c,

  // mipi_dsi1 8.8.17 "Packed Pixel Stream, 30-bit Format, Long Packet, Data
  // Type = 00 1101 (0x0D)", pages 58-59.
  k30BitR10G10B10Packed = 0x0d,

  // mipi_dsi1 8.8.18 "Packed Pixel Stream, 36-bit Format, Long Packet, Data
  // Type = 01 1101 (0x1D)", pages 59-60.
  k36BitR12G12B12Packed = 0x1d,

  // mipi_dsi1 8.8.19 "Packed Pixel Stream, 12-bit YCbCr 4:2:0 Format, Data
  // Type = 11 1101 (0x3D)", pages 60-61.
  k12BitYcbcr420Packed = 0x3d,

  // mipi_dsi1 8.8.20 "Packed Pixel Stream, 16-bit Format, Long Packet, Data
  // Type 00 1110 (0x0E)", pages 61-62.
  k16BitR5G6B5Packed = 0x0e,

  // mipi_dsi1 8.8.21 "Packed Pixel Stream, 18-bit Format, Long Packet, Data
  // Type = 01 1110 (0x1E)", pages 62-63.
  k18BitR6G6B6Packed = 0x1e,

  // mipi_dsi1 8.8.22 "Pixel Stream, 18-bit Format in Three Bytes, Long Packet,
  // Data Type = 10 1110 (0x2E)", pages 63-64.
  k18BitR6G6B6LooselyPacked = 0x2e,

  // mipi_dsi1 8.8.23 "Packed Pixel Stream, 24-bit Format, Long Packet, Data
  // Type = 11 1110 (0x3E)", pages 64-65.
  k24BitR8G8B8Packed = 0x3e,

  // mipi_dsi1 8.8.24 "Compressed Pixel Stream, Long Packet, Data Type =
  // 00 1011 (0x0B)", pages 65-66.
  kCompressed = 0x0b,
};

}  // namespace mipi_dsi

#endif  // SRC_GRAPHICS_DISPLAY_LIB_MIPI_DSI_INCLUDE_LIB_MIPI_DSI_MIPI_DSI_H_
