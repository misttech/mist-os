// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file contains mouse CPI database, not in ChromiumOS's database.
// We may temporary store some models here, then contribute to ChromiumOS's
// database. Then sync back and cleanup.

pub(crate) const MODELS: [(&'static str, &'static str, u32, &'static str); 2] = [
    // Logitech Mouses
    ("046d", "c24c", 4000, "Logitech G400s"),
    ("046d", "c08e", 1600, "Logitech MX518 2018"),
];
