// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_tee::{self as ftee, Buffer, Direction, ParameterUnknown, Value};
use fuchsia_zircon::{self as zx};
use tee_internal_impl::binding::{
    TEE_Param, TEE_Param__bindgen_ty_1, TEE_Param__bindgen_ty_2, TEE_PARAM_TYPE_MEMREF_INOUT,
    TEE_PARAM_TYPE_MEMREF_INPUT, TEE_PARAM_TYPE_MEMREF_OUTPUT, TEE_PARAM_TYPE_NONE,
    TEE_PARAM_TYPE_VALUE_INOUT, TEE_PARAM_TYPE_VALUE_INPUT, TEE_PARAM_TYPE_VALUE_OUTPUT,
};

struct Memref {
    direction: ftee::Direction,
    vmo: Option<zx::Vmo>,
    offset: Option<u64>,
    original_size: usize,
    mapped_address: usize,
    mapped_length: usize,
}

enum ParamInfo {
    None,
    Memref(Memref),
    Value(ftee::Direction),
}

// ParamAdapter has logic for translating parameters for a trusted application from the
// FIDL format to the in-memory format used by the TA and back.
// Translating between the FIDL and in-memory representations of buffer requires
// memory mapping operations and is stateful. This type stores this state and cleans
// it up when dropped.
pub struct ParamAdapter {
    tee_params: [TEE_Param; 4],
    infos: [ParamInfo; 4],
}

fn param_type(parameter: &ftee::Parameter) -> u32 {
    match parameter {
        ftee::Parameter::None(_) => TEE_PARAM_TYPE_NONE,
        ftee::Parameter::Buffer(buffer) => match buffer.direction {
            Some(Direction::Input) => TEE_PARAM_TYPE_MEMREF_INPUT,
            Some(Direction::Output) => TEE_PARAM_TYPE_MEMREF_OUTPUT,
            Some(Direction::Inout) => TEE_PARAM_TYPE_MEMREF_INOUT,
            None => TEE_PARAM_TYPE_NONE,
        },
        ftee::Parameter::Value(value) => match value.direction {
            Some(Direction::Input) => TEE_PARAM_TYPE_VALUE_INPUT,
            Some(Direction::Output) => TEE_PARAM_TYPE_VALUE_OUTPUT,
            Some(Direction::Inout) => TEE_PARAM_TYPE_VALUE_INOUT,
            None => TEE_PARAM_TYPE_NONE,
        },
        ftee::ParameterUnknown!() => TEE_PARAM_TYPE_NONE,
    }
}

fn empty_tee_param() -> TEE_Param {
    TEE_Param { value: TEE_Param__bindgen_ty_2 { a: 0, b: 0 } }
}

fn import_value_from_fidl(value: Value) -> Result<(TEE_Param, ParamInfo), Error> {
    match value.direction {
        Some(direction) => {
            let tee_param = match direction {
                Direction::Input | Direction::Inout => TEE_Param {
                    value: TEE_Param__bindgen_ty_2 {
                        a: value.a.unwrap_or_default() as u32,
                        b: value.b.unwrap_or_default() as u32,
                    },
                },
                Direction::Output => empty_tee_param(),
            };
            Ok((tee_param, ParamInfo::Value(direction)))
        }
        None => anyhow::bail!("Invalid direction"),
    }
}

