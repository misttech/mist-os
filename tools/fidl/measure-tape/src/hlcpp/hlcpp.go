// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package hlcpp

import (
	"bytes"
	"fmt"
	"regexp"
	"sort"
	"strings"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
	"go.fuchsia.dev/fuchsia/tools/fidl/measure-tape/src/measurer"
	"go.fuchsia.dev/fuchsia/tools/fidl/measure-tape/src/utils"
)

type Printer struct {
	m            *measurer.Measurer
	hIncludePath string
}

func NewPrinter(m *measurer.Measurer, hIncludePath string) *Printer {
	return &Printer{
		m:            m,
		hIncludePath: hIncludePath,
	}
}

type tmplParams struct {
	Year                   string
	HeaderTag              string
	HIncludePath           string
	Namespaces             []string
	LibraryNameWithSlashes string
	TargetTypes            []string
	CcIncludes             []string
}

func (params tmplParams) RevNamespaces() []string {
	rev := make([]string, len(params.Namespaces), len(params.Namespaces))
	for i, j := 0, len(params.Namespaces)-1; i < len(params.Namespaces); i, j = i+1, j-1 {
		rev[i] = params.Namespaces[j]
	}
	return rev
}

var header = template.Must(template.New("header").Parse(`// Copyright {{ .Year }} The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Code generated by "measure-tape"; DO NOT EDIT.
//
// See tools/fidl/measure-tape/README.md

// clang-format off
#ifndef {{ .HeaderTag }}
#define {{ .HeaderTag }}

#include <{{ .LibraryNameWithSlashes }}/cpp/fidl.h>

{{ range .Namespaces }}
namespace {{ . }} {
{{- end}}

struct Size {
  explicit Size(int64_t num_bytes, int64_t num_handles)
    : num_bytes(num_bytes), num_handles(num_handles) {}

  const int64_t num_bytes;
  const int64_t num_handles;
};

{{ range $targetType := .TargetTypes }}
// Helper function to measure {{ $targetType }}.
//
// In most cases, the size returned is a precise size. Otherwise, the size
// returned is a safe upper-bound.
Size Measure(const {{ $targetType }}& value);
{{ end }}

{{ range .RevNamespaces }}
}  // {{ . }}
{{- end}}

#endif  // {{ .HeaderTag }}
`))

var ccTop = template.Must(template.New("ccTop").Parse(`// Copyright {{ .Year }} The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Code generated by "measure-tape"; DO NOT EDIT.
//
// See tools/fidl/measure-tape/README.md

// clang-format off
#include <{{ .HIncludePath }}>
{{ range .CcIncludes }}
{{ . }}
{{- end }}
#include <zircon/types.h>

{{ range .Namespaces }}
namespace {{ . }} {
{{- end}}

namespace {

class MeasuringTape {
 public:
  MeasuringTape() = default;
`))

var ccBottom = template.Must(template.New("ccBottom").Parse(`
  Size Done() {
    if (maxed_out_) {
      return Size(ZX_CHANNEL_MAX_MSG_BYTES, ZX_CHANNEL_MAX_MSG_HANDLES);
    }
    return Size(num_bytes_, num_handles_);
  }

private:
  void MaxOut() { maxed_out_ = true; }

  bool maxed_out_ = false;
  int64_t num_bytes_ = 0;
  int64_t num_handles_ = 0;
};

}  // namespace

{{ range $targetType := .TargetTypes }}
Size Measure(const {{ $targetType }}& value) {
  MeasuringTape tape;
  tape.Measure(value);
  return tape.Done();
}
{{ end }}

{{ range .RevNamespaces }}
}  // {{ . }}
{{- end}}
`))

var pathSeparators = regexp.MustCompile("[/_.-]")

func (p *Printer) newTmplParams(targetMts []*measurer.MeasuringTape) tmplParams {
	libraryName := targetMts[0].Name().LibraryName()
	for _, targetMt := range targetMts[1:] {
		if otherLibraryName := targetMt.Name().LibraryName(); libraryName != otherLibraryName {
			panic(fmt.Sprintf("all target types must be in the same library, found %s and %s", libraryName, otherLibraryName))
		}
	}
	namespaces := []string{"measure_tape"}
	namespaces = append(namespaces, libraryName.Parts()...)

	headerTagParts := pathSeparators.Split(p.hIncludePath, -1)
	for i, part := range headerTagParts {
		headerTagParts[i] = strings.ToUpper(part)
	}
	headerTagParts = append(headerTagParts, "")
	headerTag := strings.Join(headerTagParts, "_")

	var targetMtsNames []string
	for _, targetMt := range targetMts {
		targetMtsNames = append(targetMtsNames, fmtType(targetMt.Name()))
	}
	return tmplParams{
		Year:                   "2020",
		HeaderTag:              headerTag,
		HIncludePath:           p.hIncludePath,
		LibraryNameWithSlashes: strings.Join(libraryName.Parts(), "/"),
		TargetTypes:            targetMtsNames,
		Namespaces:             namespaces,
	}
}

