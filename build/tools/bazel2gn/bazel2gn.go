// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel2gn

import (
	"errors"
	"fmt"
	"strings"

	"go.starlark.net/syntax"
)

// indentPrefix is the string value used to indent a line by one level.
const indentPrefix = "  "

// knownRules assigns true to known rules that can be converted from Bazel to GN.
var knownRules = map[string]bool{
	"go_binary":          true,
	"go_library":         true,
	"go_test":            true,
	"install_host_tools": true,
	"sdk_host_tool":      true,
	"package":            true,
}

// attrsToOmitByRules stores a mapping from known rules to attributes to omit when
// converting them to GN.
var attrsToOmitByRules = map[string]map[string]bool{
	"go_library": {
		// In GN we default cgo to true when compiling Go code, and explicitly disable
		// it in very few places. However, in Bazel, cgo defaults to false, and
		// require users to explicitly set when C sources are included.
		"cgo": true,
	},
}

// These identifiers with the same meanings are represented differently in Bazel
// and GN. specialIdentifiers maps from their Bazel representations to GN
// representations.
var specialIdentifiers = map[string]string{
	"True":  "true",
	"False": "false",
	"srcs":  "sources",
}

// specialTokens maps from special tokens in Bazel to their GN equivalents.
var specialTokens = map[syntax.Token]string{
	syntax.AND: "&&",
	syntax.OR:  "||",
}

var bazelConstraintsToGNConditions = map[string]string{
	"HOST_CONSTRAINTS": "is_host",
}

// indent indents input lines by input levels.
func indent(lines []string, level int) []string {
	var indented []string
	prefix := strings.Repeat(indentPrefix, level)
	for _, l := range lines {
		indented = append(indented, prefix+l)
	}
	return indented
}

// StmtToGN converts a Bazel statement [0] to GN.
//
// [0] https://github.com/bazelbuild/starlark/blob/master/spec.md#statements
func StmtToGN(stmt syntax.Stmt) ([]string, error) {
	switch v := stmt.(type) {
	case *syntax.LoadStmt:
		// Load statements are ignored during conversion.
		return nil, nil
	case *syntax.ExprStmt:
		return exprToGN(v.X, nil)
	default:
		return nil, fmt.Errorf("statement of type %T is not supported to be converted to GN, node details: %#v", stmt, stmt)
	}
}

// transformer is a function type that can be used by `exprToGN` to apply
// special transformations to expression nodes before conversion.
//
// This is useful for rewriting certain string values, recording values during
// traversal, or even restructuring the syntax tree.
type transformer func(syntax.Expr) (syntax.Expr, error)

// exprToGN converts a Bazel expression [0] to GN.
//
// It applies input transformers first before delegating to more specific
// conversion functions based on expression type.
//
// [0] https://github.com/bazelbuild/starlark/blob/master/spec.md#expressions
func exprToGN(expr syntax.Expr, transformers []transformer) ([]string, error) {
	for _, ts := range transformers {
		var err error
		expr, err = ts(expr)
		if err != nil {
			return nil, fmt.Errorf("applying special handler before converting expression: %v", err)
		}
	}

	switch v := expr.(type) {
	case *syntax.CallExpr:
		// NOTE: I'm not sure whether we need to plumb transformers here, so far it
		// is not necessary. callExprToGN should be a top-level entry point for
		// macro and rules.
		return callExprToGN(v)
	case *syntax.BinaryExpr:
		return binaryExprToGN(v, transformers)
	case *syntax.Ident:
		return identToGN(v)
	case *syntax.Literal:
		return []string{v.Raw}, nil
	case *syntax.ListExpr:
		return listExprToGN(v, transformers)
	default:
		return nil, fmt.Errorf("expression of type %T is not supported when converting to GN, node details: %#v", expr, expr)
	}
}

// identToGN converts a Bazel identifier to GN.
func identToGN(ident *syntax.Ident) ([]string, error) {
	val, ok := specialIdentifiers[ident.Name]
	if !ok {
		val = ident.Name
	}
	return []string{val}, nil
}

