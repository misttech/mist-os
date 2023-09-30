// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "perf-mon.h"

namespace perfmon {
zx_status_t PerfmonController::InitOnce() { return ZX_ERR_NOT_SUPPORTED; }

// Stub implemetnations for  |PmuStageConfig()| that error out as unsupported
void PerfmonController::InitializeStagingState(StagingState* ss) const {}

zx_status_t PerfmonController::StageFixedConfig(const FidlPerfmonConfig* icfg, StagingState* ss,
                                                size_t input_index, PmuConfig* ocfg) const {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t PerfmonController::StageProgrammableConfig(const FidlPerfmonConfig* icfg,
                                                       StagingState* ss, size_t input_index,
                                                       PmuConfig* ocfg) const {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t PerfmonController::StageMiscConfig(const FidlPerfmonConfig* icfg, StagingState* ss,
                                               size_t input_index, PmuConfig* ocfg) {
  return ZX_ERR_NOT_SUPPORTED;
}

}  // namespace perfmon