func (p *Printer) WriteH(buf *bytes.Buffer, targetMts []*measurer.MeasuringTape) {
	if err := header.Execute(buf, p.newTmplParams(targetMts)); err != nil {
		panic(err)
	}
}

func (p *Printer) WriteCc(buf *bytes.Buffer,
	targetMts []*measurer.MeasuringTape,
	allMethods map[measurer.MethodID]*measurer.Method) {

	params := p.newTmplParams(targetMts)
	for _, libraryName := range p.m.RootLibraries() {
		params.CcIncludes = append(params.CcIncludes,
			fmt.Sprintf("#include <%s/cpp/fidl.h>", strings.Join(libraryName.Parts(), "/")))
	}
	sort.Strings(params.CcIncludes)

	if err := ccTop.Execute(buf, params); err != nil {
		panic(err)
	}

	cb := codeBuffer{buf: buf, level: 1}
	utils.ForAllMethodsInOrder(allMethods, func(m *measurer.Method) {
		buf.WriteString("\n")
		cb.writeMethod(m)
	})

	if err := ccBottom.Execute(buf, params); err != nil {
		panic(err)
	}
}

const indent = "  "

type codeBuffer struct {
	level int
	buf   *bytes.Buffer
}

func (buf *codeBuffer) writef(format string, a ...interface{}) {
	for i := 0; i < buf.level; i++ {
		buf.buf.WriteString(indent)
	}
	buf.buf.WriteString(fmt.Sprintf(format, a...))
}

func (buf *codeBuffer) indent(fn func()) {
	buf.level++
	fn()
	buf.level--
}

var _ measurer.StatementFormatter = (*codeBuffer)(nil)

func (buf *codeBuffer) CaseMaxOut() {
	buf.writef("MaxOut();\n")
}

func (buf *codeBuffer) CaseAddNumBytes(val measurer.Expression) {
	buf.writef("num_bytes_ += %s;\n", formatExpr{val}.String())
}

func (buf *codeBuffer) CaseAddNumHandles(val measurer.Expression) {
	buf.writef("num_handles_ += %s;\n", formatExpr{val}.String())
}

func (buf *codeBuffer) CaseInvoke(id measurer.MethodID, val measurer.Expression) {
	buf.writef("%s(%s);\n", fmtMethodKind(id.Kind), formatExpr{val}.String())
}

func (buf *codeBuffer) CaseGuard(cond measurer.Expression, body *measurer.Block) {
	buf.writef("if (%s) {\n", formatExpr{cond}.String())
	buf.indent(func() {
		buf.writeBlock(body)
	})
	buf.writef("}\n")
}

func (buf *codeBuffer) CaseIterate(local, val measurer.Expression, body *measurer.Block) {
	var deref string
	if val.Nullable() {
		deref = "*"
	}
	buf.writef("for (const auto& %s : %s%s) {\n",
		formatExpr{local}.String(),
		deref, formatExpr{val}.String())
	buf.indent(func() {
		buf.writeBlock(body)
	})
	buf.writef("}\n")
}

func (buf *codeBuffer) CaseSelectVariant(
	val measurer.Expression,
	targetType fidlgen.Name,
	variants map[string]measurer.LocalWithBlock) {

	buf.writef("switch (%s.Which()) {\n", formatExpr{val}.String())
	buf.indent(func() {
		utils.ForAllVariantsInOrder(variants, func(member string, localWithBlock measurer.LocalWithBlock) {
			if member != measurer.UnknownVariant {
				buf.writef("case %s: {\n", fmtKnownVariant(targetType, member))
			} else {
				buf.writef("case %s: {\n", fmtUnknownVariant(targetType))
			}
			buf.indent(func() {
				if local := localWithBlock.Local; local != nil {
					// TODO(https://fxbug.dev/42128549): Improve local vars handling.
					buf.writef("__attribute__((unused)) auto const& %s = %s.%s();\n",
						formatExpr{local},
						formatExpr{val}, fidlgen.ToSnakeCase(member))
				}
				buf.writeBlock(localWithBlock.Body)
				buf.writef("break;\n")
			})
			buf.writef("}\n")
		})

		// In addition to all the member variants, we need to emit special
		// handling for uninitialized unions which are marked as 'invalid'.
		buf.writef("case %s: {\n", fmtInvalidVariant(targetType))
		buf.indent(func() {
			buf.writef("MaxOut();\n")
			buf.writef("break;\n")
		})
		buf.writef("}\n")
	})
	buf.writef("}\n")
}

