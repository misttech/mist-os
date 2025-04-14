// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel2gn

import (
	"fmt"
	"go.starlark.net/syntax"
)

// defaultBazelSelectCondition is the default match-the-rest select condition.
const defaultBazelSelectCondition = "//conditions:default"

// bazelSelectConditionToGN maps from Bazel select conditions (the keys of the
// dictionaries to select calls) to GN condition variables (the condition to
// check in if statements).
var bazelSelectConditionToGN = map[string]string{
	"@platforms//os:fuchsia": "is_fuchsia",
	"@platforms//os:linux":   "is_linux",
}

// Returns true iff select call is found in the subtree of `expr`.
func hasSelectCall(expr syntax.Expr) bool {
	if isSelectCall(expr) {
		return true
	}
	binaryExpr, ok := expr.(*syntax.BinaryExpr)
	if ok {
		return hasSelectCall(binaryExpr.X) || hasSelectCall(binaryExpr.Y)
	}
	return false
}

// Returns true iff the input expression is a select call.
func isSelectCall(expr syntax.Expr) bool {
	fn, ok := expr.(*syntax.CallExpr)
	if !ok {
		return false
	}
	return fn.Fn.(*syntax.Ident).Name == "select"
}

// Converts list concatenation with select calls in them to GN.
func listConcatWithSelectToGN(attrName string, expr syntax.Expr, transformers []transformer) ([]string, error) {
	for _, ts := range transformers {
		var err error
		expr, err = ts(expr)
		if err != nil {
			return nil, fmt.Errorf("applying special handler before converting list concatenation with select: %v", err)
		}
	}

	switch v := expr.(type) {
	case *syntax.CallExpr:
		return selectToGN(attrName, "+=", v, transformers)
	case *syntax.ListExpr:
		l, err := listExprToGN(v, transformers)
		if err != nil {
			return nil, fmt.Errorf("converting list expression: %v", err)
		}
		ret := append(
			[]string{fmt.Sprintf("%s += %s", attrName, l[0])},
			l[1:]...,
		)
		return ret, nil
	case *syntax.BinaryExpr:
		if v.Op != syntax.PLUS {
			return nil, fmt.Errorf("only list concatenation are support, found unexpected operator %s", v.Op)
		}
		// Traversal order is important here. Left-first to ensure the order of list
		// items, which is important in attributes like copts.
		lhs, err := listConcatWithSelectToGN(attrName, v.X, transformers)
		if err != nil {
			return nil, fmt.Errorf("converting lhs of concatenation: %v", err)
		}
		rhs, err := listConcatWithSelectToGN(attrName, v.Y, transformers)
		if err != nil {
			return nil, fmt.Errorf("converting rhs of concatenation: %v", err)
		}
		return append(lhs, rhs...), nil
	default:
		return nil, fmt.Errorf("converting list concatenation with select to GN, want call expression, binary expression, or list expression, got %T", expr)
	}
}

// Convert select nodes (which are syntax.CallExpr) to GN fragments.
func selectToGN(attrName string, op string, expr *syntax.CallExpr, transformers []transformer) ([]string, error) {
	fn := expr.Fn.(*syntax.Ident)
	if fn.Name != "select" {
		return nil, fmt.Errorf("want select call, got %s", fn.Name)
	}
	if len(expr.Args) == 0 {
		return nil, fmt.Errorf("no args found for select, expect at least one arg")
	}

	var ret []string
	for _, entry := range expr.Args[0].(*syntax.DictExpr).List {
		e := entry.(*syntax.DictEntry)
		key, ok := e.Key.(*syntax.Literal)
		if !ok {
			return nil, fmt.Errorf("only literals are supported as keys in dictionaries in selects")
		}

		// key.Raw is quoted, so unquote to get the string value.
		selectCondition := key.Raw[1 : len(key.Raw)-1]
		if selectCondition == defaultBazelSelectCondition {
			if len(ret) == 0 {
				return nil, fmt.Errorf("default select condition %q found with no other matching cases", selectCondition)
			}
			ret[len(ret)-1] += " else {"
		} else {
			gnCondition, ok := bazelSelectConditionToGN[selectCondition]
			if !ok {
				return nil, fmt.Errorf("unknown Bazel select condition %q in select", selectCondition)
			}
			if len(ret) > 0 {
				ret[len(ret)-1] += fmt.Sprintf(" else if (%s) {", gnCondition)
			} else {
				ret = append(ret, fmt.Sprintf("if (%s) {", gnCondition))
			}
		}

		valueInGN, err := exprToGN(e.Value, transformers)
		if err != nil {
			return nil, fmt.Errorf("converting dictionary value in select to GN: %v", err)
		}
		ret = append(ret, fmt.Sprintf("  %s %s %s", attrName, op, valueInGN[0]))
		ret = append(ret, indent(valueInGN[1:], 1)...)
		ret = append(ret, "}")
	}

	if len(expr.Args) > 1 {
		noMatchError, ok := expr.Args[1].(*syntax.BinaryExpr)
		if !ok || noMatchError.X.(*syntax.Ident).Name != "no_match_error" {
			return nil, fmt.Errorf("the second arg of select must be `no_match_error`, got %q", noMatchError.X.(*syntax.Ident).Name)
		}

		errStr, ok := noMatchError.Y.(*syntax.Literal)
		if !ok {
			return nil, fmt.Errorf("value of `no_match_error` must be a literal string, got %T", noMatchError.Y)
		}

		ret[len(ret)-1] += " else {"
		ret = append(ret, []string{
			// NOTE: raw is quoted so we don't need to quote again.
			fmt.Sprintf("  assert(false, %s)", errStr.Raw),
			"}",
		}...)
	}
	return ret, nil
}
