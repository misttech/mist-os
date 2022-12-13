// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package go_runtime

import (
	"embed"
	"fmt"
	"path/filepath"
	"sort"
	"strings"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/golang"
)

//go:embed templates/*
var templates embed.FS

type Generator struct {
	fidlgen.Generator
}

// Go runtime sources, given by source-relative path. Each file has a
// corresponding template named "Generate-${file basename}".
var generatedSources = []string{
	// Syscall bindings are defined in the 'runtime' package instead of the
	// 'syscall/zx' package, since the former currently also needs to make
	// syscalls and the latter is one of its dependents.
	filepath.Join("src", "runtime", "vdso_keys_fuchsia.go"),
	// TODO(fxbug.dev/110295): filepath.Join("src", "runtime", "vdsocalls_fuchsia_amd64.s"),
	// TODO(fxbug.dev/110295): filepath.Join("src", "runtime", "vdsocalls_fuchsia_arm64.s"),

	// Defines the public `Sys_foo_bar(...)`s, defined as jumps to the runtime
	// package's `vdsoCall_zx_foo_bar` analogues.
	filepath.Join("src", "syscall", "zx", "syscalls_fuchsia.go"),
	//TODO(fxbug.dev/110295): filepath.Join("src", "syscall", "zx", "syscalls_fuchsia_amd64.s"),
	//TODO(fxbug.dev/110295): filepath.Join("src", "syscall", "zx", "syscalls_fuchsia_arm64.s"),
}

func NewGenerator(formatter fidlgen.Formatter) *Generator {
	gen := fidlgen.NewGenerator("GoRuntimeTemplates", templates, formatter, template.FuncMap{
		"FFIParameterType":         ffiParameterType,
		"FFIReturnType":            ffiReturnType,
		"Hash":                     elfGNUHash,
		"LowerCaseWithUnderscores": lowerCaseWithUnderscores,
		"LastParameterIndex":       func(syscall zither.Syscall) int { return len(syscall.Parameters) - 1 },
		"ParameterType":            parameterType,
		"ReturnType":               returnType,
	})
	return &Generator{*gen}
}

func (gen Generator) DeclOrder() zither.DeclOrder { return zither.SourceDeclOrder }

func (gen *Generator) Generate(summaries []zither.FileSummary, outputDir string) ([]string, error) {
	var syscalls []zither.Syscall
	for _, summary := range summaries {
		for _, decl := range summary.Decls {
			if !decl.IsSyscallFamily() {
				continue
			}
			for _, syscall := range decl.AsSyscallFamily().Syscalls {
				syscalls = append(syscalls, syscall)
			}
		}
	}
	sort.Slice(syscalls, func(i, j int) bool {
		return strings.Compare(syscalls[i].Name, syscalls[j].Name) < 0
	})

	var outputs []string
	for _, file := range generatedSources {
		output := filepath.Join(outputDir, file)
		templateName := "Generate-" + filepath.Base(file)
		if err := gen.GenerateFile(output, templateName, syscalls); err != nil {
			return nil, err
		}
		outputs = append(outputs, output)
	}
	return outputs, nil
}

func ffiType(desc zither.TypeDescriptor) string {
	switch desc.Kind {
	case zither.TypeKindBool, zither.TypeKindInteger, zither.TypeKindSize:
		return golang.PrimitiveTypeName(fidlgen.PrimitiveSubtype(desc.Type))
	case zither.TypeKindEnum:
		enum := desc.Decl.(*zither.Enum)
		return golang.PrimitiveTypeName(fidlgen.PrimitiveSubtype(enum.Subtype))
	case zither.TypeKindBits:
		bits := desc.Decl.(*zither.Bits)
		return golang.PrimitiveTypeName(fidlgen.PrimitiveSubtype(bits.Subtype))
		// Structs are passed as pointers.
	case zither.TypeKindStruct, zither.TypeKindArray,
		zither.TypeKindPointer, zither.TypeKindVoidPointer:
		return "unsafe.Pointer"
	case zither.TypeKindHandle:
		return "uint32"
	case zither.TypeKindAlias:
		// TODO(fxbug.dev/110021): These types are currently misdefined as
		// `uint64` aliases (instead of `uintptr` ones).
		if desc.Type == "zx/paddr" || desc.Type == "zx/vaddr" {
			return "uintptr"
		}
		alias := desc.Decl.(*zither.Alias)
		return ffiType(alias.Value)
	default:
		panic(fmt.Sprintf("unknown kind %q: %#v", desc.Kind, desc))
	}
}

//
// Template functions.
//

func lowerCaseWithUnderscores(el zither.Element) string {
	// Account for reserved keywords with some cheesy renaming.
	name := zither.LowerCaseWithUnderscores(el)
	switch name {
	case "type":
		return "typ"
	case "func":
		return "funk"
	case "g":
		return "g_"
	default:
		return name
	}
}

func parameterType(param zither.SyscallParameter) string {
	kind := param.Type.Kind

	// Structs and non-inputs should be passed as pointers.
	if !kind.IsPointerLike() && (!param.IsStrictInput() || kind == zither.TypeKindStruct) {
		elementType := param.Type
		elementType.Mutable = !param.IsStrictInput()
		return golang.DescribeType(zither.TypeDescriptor{
			Kind:        zither.TypeKindPointer,
			ElementType: &elementType,
		})
	}
	return golang.DescribeType(param.Type)
}

func returnType(syscall zither.Syscall) string {
	if syscall.ReturnType == nil {
		return ""
	}
	return golang.DescribeType(*syscall.ReturnType)
}

func ffiParameterType(param zither.SyscallParameter) string {
	// Non-inputs are passed by pointers.
	if !param.IsStrictInput() {
		return "unsafe.Pointer"
	}
	return ffiType(param.Type)
}

func ffiReturnType(syscall zither.Syscall) string {
	if syscall.ReturnType == nil {
		return ""
	}
	return ffiType(*syscall.ReturnType)
}

// String hash for the DT_GNU_HASH format, which uses the djb2 hash algorithm.
// This gives one the parts of the ELF dynamic linking metadata needed to look
// up names in the vDSO.
func elfGNUHash(s string) string {
	h := uint32(5381)
	for _, c := range s {
		h += (h << 5) + uint32(c)
	}
	return fmt.Sprintf("%#x", int(h))
}
