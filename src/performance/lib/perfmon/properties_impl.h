// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_LIB_PERFMON_PROPERTIES_IMPL_H_
#define SRC_PERFORMANCE_LIB_PERFMON_PROPERTIES_IMPL_H_

#include <fidl/fuchsia.perfmon.cpu/cpp/fidl.h>
#include <lib/zircon-internal/device/cpu-trace/perf-mon.h>

#include "src/performance/lib/perfmon/properties.h"

namespace perfmon::internal {

void FidlToPerfmonProperties(const fuchsia_perfmon_cpu::Properties& props, Properties* out_props);

}  // namespace perfmon::internal

#endif  // SRC_PERFORMANCE_LIB_PERFMON_PROPERTIES_IMPL_H_
