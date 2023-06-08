// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef HID_FT5336_KHADAS_TS050_H_
#define HID_FT5336_KHADAS_TS050_H_

#include <zircon/compiler.h>
#include <zircon/types.h>

__BEGIN_CDECLS

size_t get_ft5336_khadas_ts050_report_desc(const uint8_t** buf);

__END_CDECLS

#endif  // HID_FT5336_KHADAS_TS050_H_
