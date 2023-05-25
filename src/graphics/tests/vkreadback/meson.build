# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# Designed for building with Android NDK.
# Builds vkreadback using test framework from //src/graphics/tests/common,
# gtest from //third_party/googletest, and pulls vulkan headers from
# //third_party/Vulkan-Headers.
#

fuchsia_root='../../../..'

sources = [
    'android/main.cc',
    'vkreadback_test.cc',
    'vkreadback.cc',
    '../common/vulkan_context.cc',
    '../common/utils.cc',
    fuchsia_root + '/third_party/googletest/src/googletest/src/gtest-all.cc',
]

inc_gtest = include_directories(
    fuchsia_root + '/third_party/googletest/src/googletest/include',
    fuchsia_root + '/third_party/googletest/src/googletest',
)

inc_vulkan = include_directories(
    fuchsia_root + '/third_party/Vulkan-Headers/include',
)

executable('vkreadback',
    sources,
    include_directories: [inc_top, inc_vulkan, inc_gtest],
    cpp_args: ['-DVULKAN_HPP_NO_EXCEPTIONS=1', '-std=c++17'],
    link_args: [ '-static-libstdc++', '-lvulkan' ],
)