func targetCompatibleWithToGNConditions(expr syntax.Expr) ([]string, error) {
	switch v := expr.(type) {
	case *syntax.Ident:
		gnCondition, ok := bazelConstraintsToGNConditions[v.Name]
		if !ok {
			return nil, fmt.Errorf("unsupported target_compatible_with variable: %v", v.Name)
		}
		return []string{gnCondition}, nil
	default:
		return nil, fmt.Errorf("unsupported type %T as value to target_compatible_with in Bazel, node details: %#v", expr, expr)
	}
}

// bazelVisibilityToGN converts Bazel visibility values [0] to GN [1].
//
// NOTE: Bazel visibility is based on package groups [2], while GN visibility is
// based on target. However it should be possible to create matching groups in
// GN for the exact same visibility control in the most granular cases.
//
// [0] https://bazel.build/concepts/visibility#visibility-specifications
// [1] https://gn.googlesource.com/gn/+/main/docs/reference.md#var_visibility
// [2] https://bazel.build/reference/be/functions#package_group
func bazelVisibilityToGN(expr syntax.Expr) (syntax.Expr, error) {
	lit, ok := expr.(*syntax.Literal)
	if !ok {
		return expr, nil
	}
	switch {
	case lit.Raw == `"//visibility:public"`:
		lit.Raw = `"*"`
	case lit.Raw == `"//visibility:private"`:
		lit.Raw = `":*"`
	case strings.HasSuffix(lit.Raw, `:__pkg__"`):
		lit.Raw = strings.ReplaceAll(lit.Raw, `:__pkg__"`, `:*"`)
	case strings.HasSuffix(lit.Raw, `:__subpackages__"`):
		lit.Raw = strings.ReplaceAll(lit.Raw, `:__subpackages__"`, `/*"`)
	default:
		// This is a Bazel visibility on a package group, it should stay unchanged.
		// In GN there should be a target matching the path of this package group.
	}
	return lit, nil
}

// callExprToGN converts a Bazel call expression [0] to GN. These calls should
// be macro or Bazel rules known to the converter.
//
// [0] https://github.com/bazelbuild/starlark/blob/master/spec.md#function-and-method-calls
func callExprToGN(expr *syntax.CallExpr) ([]string, error) {
	fn := expr.Fn.(*syntax.Ident)
	if !knownRules[fn.Name] {
		return nil, fmt.Errorf("%s is not a known Bazel rule to convert to GN", fn.Name)
	}

	// TODO(jayzhuang): Handle package level settings, e.g. visibility.
	if fn.Name == "package" {
		return nil, nil
	}

	attrsToOmit := attrsToOmitByRules[fn.Name]

	// Loops through all arguments to handle special ones first.
	var name string
	var remainingArgs []*syntax.BinaryExpr
	var wrappingConditions []string
	for _, arg := range expr.Args {
		binaryExpr, ok := arg.(*syntax.BinaryExpr)
		if !ok || binaryExpr.Op != syntax.EQ {
			return nil, fmt.Errorf("only attribute assignments (e.g. `attr = value`) are allowed in Bazel targets to be converted to GN, got %#v", arg)
		}
		ident, ok := binaryExpr.X.(*syntax.Ident)
		if !ok {
			return nil, fmt.Errorf("unexpected node type on the left hand side of binary expression in target definition, want syntax.Ident, got %T", binaryExpr.X)
		}
		if attrsToOmit[ident.Name] {
			continue
		}
		if ident.Name == "name" {
			lines, err := exprToGN(binaryExpr.Y, nil)
			if err != nil {
				return nil, fmt.Errorf("converting target name: %v", err)
			}
			name = strings.Join(lines, "\n")
			continue
		}
		if ident.Name == "target_compatible_with" {
			var err error
			wrappingConditions, err = targetCompatibleWithToGNConditions(binaryExpr.Y)
			if err != nil {
				return nil, fmt.Errorf("converting Bazel target_compatible_with to GN conditions: %v", err)
			}
			continue
		}
		remainingArgs = append(remainingArgs, binaryExpr)
	}
	if name == "" {
		return nil, errors.New("missing `name` attribute in Bazel target")
	}

	ret := []string{fmt.Sprintf("%s(%s) {", fn.Name, name)}

	// Loop through all args again to actually build the content of this target.
	for _, arg := range remainingArgs {
		lines, err := attrAssignmentToGN(arg)
		if err != nil {
			return nil, fmt.Errorf("converting Bazel attribute to GN: %v", err)
		}
		ret = append(ret, indent(lines, 1)...)
	}

	ret = append(ret, "}")
	if len(wrappingConditions) > 0 {
		ret = append([]string{
			fmt.Sprintf("if (%s) {", strings.Join(wrappingConditions, " && ")),
		}, indent(ret, 1)...)
		ret = append(ret, "}")
	}
	return ret, nil
}

