// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <phys/zircon-info-note.h>

#include "zircon-info-test.h"

ZIRCON_INFO_NOTE ZirconInfoNote<ZirconInfoTest{.x = 17, .y = 23}> info_note;
