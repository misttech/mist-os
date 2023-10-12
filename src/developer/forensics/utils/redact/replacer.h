// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_UTILS_REDACT_REPLACER_H_
#define SRC_DEVELOPER_FORENSICS_UTILS_REDACT_REPLACER_H_

#include <lib/fit/function.h>

#include <string>
#include <string_view>

#include "src/developer/forensics/utils/redact/cache.h"

namespace forensics {

// A Replacer is an invocable object that replaces substrings in |text| and returns |text|.
using Replacer = ::fit::function<std::string&(RedactionIdCache&, std::string& text)>;

// Constructs a Replacer that substitutes all instances of |pattern| with |replacement|.
Replacer ReplaceWithText(std::string_view pattern, std::string_view replacement);

// Constructs a Replacer that substitutes all instances of |pattern| with |format| and the id for
// the matched pattern.
//
// Note: |pattern| must extract at least 1 value.
// Note: |format| must contain exactly 1 integer format specifier, i.e. '%d'.
Replacer ReplaceWithIdFormatString(std::string_view pattern, std::string_view format);

// Constructs a Replacer that substitutes all instances IPv4 address with "<REDACTED-IPV4: %d>"
Replacer ReplaceIPv4();

// Constructs a Replacer that substitutes all instances IPv6 address with some variation of
// "<REDACTED-IPV6: %d>"
Replacer ReplaceIPv6();

// Constructs a Replacer that substitutes all instances of MAC address with a string like
// "<REDACTED-MAC: %d>"
Replacer ReplaceMac();

// Constructs a Replacer that substitutes all instances of SSIDs with a string like
// "<REDACTED-SSID: %d>"
Replacer ReplaceSsid();

namespace mac_utils {

// Returns a sub-string view of the first three bytes of |mac|, including the delimiters
// that follow each byte.
//
// This function assumes |mac| is a proper representation of a MAC address. Undefined
// behavior will result if passed any other string.
std::string GetOuiPrefix(const std::string& mac);

// Constructs a MAC address equivalent to |mac| but in the canonical form with
// exactly two digits per bytes and colons as delimiters.
//
// This function assumes |mac| is a proper representation of a MAC address. Undefined
// behavior will result if passed any other string.
std::string CanonicalizeMac(const std::string& original_mac);

}  // namespace mac_utils

}  // namespace forensics

#endif  // SRC_DEVELOPER_FORENSICS_UTILS_REDACT_REPLACER_H_