func (buf *codeBuffer) CaseDeclareMaxOrdinal(local measurer.Expression) {
	buf.writef("int32_t %s = 0;\n", formatExpr{local}.String())
}

func (buf *codeBuffer) CaseSetMaxOrdinal(local, ordinal measurer.Expression) {
	buf.writef("%s = %s;\n", formatExpr{local}.String(), formatExpr{ordinal}.String())
}

func (buf *codeBuffer) writeBlock(b *measurer.Block) {
	b.ForAllStatements(func(stmt *measurer.Statement) {
		stmt.Visit(buf)
	})
}

func (buf *codeBuffer) writeMethod(m *measurer.Method) {
	buf.writef("void %s(const %s& %s) {\n", fmtMethodKind(m.ID.Kind), fmtType(m.ID.TargetType), formatExpr{m.Arg})
	buf.indent(func() {
		buf.writeBlock(m.Body)
	})
	buf.writef("}\n")
}

func fmtMethodKind(kind measurer.MethodKind) string {
	switch kind {
	case measurer.Measure:
		return "Measure"
	case measurer.MeasureOutOfLine:
		return "MeasureOutOfLine"
	case measurer.MeasureHandles:
		return "MeasureHandles"
	default:
		panic(fmt.Sprintf("should not be reachable for method kind %v", kind))
	}
}

func fmtType(name fidlgen.Name) string {
	return fmt.Sprintf("::%s::%s", strings.Join(name.LibraryName().Parts(), "::"), name.DeclarationName())
}

func fmtKnownVariant(name fidlgen.Name, variant string) string {
	return fmt.Sprintf("%s::Tag::k%s", fmtType(name), fidlgen.ToUpperCamelCase(variant))
}

func fmtUnknownVariant(name fidlgen.Name) string {
	return fmt.Sprintf("%s::Tag::kUnknown", fmtType(name))
}

func fmtInvalidVariant(name fidlgen.Name) string {
	return fmt.Sprintf("%s::Tag::Invalid", fmtType(name))
}

type formatExpr struct {
	measurer.Expression
}

func (val formatExpr) String() string {
	return val.Fmt(val)
}

var _ measurer.ExpressionFormatter = formatExpr{}

func (formatExpr) CaseNum(num int) string {
	return fmt.Sprintf("%d", num)
}

func (formatExpr) CaseLocal(name string, _ measurer.TapeKind) string {
	return name
}

func (formatExpr) CaseMemberOf(val measurer.Expression, member string, _ measurer.TapeKind, _ bool) string {
	var accessor string
	if kind := val.AssertKind(measurer.Struct, measurer.Union, measurer.Table); kind != measurer.Struct {
		accessor = "()"
	}
	return fmt.Sprintf("%s%s%s%s", formatExpr{val}, getDerefOp(val), fidlgen.ToSnakeCase(member), accessor)
}

func (formatExpr) CaseFidlAlign(val measurer.Expression) string {
	return fmt.Sprintf("FIDL_ALIGN(%s)", formatExpr{val})
}

func (formatExpr) CaseLength(val measurer.Expression) string {
	var op string
	switch val.AssertKind(measurer.String, measurer.Vector) {
	case measurer.String:
		op = "length"
	case measurer.Vector:
		op = "size"
	}
	return fmt.Sprintf("%s%s%s()", formatExpr{val}, getDerefOp(val), op)
}

func (formatExpr) CaseHasMember(val measurer.Expression, member string) string {
	return fmt.Sprintf("%s%shas_%s()", formatExpr{val}, getDerefOp(val), fidlgen.ToSnakeCase(member))
}

func (formatExpr) CaseMult(lhs, rhs measurer.Expression) string {
	return fmt.Sprintf("%s * %s", formatExpr{lhs}, formatExpr{rhs})
}

func getDerefOp(val measurer.Expression) string {
	if val.Nullable() {
		return "->"
	}
	return "."
}
