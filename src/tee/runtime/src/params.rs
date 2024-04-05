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

struct BufferMapping {
    vmo: zx::Vmo,
    mapped_address: usize,
    mapped_length: usize,
}

// TEEParams has logic for translating parameters for a trusted application from the
// FIDL format to the in-memory format used by the TA and back.
// Translating between the FIDL and in-memory representations of buffer requires
// memory mapping operations and is stateful. This type stores this state and cleans
// it up when dropped.
pub struct TEEParams {
    mapped_buffers: Vec<BufferMapping>,
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

fn import_value_from_fidl(value: Value) -> Result<TEE_Param, Error> {
    Ok(match value.direction {
        Some(direction) => match direction {
            Direction::Input | Direction::Inout => TEE_Param {
                value: TEE_Param__bindgen_ty_2 {
                    a: value.a.unwrap_or_default() as u32,
                    b: value.b.unwrap_or_default() as u32,
                },
            },
            Direction::Output => empty_tee_param(),
        },
        None => anyhow::bail!("Invalid direction"),
    })
}

impl TEEParams {
    pub fn new() -> Self {
        Self { mapped_buffers: vec![] }
    }

    pub fn import_from_fidl(
        &mut self,
        parameters: Vec<ftee::Parameter>,
    ) -> Result<(u32, Vec<TEE_Param>), Error> {
        if parameters.len() > 4 {
            anyhow::bail!("Expected 4 parameters in set but got {}", parameters.len());
        }

        let mut param_types = 0;
        let mut param_shift = 0;
        let mut params = vec![];
        for param in parameters {
            param_types |= param_type(&param) << param_shift;
            param_shift += 4;
            use fidl_fuchsia_tee::Parameter;
            let param = match param {
                Parameter::None(_) => empty_tee_param(),
                Parameter::Buffer(buffer) => self.import_buffer_from_fidl(buffer)?,
                Parameter::Value(value) => import_value_from_fidl(value)?,
                ParameterUnknown!() => anyhow::bail!("Unexpected parameter type"),
            };
            params.push(param);
        }
        // 4.3.6.2 Initial Content of params Argument specifies that unset values must be filled with zeros.
        // These entries do not affect the param_types value.
        while params.len() < 4 {
            params.push(empty_tee_param());
        }
        Ok((param_types, params))
    }

    fn import_buffer_from_fidl(&mut self, buffer: Buffer) -> Result<TEE_Param, Error> {
        Ok(match buffer.direction {
            Some(direction) => match direction {
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
                    let addr = if let Some(vmo) = vmo {
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

                        self.mapped_buffers.push(BufferMapping {
                            vmo,
                            mapped_address: addr,
                            mapped_length,
                        });
                        addr
                    } else {
                        // The VMO can be omitted meaning that no memory is
                        // mapped. This is useful for asking the TA to compute
                        // the size of a buffer needed for an operation. In this
                        // case, the null address is provided as the buffer
                        // address.
                        0
                    };
                    TEE_Param {
                        memref: TEE_Param__bindgen_ty_1 {
                            buffer: addr as *mut std::os::raw::c_void,
                            size: mapped_length,
                        },
                    }
                }
                Direction::Output => empty_tee_param(),
            },
            None => anyhow::bail!("Invalid direction"),
        })
    }

    fn unmap_buffers(&mut self) {
        for mapped_buffer in &self.mapped_buffers {
            unsafe {
                fuchsia_runtime::vmar_root_self()
                    .unmap(mapped_buffer.mapped_address, mapped_buffer.mapped_length)
                    .unwrap()
            };
            let _ = mapped_buffer.vmo;
        }
        self.mapped_buffers.clear();
    }
}

