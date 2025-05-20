#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# TODO(b/416339669): Now that the old module has been renamed, this file is
# needed to make imports from the internal repo work without modification.
# Remove this file once internal repo has transitioned to use the new location
# and naming for this module.

from power import gonk, monsoon, sampler

# Classes
PowerSampler = sampler.PowerSampler
PowerSamplerConfig = sampler.PowerSamplerConfig

# Functions
create_power_sampler = monsoon.create_power_sampler
merge_gonk_data = gonk.merge_gonk_data
merge_power_data = monsoon.merge_power_data
read_gonk_header = gonk.read_gonk_header
read_gonk_samples = gonk.read_gonk_samples
