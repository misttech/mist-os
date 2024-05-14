
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cinttypes>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/uint128.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/uuid.h"

#ifndef SCNx8
#define SCNx8 "hhx"
#endif

namespace bt {
namespace {

// Format string that can be passed to sscanf. This allows sscanf to convert
// each octet into a uint8_t.
constexpr char kScanUuidFormatString[] =
    "%2" SCNx8 "%2" SCNx8 "%2" SCNx8 "%2" SCNx8
    "-"
    "%2" SCNx8 "%2" SCNx8
    "-"
    "%2" SCNx8 "%2" SCNx8
    "-"
    "%2" SCNx8 "%2" SCNx8
    "-"
    "%2" SCNx8 "%2" SCNx8 "%2" SCNx8 "%2" SCNx8 "%2" SCNx8 "%2" SCNx8;

// Parses the contents of a |uuid_string| and returns the result in |out_bytes|.
// Returns false if |uuid_string| does not represent a valid UUID.
// TODO(armansito): After having used UUID in camel-case words all over the
// place, I've decided that it sucks. I'm explicitly naming this using the
// "Uuid" style as a reminder to fix style elsewhere.
bool ParseUuidString(const std::string& uuid_string, UInt128* out_bytes) {
  BT_DEBUG_ASSERT(out_bytes);

  if (uuid_string.length() == 4) {
    // Possibly a 16-bit short UUID, parse it in context of the Base UUID.
    return ParseUuidString(
        "0000" + uuid_string + "-0000-1000-8000-00805F9B34FB", out_bytes);
  }

  // This is a 36 character string, including 4 "-" characters and two
  // characters for each of the 16-octets that form the 128-bit UUID.
  if (uuid_string.length() != 36)
    return false;

  int result = std::sscanf(uuid_string.c_str(),
                           kScanUuidFormatString,
                           out_bytes->data() + 15,
                           out_bytes->data() + 14,
                           out_bytes->data() + 13,
                           out_bytes->data() + 12,
                           out_bytes->data() + 11,
                           out_bytes->data() + 10,
                           out_bytes->data() + 9,
                           out_bytes->data() + 8,
                           out_bytes->data() + 7,
                           out_bytes->data() + 6,
                           out_bytes->data() + 5,
                           out_bytes->data() + 4,
                           out_bytes->data() + 3,
                           out_bytes->data() + 2,
                           out_bytes->data() + 1,
                           out_bytes->data());

  return (result > 0) && (static_cast<size_t>(result) == out_bytes->size());
}

}  // namespace

bool IsStringValidUuid(const std::string& uuid_string) {
  UInt128 bytes;
  return ParseUuidString(uuid_string, &bytes);
}

bool StringToUuid(const std::string& uuid_string, UUID* out_uuid) {
  BT_DEBUG_ASSERT(out_uuid);

  UInt128 bytes;
  if (!ParseUuidString(uuid_string, &bytes)) {
    return false;
  }

  *out_uuid = UUID(bytes);
  return true;
}

}  // namespace bt
