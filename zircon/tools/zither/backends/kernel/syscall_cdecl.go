// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package kernel

import (
	"bytes"
	"fmt"
	"text/template"

	"go.fuchsia.dev/fuchsia/zircon/tools/zither"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither/backends/c"
)

const syscallCDeclTemplate = `
{{ $lastParamIndex := LastParameterIndex . }}
{{ Macro . }}(
	{{ Name . }}, {{ ReturnType .}}, {{ Attributes .}}, {{ len .Parameters }},
	(
{{- range $i, $param := .Parameters }}
	{{- Name $param }}{{ if ne $i $lastParamIndex }},{{ end }}
{{- end -}}
    ),
	(
{{- if .Parameters }}
{{- range $i, $param := .Parameters }}
	{{- ParameterType $param }} {{ Name $param }}{{ if ne $i $lastParamIndex }},{{ end }}
{{- end -}}
{{- else }}
	void
{{- end -}}
	))
`

// PointerView distinguishes how pointers passed across syscall boundaries are
// treated in user- and kernel-space.
type PointerView int

const (
	PointerViewUserspace PointerView = iota
	PointerViewKernel
)

// SyscallCDecl generates a macro-friendly representation of the syscall,
// minor variations of which are used across generated syscall-related
// sources.
func SyscallCDecl(syscall zither.Syscall, ptrView PointerView, macroName func(zither.Syscall) string) string {
	tmpl := template.New("SyscallCDeclTemplate").Funcs(template.FuncMap{
		"Attributes": cDeclAttributes,
		"LastParameterIndex": func(syscall zither.Syscall) int {
			return len(syscall.Parameters) - 1
		},
		"Macro":      macroName,
		"Name":       zither.LowerCaseWithUnderscores,
		"ReturnType": cDeclReturnType,
		"ParameterType": func(param zither.SyscallParameter) string {
			return cDeclParameterType(param, ptrView, true)
		},
	})
	template.Must(tmpl.Parse(syscallCDeclTemplate))
	buf := new(bytes.Buffer)
	if err := tmpl.Execute(buf, syscall); err != nil {
		panic(err)
	}
	return string(buf.Bytes())
}

func cDeclAttributes(syscall zither.Syscall) string {
	if syscall.Const {
		return "__CONST"
	}
	if syscall.NoReturn {
		return "__NO_RETURN"
	}
	return "/* no attributes */"
}

func cDeclMacro(syscall zither.Syscall) string {
	if syscall.Category == zither.SyscallCategoryVdsoCall {
		return "VDSO_SYSCALL"
	}
	if syscall.Category == zither.SyscallCategoryInternal {
		return "INTERNAL_SYSCALL"
	}
	if syscall.Blocking {
		return "BLOCKING_SYSCALL"
	}
	return "KERNEL_SYSCALL"
}

func passedAsPointer(param zither.SyscallParameter) bool {
	if !param.IsStrictInput() {
		return true
	}
	kind := param.Type.Kind
	// Structs are always passed as pointers.
	return kind.IsPointerLike() || kind == zither.TypeKindStruct
}

func cDeclParameterType(param zither.SyscallParameter, ptrView PointerView, annotated bool) string {
	kind := param.Type.Kind

	typ := c.DescribeType(param.Type).Type
	if passedAsPointer(param) {
		isPointer := kind.IsPointerLike()
		isConst := param.IsStrictInput()
		switch ptrView {
		case PointerViewUserspace:
			if !isPointer {
				typ += "*"
			}
			if isConst {
				typ = "const " + typ
			}
		case PointerViewKernel:
			elementType := typ
			if kind == zither.TypeKindVoidPointer {
				elementType = "void"
			} else if isPointer {
				elementType = c.DescribeType(*param.Type.ElementType).Type
			}
			if isConst {
				elementType = "const " + elementType
			}

			switch param.Orientation {
			case zither.ParameterOrientationIn:
				typ = fmt.Sprintf("user_in_ptr<%s>", elementType)
			case zither.ParameterOrientationOut:
				if kind == zither.TypeKindHandle && !param.HasTag(zither.ParameterTagDecayedFromVector) {
					typ += "*"
				} else {
					typ = fmt.Sprintf("user_out_ptr<%s>", elementType)
				}
			case zither.ParameterOrientationInOut:
				typ = fmt.Sprintf("user_inout_ptr<%s>", elementType)
			}
		}
	}

	if annotated {
		annotation := ""
		switch kind {
		// Handle and handle pointers have a related set of annotations informing
		// static analysis.
		case zither.TypeKindHandle, zither.TypeKindPointer:
			if kind == zither.TypeKindPointer && param.Type.ElementType.Kind != zither.TypeKindHandle {
				break
			}
			action := "use"
			if param.HasTag(zither.ParameterTagReleasedHandle) {
				action = "release"
			} else if param.IsStrictOutput() {
				action = "acquire"
			}
			label := "Fuchsia"
			if param.HasTag(zither.ParameterTagUncheckedHandle) {
				label = "FuchsiaUnchecked"
			}
			annotation = fmt.Sprintf("_ZX_SYSCALL_ANNO(%s_handle(%q))", action, label)
		}
		if annotation != "" {
			typ = annotation + " " + typ
		}
	}

	return typ
}

func cDeclReturnType(syscall zither.Syscall) string {
	if syscall.ReturnType == nil {
		return "void"
	}
	return c.DescribeType(*syscall.ReturnType).Type
}