// attrAssignmentToGN converts a Bazel assignment [0] to GN. These assignments
// are used to assign values to fields during target definitions in Bazel.
//
// This function handles the special cases in attribute assignments, e.g. when
// select calls are involved. This is done through applying node transformers
// funcs during the traversal.
//
// NOTE: Assignment is a special binary expression with operator =.
//
// [0] https://github.com/bazelbuild/starlark/blob/master/spec.md#assignments
func attrAssignmentToGN(expr *syntax.BinaryExpr) ([]string, error) {
	lhs, ok := expr.X.(*syntax.Ident)
	if !ok {
		return nil, fmt.Errorf("expecting an identifier on the left hand side of attribute assignment, got %T", expr.X)
	}
	attrName := lhs.Name

	var transformers []transformer
	if attrName == "visibility" {
		transformers = append(transformers, bazelVisibilityToGN)
	}

	return binaryExprToGN(expr, transformers)
}

// binaryExprToGN converts a general Bazel binary expression [0] to GN.
//
// [0] https://github.com/bazelbuild/starlark/blob/master/spec.md#binary-operators
func binaryExprToGN(expr *syntax.BinaryExpr, transformers []transformer) ([]string, error) {
	lhs, err := exprToGN(expr.X, transformers)
	if err != nil {
		return nil, fmt.Errorf("converting lhs of binary expression: %v", err)
	}
	if len(lhs) == 0 {
		return nil, errors.New("lhs of binary expression is unexpectedly empty")
	}

	rhs, err := exprToGN(expr.Y, transformers)
	if err != nil {
		return nil, fmt.Errorf("converting rhs of binary expression: %v", err)
	}
	if len(rhs) == 0 {
		return nil, errors.New("rhs hand side of binary expression is unexpectedly empty")
	}

	ret := lhs[:len(lhs)-1]
	ret = append(ret, fmt.Sprintf("%s %s %s", lhs[len(lhs)-1], opToGN(expr.Op), rhs[0]))
	ret = append(ret, rhs[1:]...)
	return ret, nil
}

func opToGN(op syntax.Token) string {
	ret, ok := specialTokens[op]
	if !ok {
		return op.String()
	}
	return ret
}

// listExprToGN converts a Bazel list expression [0] to GN.
//
// [0] https://github.com/bazelbuild/starlark/blob/master/spec.md#lists
func listExprToGN(expr *syntax.ListExpr, transformers []transformer) ([]string, error) {
	ret := []string{"["}

	for _, elm := range expr.List {
		elmLines, err := exprToGN(elm, transformers)
		if err != nil {
			return nil, fmt.Errorf("converting list element: %v", err)
		}
		if len(elmLines) == 0 {
			continue
		}
		elmLines[len(elmLines)-1] = elmLines[len(elmLines)-1] + ","
		ret = append(ret, indent(elmLines, 1)...)
	}

	ret = append(ret, "]")
	return ret, nil
}
