// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fidl_fuchsia_examples as fex;

#[test]
fn persist_unpersist() -> Result<(), fidl::Error> {
    // [START persist]
    let mut original_value = fex::Color { id: 0, name: "red".to_string() };
    let bytes = fidl::encoding::persist(&mut original_value)?;
    // [END persist]

    // [START unpersist]
    let decoded_value = fidl::encoding::unpersist::<fex::Color>(&bytes)?;
    assert_eq!(original_value, decoded_value);
    // [END unpersist]

    Ok(())
}

#[test]
fn standalone_encode_decode() -> Result<(), fidl::Error> {
    // [START standalone_encode]
    let original_value = fex::JsonValue::StringValue("hello".to_string());
    let (bytes, handle_dispositions, wire_metadata) =
        fidl::encoding::standalone_encode::<fex::JsonValue>(&original_value)?;
    // [END standalone_encode]

    // [START standalone_decode]
    let mut handle_infos =
        fidl::encoding::convert_handle_dispositions_to_infos(handle_dispositions)?;
    let decoded_value = fidl::encoding::standalone_decode::<fex::JsonValue>(
        &bytes,
        &mut handle_infos,
        &wire_metadata,
    )?;
    assert_eq!(original_value, decoded_value);
    // [END standalone_decode]

    Ok(())
}