fn import_buffer_from_fidl(buffer: Buffer) -> Result<(TEE_Param, ParamInfo), Error> {
    match buffer.direction {
        Some(direction) => {
            match direction {
                Direction::Input | Direction::Inout => {
                    let vmo = buffer.vmo;
                    let mapped_length = match buffer.size {
                        None => 0,
                        Some(size) => size as usize,
                    };
                    let mapping_offset = match buffer.offset {
                        None => anyhow::bail!("Missing offset"),
                        Some(offset) => offset,
                    };
                    let (vmo, mapped_address, mapped_length) = if let Some(vmo) = vmo {
                        let flags = match direction {
                            Direction::Input => zx::VmarFlags::PERM_READ,
                            Direction::Inout => {
                                zx::VmarFlags::PERM_READ | zx::VmarFlags::PERM_WRITE
                            }
                            _ => anyhow::bail!("Invalid direction"),
                        };
                        let addr = fuchsia_runtime::vmar_root_self()
                            .map(0, &vmo, mapping_offset, mapped_length, flags)
                            .map_err(|e| anyhow::anyhow!("Unable to map VMO: {e:?}"))?;

                        (Some(vmo), addr, mapped_length)
                    } else {
                        // The VMO can be omitted meaning that no memory is
                        // mapped. This is useful for asking the TA to compute
                        // the size of a buffer needed for an operation. In this
                        // case, the null address is provided as the buffer
                        // address.
                        (None, 0, 0)
                    };
                    return Ok((
                        TEE_Param {
                            memref: TEE_Param__bindgen_ty_1 {
                                buffer: mapped_address as *mut std::os::raw::c_void,
                                size: mapped_length,
                            },
                        },
                        ParamInfo::Memref(Memref {
                            direction,
                            vmo,
                            offset: Some(mapping_offset),
                            original_size: mapped_length,
                            mapped_address,
                            mapped_length,
                        }),
                    ));
                }
                Direction::Output => {
                    return Ok((
                        empty_tee_param(),
                        ParamInfo::Memref(Memref {
                            direction,
                            vmo: None,
                            offset: None,
                            original_size: 0,
                            mapped_address: 0,
                            mapped_length: 0,
                        }),
                    ))
                }
            }
        }
        None => anyhow::bail!("Invalid direction"),
    }
}

impl ParamAdapter {
    pub fn from_fidl(parameters: Vec<ftee::Parameter>) -> Result<(Self, u32), Error> {
        if parameters.len() > 4 {
            anyhow::bail!("Expected <= 4 parameters in set but got {}", parameters.len());
        }

        let mut param_types = 0;
        // 4.3.6.2 Initial Content of params Argument specifies that unset values must be filled with zeros.
        // These entries do not affect the param_types value.
        let mut tee_params = [empty_tee_param(); 4];
        let mut infos = [ParamInfo::None, ParamInfo::None, ParamInfo::None, ParamInfo::None];
        let mut i = 0;
        for param in parameters {
            param_types |= param_type(&param) << (i * 4);
            use fidl_fuchsia_tee::Parameter;
            let (param, info) = match param {
                Parameter::None(_) => (empty_tee_param(), ParamInfo::None),
                Parameter::Buffer(buffer) => import_buffer_from_fidl(buffer)?,
                Parameter::Value(value) => import_value_from_fidl(value)?,
                ParameterUnknown!() => anyhow::bail!("Unexpected parameter type"),
            };
            tee_params[i] = param;
            infos[i] = info;
            i += 1;
        }
        Ok((Self { tee_params, infos }, param_types))
    }