impl Drop for TEEParams {
    fn drop(&mut self) {
        self.unmap_buffers()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    fn empty_fidl_parameters_to_tee_params() {
        let fidl_parameters = vec![];

        let mut tee_params = TEEParams::new();

        let (param_types, params) = tee_params.import_from_fidl(fidl_parameters).unwrap();

        assert_eq!(param_types, 0);
        assert_eq!(params.len(), 4);
        for i in 0..4 {
            // Accessing the 'value' field of the parameter's union is unsafe as it
            // is a repr(C) union and carries no type information.
            let value = unsafe { params[i].value };
            assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 0, b: 0 });
        }
    }

    #[fuchsia::test]
    fn too_many_fidl_parameters() {
        let fidl_parameters = vec![
            ftee::Parameter::Value(Value { ..Default::default() }),
            ftee::Parameter::Value(Value { ..Default::default() }),
            ftee::Parameter::Value(Value { ..Default::default() }),
            ftee::Parameter::Value(Value { ..Default::default() }),
            ftee::Parameter::Value(Value { ..Default::default() }),
        ];

        let mut tee_params = TEEParams::new();
        let result = tee_params.import_from_fidl(fidl_parameters);
        assert!(result.is_err());
    }

    #[fuchsia::test]
    fn fidl_parameter_value_to_tee_params() {
        let fidl_parameters = vec![ftee::Parameter::Value(Value {
            direction: Some(Direction::Input),
            a: Some(1),
            b: Some(2),
            ..Default::default()
        })];

        let mut tee_params = TEEParams::new();

        let (param_types, params) = tee_params.import_from_fidl(fidl_parameters).unwrap();

        assert_eq!(params.len(), 4);
        assert_eq!(param_types, TEE_PARAM_TYPE_VALUE_INPUT);
        // Accessing the 'value' field of the parameter's union is unsafe as it
        // is a repr(C) union and carries no type information.
        let value = unsafe { params[0].value };
        assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 1, b: 2 });
        for i in 1..4 {
            let value = unsafe { params[i].value };
            assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 0, b: 0 });
        }
    }

    #[fuchsia::test]
    fn fidl_parameter_multiple_value_to_tee_params() {
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

        let mut tee_params = TEEParams::new();

        let (param_types, params) = tee_params.import_from_fidl(fidl_parameters).unwrap();

        assert_eq!(param_types, TEE_PARAM_TYPE_VALUE_INPUT | (TEE_PARAM_TYPE_VALUE_INOUT << 4));
        // Accessing the 'value' field of the parameter's union is unsafe as it
        // is a repr(C) union and carries no type information.
        let value = unsafe { params[0].value };
        assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 1, b: 2 });
        let value = unsafe { params[1].value };
        assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 3, b: 4 });
        for i in 2..4 {
            let value = unsafe { params[i].value };
            assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 0, b: 0 });
        }
    }

    #[fuchsia::test]
    fn fidl_parameter_with_buffers() {
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

        let mut tee_params = TEEParams::new();

        let (param_types, params) = tee_params.import_from_fidl(fidl_parameters).unwrap();

        assert_eq!(
            param_types,
            TEE_PARAM_TYPE_MEMREF_INOUT
                | (TEE_PARAM_TYPE_MEMREF_INPUT << 4)
                | (TEE_PARAM_TYPE_MEMREF_INOUT << 8)
        );
        assert_eq!(params.len(), 4);
        // Accessing the 'value' field of the parameter's union is unsafe as it
        // is a repr(C) union and carries no type information.
        let value = unsafe { params[0].memref };
        assert_eq!(value.size, page_size as usize);
        assert!(value.buffer != std::ptr::null_mut() as *mut std::os::raw::c_void);
        let memref_contents = unsafe { std::slice::from_raw_parts(value.buffer as *const u8, 4) };
        assert_eq!(memref_contents, &[1, 2, 3, 4]);

        let value = unsafe { params[1].memref };
        assert_eq!(value.size, 2 * page_size as usize);
        assert!(value.buffer != std::ptr::null_mut() as *mut std::os::raw::c_void);
        let memref_contents = unsafe { std::slice::from_raw_parts(value.buffer as *const u8, 4) };
        assert_eq!(memref_contents, &[5, 6, 7, 8]);

        let value = unsafe { params[2].memref };
        assert_eq!(value.size, 0);
        assert_eq!(value.buffer, std::ptr::null_mut() as *mut std::os::raw::c_void);

        for i in 3..4 {
            let value = unsafe { params[i].value };
            assert_eq!(value, TEE_Param__bindgen_ty_2 { a: 0, b: 0 });
        }
    }
}
