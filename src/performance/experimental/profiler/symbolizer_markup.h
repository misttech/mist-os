// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SYMBOLIZER_MARKUP_H_
#define SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SYMBOLIZER_MARKUP_H_

#include "sampler.h"
#include "symbolization_context.h"

namespace profiler::symbolizer_markup {

const std::string kReset = "{{{reset}}}\n";
std::string FormatModule(const Module& mod);
std::string FormatSample(const Sample& sample);

}  // namespace profiler::symbolizer_markup
#endif  // SRC_PERFORMANCE_EXPERIMENTAL_PROFILER_SYMBOLIZER_MARKUP_H_