    pub fn tee_params_mut<'a>(&'a mut self) -> &'a mut [TEE_Param; 4] {
        &mut self.tee_params
    }

    // 4.3.6.3 Behavior of the Framework when the Trusted Application Returns
    pub fn export_to_fidl(mut self) -> Result<Vec<ftee::Parameter>, Error> {
        let mut ret = vec![];
        for i in 0..4 {
            ret.push(match &mut self.infos[i] {
                ParamInfo::None => ftee::Parameter::None(ftee::None_),
                ParamInfo::Value(direction) => match direction {
                    Direction::Input => ftee::Parameter::None(ftee::None_),
                    Direction::Inout | Direction::Output => {
                        let tee_value = unsafe { self.tee_params[i].value };
                        let a = Some(tee_value.a as u64);
                        let b = Some(tee_value.b as u64);
                        ftee::Parameter::Value(Value {
                            direction: Some(*direction),
                            a,
                            b,
                            c: None,
                            ..Default::default()
                        })
                    }
                },
                ParamInfo::Memref(memref) => {
                    unsafe {
                        if memref.mapped_address != 0 {
                            // If we mapped the buffer into our address space we need to unmap it.
                            fuchsia_runtime::vmar_root_self()
                                .unmap(memref.mapped_address, memref.mapped_length)
                                .unwrap();
                        }
                    }
                    match memref.direction {
                        Direction::Input => ftee::Parameter::None(ftee::None_),
                        Direction::Inout | Direction::Output => {
                            // Table 4-9: Interpretation of params[i] when Trusted Application Entry Point Returns
                            // The Framework reads params[i].memref.size:
                            let tee_memref = unsafe { self.tee_params[i].memref };
                            let size = tee_memref.size;
                            if size > memref.original_size {
                                // * If it is greater than the original value of size, it is considered
                                // as a request for a larger buffer. In this case, the Framework
                                // assumes that the Trusted Application has not written
                                // anything in the buffer and no data will be synchronized.
                                ftee::Parameter::Buffer(Buffer {
                                    direction: Some(memref.direction),
                                    vmo: None,
                                    offset: memref.offset,
                                    size: Some(size as u64),
                                    ..Default::default()
                                })
                            } else {
                                // * If it is equal or less than the original value of size, it is
                                // considered as the actual size of the memory buffer. In this
                                // case, the Framework assumes that the Trusted Application
                                // has not written beyond this actual size and only this actual
                                // size will be synchronized with the client.
                                ftee::Parameter::Buffer(Buffer {
                                    direction: Some(memref.direction),
                                    vmo: memref.vmo.take(),
                                    offset: memref.offset,
                                    size: Some(size as u64),
                                    ..Default::default()
                                })
                            }
                        }
                    }
                }
            })
        }
        // Remove trailing 'None' entries.
        while !ret.is_empty() {
            match ret.last() {
                Some(ftee::Parameter::None(_)) => {
                    let _ = ret.pop().unwrap();
                }
                _ => break,
            }
        }
        Ok(ret)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    fn empty_fidl_parameters_to_tee_params() -> Result<(), Error> {
        let fidl_parameters = vec![];

        let (mut adapter, param_types) = ParamAdapter::from_fidl(fidl_parameters)?;

        assert_eq!(param_types, 0);

        let tee_params = adapter.tee_params_mut();
        for i in 0..4 {
            // Accessing the 'value' field of the parameter's union is unsafe as it
            // is a repr(C) union and carries no type information.
            let value = unsafe { tee_params[i].value };
            assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 0, b: 0 });
        }
        Ok(())
    }

    #[fuchsia::test]
    fn too_many_fidl_parameters() -> Result<(), Error> {
        let fidl_parameters = vec![
            ftee::Parameter::Value(Value { ..Default::default() }),
            ftee::Parameter::Value(Value { ..Default::default() }),
            ftee::Parameter::Value(Value { ..Default::default() }),
            ftee::Parameter::Value(Value { ..Default::default() }),
            ftee::Parameter::Value(Value { ..Default::default() }),
        ];

        let result = ParamAdapter::from_fidl(fidl_parameters);

        assert!(result.is_err());
        Ok(())
    }

    #[fuchsia::test]
    fn fidl_parameter_value_to_tee_params() -> Result<(), Error> {
        let fidl_parameters = vec![ftee::Parameter::Value(Value {
            direction: Some(Direction::Input),
            a: Some(1),
            b: Some(2),
            ..Default::default()
        })];

        let (mut adapter, param_types) = ParamAdapter::from_fidl(fidl_parameters)?;

        assert_eq!(param_types, TEE_PARAM_TYPE_VALUE_INPUT);
        let tee_params = adapter.tee_params_mut();
        // Accessing the 'value' field of the parameter's union is unsafe as it
        // is a repr(C) union and carries no type information.
        let value = unsafe { tee_params[0].value };
        assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 1, b: 2 });
        for i in 1..4 {
            let value = unsafe { tee_params[i].value };
            assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 0, b: 0 });
        }
        Ok(())
    }

    #[fuchsia::test]
    fn fidl_parameter_multiple_value_to_tee_params() -> Result<(), Error> {
        let fidl_parameters = vec![
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Input),
                a: Some(1),
                b: Some(2),
                ..Default::default()
            }),
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Inout),
                a: Some(3),
                b: Some(4),
                ..Default::default()
            }),
        ];

        let (mut adapter, param_types) = ParamAdapter::from_fidl(fidl_parameters)?;

        assert_eq!(param_types, TEE_PARAM_TYPE_VALUE_INPUT | (TEE_PARAM_TYPE_VALUE_INOUT << 4));
        let tee_params = adapter.tee_params_mut();
        // Accessing the 'value' field of the parameter's union is unsafe as it
        // is a repr(C) union and carries no type information.
        let value = unsafe { tee_params[0].value };
        assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 1, b: 2 });
        let value = unsafe { tee_params[1].value };
        assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 3, b: 4 });
        for i in 2..4 {
            let value = unsafe { tee_params[i].value };
            assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 0, b: 0 });
        }
        Ok(())
    }

    #[fuchsia::test]
    fn fidl_parameter_with_buffers() -> Result<(), Error> {
        let page_size = zx::system_get_page_size() as u64;
        let vmo1 = zx::Vmo::create(page_size).unwrap();
        vmo1.write(&[1, 2, 3, 4], 0).unwrap();
        let vmo2 = zx::Vmo::create(3 * page_size).unwrap();
        vmo2.write(&[5, 6, 7, 8], page_size).unwrap();

        let fidl_parameters = vec![
            ftee::Parameter::Buffer(Buffer {
                direction: Some(Direction::Inout),
                vmo: Some(vmo1),
                offset: Some(0),
                size: Some(page_size),
                ..Default::default()
            }),
            ftee::Parameter::Buffer(Buffer {
                direction: Some(Direction::Input),
                vmo: Some(vmo2),
                offset: Some(page_size),
                size: Some(2 * page_size),
                ..Default::default()
            }),
            ftee::Parameter::Buffer(Buffer {
                direction: Some(Direction::Inout),
                vmo: None,
                offset: Some(0),
                size: Some(0),
                ..Default::default()
            }),
        ];

        let (mut adapter, param_types) = ParamAdapter::from_fidl(fidl_parameters)?;

        assert_eq!(
            param_types,
            TEE_PARAM_TYPE_MEMREF_INOUT
                | (TEE_PARAM_TYPE_MEMREF_INPUT << 4)
                | (TEE_PARAM_TYPE_MEMREF_INOUT << 8)
        );
        let tee_params = adapter.tee_params_mut();
        // Accessing the 'value' field of the parameter's union is unsafe as it
        // is a repr(C) union and carries no type information.
        let value = unsafe { tee_params[0].memref };
        assert_eq!(value.size, page_size as usize);
        assert!(value.buffer != std::ptr::null_mut() as *mut std::os::raw::c_void);
        let memref_contents = unsafe { std::slice::from_raw_parts(value.buffer as *const u8, 4) };
        assert_eq!(memref_contents, &[1, 2, 3, 4]);

        let value = unsafe { tee_params[1].memref };
        assert_eq!(value.size, 2 * page_size as usize);
        assert!(value.buffer != std::ptr::null_mut() as *mut std::os::raw::c_void);
        let memref_contents = unsafe { std::slice::from_raw_parts(value.buffer as *const u8, 4) };
        assert_eq!(memref_contents, &[5, 6, 7, 8]);

        let value = unsafe { tee_params[2].memref };
        assert_eq!(value.size, 0);
        assert_eq!(value.buffer, std::ptr::null_mut() as *mut std::os::raw::c_void);

        for i in 3..4 {
            let value = unsafe { tee_params[i].value };
            assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 0, b: 0 });
        }
        Ok(())
    }

    #[fuchsia::test]
    fn test_export_empty_to_fidl() -> Result<(), Error> {
        let (adapter, param_types) = ParamAdapter::from_fidl(vec![])?;

        assert_eq!(param_types, TEE_PARAM_TYPE_NONE);

        let fidl_parameters = adapter.export_to_fidl()?;

        assert!(fidl_parameters.is_empty());
        Ok(())
    }

    #[fuchsia::test]
    fn test_unchanged_value_directions() -> Result<(), Error> {
        let fidl_parameters = vec![
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Input),
                a: Some(1),
                b: Some(2),
                c: None,
                ..Default::default()
            }),
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Inout),
                a: Some(3),
                b: Some(4),
                c: None,
                ..Default::default()
            }),
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Output),
                a: None,
                b: None,
                c: None,
                ..Default::default()
            }),
        ];
        let (adapter, param_types) = ParamAdapter::from_fidl(fidl_parameters)?;

        assert_eq!(
            param_types,
            TEE_PARAM_TYPE_VALUE_INPUT
                | TEE_PARAM_TYPE_VALUE_INOUT << 4
                | TEE_PARAM_TYPE_VALUE_OUTPUT << 8
        );

        let fidl_parameters = adapter.export_to_fidl()?;
        assert_eq!(fidl_parameters.len(), 3);
        assert_eq!(fidl_parameters[0], ftee::Parameter::None(ftee::None_));
        assert_eq!(
            fidl_parameters[1],
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Inout),
                a: Some(3),
                b: Some(4),
                c: None,
                ..Default::default()
            })
        );
        assert_eq!(
            fidl_parameters[2],
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Output),
                a: Some(0),
                b: Some(0),
                c: None,
                ..Default::default()
            })
        );

        Ok(())
    }

    #[fuchsia::test]
    fn test_export_changed_value_to_fidl() -> Result<(), Error> {
        let fidl_parameters = vec![
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Inout),
                a: Some(1),
                b: Some(2),
                c: None,
                ..Default::default()
            }),
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Output),
                a: None,
                b: None,
                c: None,
                ..Default::default()
            }),
        ];
        let (mut adapter, param_types) = ParamAdapter::from_fidl(fidl_parameters)?;

        assert_eq!(param_types, TEE_PARAM_TYPE_VALUE_INOUT | TEE_PARAM_TYPE_VALUE_OUTPUT << 4);

        let tee_params = adapter.tee_params_mut();
        let value = unsafe { &mut tee_params[0].value };
        value.a = 3;
        value.b = 4;
        let value = unsafe { &mut tee_params[1].value };
        value.a = 5;
        value.b = 6;

        let fidl_parameters = adapter.export_to_fidl()?;

        assert_eq!(fidl_parameters.len(), 2);
        assert_eq!(
            fidl_parameters[0],
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Inout),
                a: Some(3),
                b: Some(4),
                c: None,
                ..Default::default()
            })
        );
        assert_eq!(
            fidl_parameters[1],
            ftee::Parameter::Value(Value {
                direction: Some(Direction::Output),
                a: Some(5),
                b: Some(6),
                c: None,
                ..Default::default()
            })
        );

        Ok(())
    }

    #[fuchsia::test]
    fn test_export_changed_memref_to_fidl() -> Result<(), Error> {
        // This test checks that we can provide a buffer with direction "INOUT" and modify the buffer's
        // contents and size then translate that back to a FIDL response.
        let page_size = zx::system_get_page_size() as u64;
        let vmo = zx::Vmo::create(page_size).unwrap();
        vmo.write(&[1, 2, 3], 0).unwrap();
        let fidl_parameters = vec![ftee::Parameter::Buffer(Buffer {
            direction: Some(Direction::Inout),
            vmo: Some(vmo),
            offset: Some(0),
            size: Some(page_size),
            ..Default::default()
        })];

        let (mut adapter, param_types) = ParamAdapter::from_fidl(fidl_parameters)?;

        assert_eq!(param_types, TEE_PARAM_TYPE_MEMREF_INOUT);

        {
            // This logic makes two modifications to the TEE_Param[4] representation of the buffer:
            // 1. Writes the byte sequence [3, 4, 5] to the start of the buffer.
            // 2. Sets the memref's "size" to 3 down from its initial value of page_size.
            let tee_params = adapter.tee_params_mut();
            let memref = unsafe { &mut tee_params[0].memref };
            assert_eq!(memref.size, page_size as usize);
            assert_ne!(memref.buffer, std::ptr::null_mut() as *mut std::ffi::c_void);

            {
                let mapped_buffer =
                    unsafe { std::slice::from_raw_parts_mut(memref.buffer as *mut u8, 3) };
                mapped_buffer.copy_from_slice(&[3, 4, 5]);
            }
            memref.size = 3;
        }

        let fidl_parameters = adapter.export_to_fidl()?;

        assert_eq!(fidl_parameters.len(), 1);

        if let ftee::Parameter::Buffer(buffer) = &fidl_parameters[0] {
            // We expect to get back a buffer that has the byte sequence [3, 4, 5] at the start and
            // a reported size of 3.
            let mut read = [0u8; 3];
            let _ = buffer.vmo.as_ref().unwrap().read(&mut read, 0).unwrap();
            assert_eq!(read, [3, 4, 5]);
            assert_eq!(buffer.direction, Some(Direction::Inout));
            assert_eq!(buffer.offset, Some(0));
            assert_eq!(buffer.size, Some(3));
        } else {
            assert!(
                false,
                "Expected a Buffer parameter but observed another type: {:?}",
                &fidl_parameters[0]
            );
        }

        Ok(())
    }

    #[fuchsia::test]
    fn test_memref_size_increased() -> Result<(), Error> {
        // This tests the behavior when a buffer's size is changed in the TEE_Param[4]
        // representation to a larger value than was initially specified. According to the
        // specification this pattern is used to let a TA indicate that an operation requires a
        // larger buffer than was initially provided.  In this case no data is returned to the
        // client but the larger size is returned with the expectation that the client will allocate
        // a larger buffer and try again.
        let page_size = zx::system_get_page_size() as u64;
        let vmo = zx::Vmo::create(page_size).unwrap();
        let fidl_parameters = vec![
            ftee::Parameter::Buffer(Buffer {
                direction: Some(Direction::Inout),
                vmo: None,
                offset: Some(0),
                size: Some(0),
                ..Default::default()
            }),
            ftee::Parameter::Buffer(Buffer {
                direction: Some(Direction::Inout),
                vmo: Some(vmo),
                offset: Some(0),
                size: Some(page_size),
                ..Default::default()
            }),
        ];

        let (mut adapter, param_types) = ParamAdapter::from_fidl(fidl_parameters)?;

        assert_eq!(param_types, TEE_PARAM_TYPE_MEMREF_INOUT | TEE_PARAM_TYPE_MEMREF_INOUT << 4);

        {
            let tee_params = adapter.tee_params_mut();
            let memref = unsafe { &mut tee_params[0].memref };
            // The first parameter is grown from 0 bytes to 12 bytes.
            memref.size = 12;
            let memref = unsafe { &mut tee_params[1].memref };
            // The second parameter is grown from 1 page to 2 pages.
            memref.size = 2 * page_size as usize;
        }

        let fidl_parameters = adapter.export_to_fidl()?;

        assert_eq!(
            fidl_parameters,
            vec![
                ftee::Parameter::Buffer(Buffer {
                    direction: Some(Direction::Inout),
                    vmo: None,
                    offset: Some(0),
                    size: Some(12),
                    ..Default::default()
                }),
                ftee::Parameter::Buffer(Buffer {
                    direction: Some(Direction::Inout),
                    vmo: None,
                    offset: Some(0),
                    size: Some(2 * page_size),
                    ..Default::default()
                }),
            ]
        );

        Ok(())
    }
}
