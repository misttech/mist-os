// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub(crate) const MODELS: [(&'static str, &'static str, u32, &'static str); 3] = [
    // Logitech Mouses
    ("046d", "c40*", 600, "Logitech Trackballs*"),
    // Apple
    ("05ac", "*", 373, "Apple mice (other)"),
    ("05ac", "0304", 400, "Apple USB Optical Mouse (Mighty Mouse)"),
];
