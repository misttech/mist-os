// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel2gn

import (
	"fmt"
	"maps"
	"regexp"

	"go.starlark.net/syntax"
)

// indentPrefix is the string value used to indent a line by one level.
//
// NOTE: This prefix is only used by the internal representation of this package.
// The final output is formatted by `gn format`.
const indentPrefix = "\t"

// clearAnnotation is a comment annotation that indicates the assignment is an explicit clear.
const clearAnnotation = "# bazel2gn: clear"

// bazelRuleToGNTemplate maps from Bazel rule names to GN template names. They can
// be the same if Bazel and GN shared the same template name.
//
// This map is also used to check known Bazel rules that can be converted to GN.
// i.e. Bazel rules not found in this map is not supported by bazel2gn yet.
var bazelRuleToGNTemplate = map[string]string{
	// Go
	"go_binary":  "go_binary",
	"go_library": "go_library",
	"go_test":    "go_test",

	// Rust
	"rust_binary":     "rustc_binary",
	"rust_library":    "rustc_library",
	"rustc_binary":    "rustc_binary",
	"rustc_library":   "rustc_library",
	"rust_proc_macro": "rustc_macro",

	// C++
	"cc_library": "source_set",

	// IDK & SDK
	"idk_cc_source_library": "sdk_source_set",
	"sdk_host_tool":         "sdk_host_tool",

	// Other
	"install_host_tools": "install_host_tools",
	"package":            "package",
}

// attrsToOmitByRules stores a mapping from known Bazel rules to attributes to
// omit when converting them to GN.
var attrsToOmitByRules = map[string]map[string]bool{
	"go_library": {
		// In GN we default cgo to true when compiling Go code, and explicitly disable
		// it in very few places. However, in Bazel, cgo defaults to false, and
		// require users to explicitly set when C sources are included.
		"cgo": true,
	},
}

// Common Bazel attributes that use different names in GN.
var commonAttrMap = map[string]string{
	"srcs": "sources",
	"hdrs": "public",
}

// ccAttrMap maps from attribute names in Bazel CC rules to GN parameter names.
// This map only includes attributes that have different names in Bazel and GN.
var ccAttrMap = map[string]string{
	"copts":               "configs",
	"deps":                "public_deps",
	"implementation_deps": "deps",
}

// rustAttrMap maps from attribute name in Bazel Rust rules to GN parameter names.
// This map only includes attributes that have different names in Bazel and GN.
var rustAttrMap = map[string]string{
	"crate_features": "features",
}

// idkAttrMap maps from attribute name in Bazel IDK rules to GN parameter names.
// This map only includes attributes that have different names in Bazel and GN.
var idkAttrMap = map[string]string{
	"api_area":            "sdk_area",
	"deps":                "public_deps",
	"idk_name":            "sdk_name",
	"implementation_deps": "deps",
}

// A mapping from Bazel rule names to attribute mappings.
// Attribute mappings map from Bazel rule attributes that use different names in GN.
var attrMapsByRules = map[string]map[string]string{
	// C++
	"cc_library": ccAttrMap,

	// Rust
	"rust_binary":     rustAttrMap,
	"rust_library":    rustAttrMap,
	"rust_proc_macro": rustAttrMap,
	"rustc_binary":    rustAttrMap,
	"rustc_library":   rustAttrMap,

	// IDK
	"idk_cc_source_library": idkAttrMap,
}

// These identifiers with the same meanings are represented differently in Bazel
// and GN. specialIdentifiers maps from their Bazel representations to GN
// representations.
var specialIdentifiers = map[string]string{
	"True":  "true",
	"False": "false",
}

// specialTokens maps from special tokens in Bazel to their GN equivalents.
var specialTokens = map[syntax.Token]string{
	syntax.AND: "&&",
	syntax.OR:  "||",
}

var bazelConstraintsToGNConditions = map[string]string{
	"HOST_CONSTRAINTS": "is_host",
}

var thirdPartyRustCrateRE = regexp.MustCompile(`^"\/\/third_party\/rust_crates.+:`)

// coptToConfig maps from Bazel copt values to configs to use in GN.
var coptToConfig = map[string]string{
	"-Wno-implicit-fallthrough": "//build/config:Wno-implicit-fallthrough",
}

// attrGNAssignmentOps maps from GN attribute names to the assignment operators to use in GN.
//
// NOTE: Entries in this map should be clearly documented.
var attrGNAssignmentOps = map[string]string{
	// `configs` in GN are rarely (never?) empty lists because we set them in BUILDCONFIG.gn.
	// Trying to overwrite a non-empty list in GN with a non-empty value will fail.
	// Simply replacing assignment with `+=` works for the initial use cases we need.
	// More complex mechanism may be required if we need to selectively overwrite config assignments.
	"configs": "+=",
}

// mustMergeMaps merges two input maps and return a new one with keys and values from
// both inputs.
//
// This function panics if m1 and m2 have duplicate keys. All these cases should be
// caught at build time so it's OK to panic here.
func mustMergeMaps(m1, m2 map[string]string) map[string]string {
	var m = maps.Clone(m1)
	for k, v := range m2 {
		if _, ok := m[k]; ok {
			panic(fmt.Sprintf("Duplicate key when merging maps: %q", k))
		}
		m[k] = v
	}
	return m
}
