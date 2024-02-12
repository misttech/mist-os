// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Used by the `schema!` macro to refer to this crate through `::ffx_validation`.
#[allow(unused_extern_crates)]
extern crate self as ffx_validation;

pub mod schema;
pub use ffx_validation_proc_macro::schema;
pub mod validate;

pub use schema::Schema;

#[track_caller]
pub fn validation_test<
    T: schema::Schema,
    S: for<'a> serde::Deserialize<'a> + serde::Serialize + PartialEq + std::fmt::Debug,
>(
    examples: &[serde_json::Value],
) {
    let mut fail = false;

    for example in examples {
        println!("Example: {example}");

        if let Err(_) = validate::validate(T::TYPE, example) {
            eprintln!("/!\\ Example failed schema validation");
            fail = true;
        }

        match S::deserialize(example) {
            Ok(deser) => {
                println!("Deserialized: {deser:?}");

                match serde_json::to_value(&deser) {
                    Ok(serde_val) => {
                        println!("Reserialized: {serde_val}");

                        println!("Comparing serialized outputs");
                        if let Err(_) =
                            validate::compare::ValidationTestDiff::check(example, &serde_val)
                        {
                            eprintln!("/!\\ Serialization output is different");
                            fail = true;
                        }

                        if let Err(_) = validate::validate(T::TYPE, &serde_val) {
                            eprintln!("/!\\ Serialization output failed schema validation");
                            fail = true;
                        }

                        match S::deserialize(&serde_val) {
                            Ok(deser_from_ser) => {
                                println!("Deserialized reserialized: {deser_from_ser:?}");

                                if deser != deser_from_ser {
                                    eprintln!("/!\\ Deserialized serialization output is different:\n\t{serde_val}");
                                    fail = true;
                                }
                            }
                            Err(err) => {
                                eprintln!(
                                    "/!\\ Could not deserialize reserialized example:\n\t{err}"
                                );
                                fail = true;
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("/!\\ Could not serialize example:\n\t{err}");
                        fail = true;
                    }
                }
            }
            Err(err) => {
                eprintln!("/!\\ Could not deserialize example:\n\t{err}");
                fail = true;
            }
        }
        println!("\n=======\n");
    }

    if fail {
        panic!("Validation test failed");
    }
}
