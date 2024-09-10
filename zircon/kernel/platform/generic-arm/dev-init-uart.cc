// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/uart/all.h>
#include <lib/uart/null.h>
#include <lib/zbi-format/driver-config.h>

#include <dev/init.h>
#include <ktl/variant.h>

#include <ktl/enforce.h>

void PlatformUartDriverHandoffEarly(const uart::all::Driver& serial) {}

void PlatformUartDriverHandoffLate(const uart::all::Driver& serial) {}
