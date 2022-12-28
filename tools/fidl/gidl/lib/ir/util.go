// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package ir

import (
	"fmt"
	"reflect"

	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/config"
)

func Merge(input []All) All {
	var output All
	for _, elem := range input {
		output.EncodeSuccess = append(output.EncodeSuccess, elem.EncodeSuccess...)
		output.DecodeSuccess = append(output.DecodeSuccess, elem.DecodeSuccess...)
		output.EncodeFailure = append(output.EncodeFailure, elem.EncodeFailure...)
		output.DecodeFailure = append(output.DecodeFailure, elem.DecodeFailure...)
		output.Benchmark = append(output.Benchmark, elem.Benchmark...)
	}
	return output
}

func FilterByBinding(input All, binding string) All {
	shouldKeep := func(binding string, allowlist *LanguageList, denylist *LanguageList) bool {
		if denylist != nil && denylist.Includes(binding) {
			return false
		}
		if allowlist != nil {
			return allowlist.Includes(binding)
		}
		if LanguageList(config.DefaultBindingsDenylist).Includes(binding) {
			return false
		}
		return true
	}
	var output All
	for _, def := range input.EncodeSuccess {
		if shouldKeep(binding, def.BindingsAllowlist, def.BindingsDenylist) {
			output.EncodeSuccess = append(output.EncodeSuccess, def)
		}
	}
	for _, def := range input.DecodeSuccess {
		if shouldKeep(binding, def.BindingsAllowlist, def.BindingsDenylist) {
			output.DecodeSuccess = append(output.DecodeSuccess, def)
		}
	}
	for _, def := range input.EncodeFailure {
		if shouldKeep(binding, def.BindingsAllowlist, def.BindingsDenylist) {
			output.EncodeFailure = append(output.EncodeFailure, def)
		}
	}
	for _, def := range input.DecodeFailure {
		if shouldKeep(binding, def.BindingsAllowlist, def.BindingsDenylist) {
			output.DecodeFailure = append(output.DecodeFailure, def)
		}
	}
	for _, def := range input.Benchmark {
		if shouldKeep(binding, def.BindingsAllowlist, def.BindingsDenylist) {
			output.Benchmark = append(output.Benchmark, def)
		}
	}
	return output
}

func ValidateAllType(input All, generatorType string) {
	forbid := func(fields ...interface{}) {
		for _, field := range fields {
			if reflect.ValueOf(field).Len() > 0 {
				panic("illegal field specified")
			}
		}
	}
	switch generatorType {
	case "conformance":
		forbid(input.Benchmark)
	case "benchmark":
		forbid(input.EncodeSuccess, input.DecodeSuccess, input.EncodeFailure, input.DecodeFailure)
	case "measure_tape":
		forbid(input.Benchmark)
	default:
		panic(fmt.Sprintf("unexpected generator type: %s", generatorType))
	}
}

func TypeFromValue(value Value) string {
	record, ok := value.(RecordLike)
	if !ok {
		panic(fmt.Sprintf("cannot extract type name from: %T", value))
	}
	return record.TypeName()
}

func GetHandlesFromHandleDispositions(handleDispositions []HandleDisposition) []Handle {
	var handles []Handle
	for _, handleDisposition := range handleDispositions {
		handles = append(handles, handleDisposition.Handle)
	}
	return handles
}

// GetUnusedHandles returns the list of handles from the input slice that do not
// appear in the provided Value
func GetUnusedHandles(value Value, handles []Handle) []Handle {
	usedHandles := make(map[Handle]struct{})
	populateUsedHandles(value, usedHandles)

	var unused []Handle
	for _, handle := range handles {
		if _, ok := usedHandles[handle]; !ok {
			unused = append(unused, handle)
		}
	}
	return unused
}

func populateUsedHandles(value Value, seen map[Handle]struct{}) {
	switch value := value.(type) {
	case AnyHandle:
		seen[value.GetHandle()] = struct{}{}
	case Record:
		for _, field := range value.Fields {
			populateUsedHandles(field.Value, seen)
		}
	case UnknownData:
		for _, handle := range value.Handles {
			seen[handle] = struct{}{}
		}
	case []Value:
		for _, item := range value {
			populateUsedHandles(item, seen)
		}
	}
}
