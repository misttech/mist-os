// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package fidlgen

import (
	"strings"
	"unicode"
	"unicode/utf8"
)

// The functions in this file convert names between snake_case, ALL_CAPS_SNAKE,
// UpperCamelCase, lowerCamelCase, kCamelCase, and "friendly case". They accept
// inputs in any style, split into parts, and recombine with the desired style.
// See "RFC-0040: Identifier Uniqueness" for details on how name-part boundaries
// are calculated.
//
// TODO(https://fxbug.dev/42177200): Fix any discrepancies with RFC-0040. In particular,
// ensure that all these functions are idempotent.

// nameParts breaks an identifier into parts recognized according to RFC-0040
// which can be recombined into identifiers in different case systems.
//
// Splits the string according to what is recognized as boundaries for both
// snake case and camel case, allowing it to be transformed into either case
// system regardless of the original case. Note that runs of multiple uppercase
// characters are treated as a single part, so all-caps abbreviations will be
// grouped. For example, "HTTPExample" will be split into "HTTP" and "Example".
func nameParts(name string) []string {
	var parts []string
	for _, namePart := range strings.Split(name, "_") {
		partStart := 0
		lastRune, _ := utf8.DecodeRuneInString(namePart)
		lastRuneStart := 0
		for i, curRune := range namePart {
			if i == 0 {
				continue
			}
			if unicode.IsUpper(curRune) && !unicode.IsUpper(lastRune) {
				parts = append(parts, namePart[partStart:i])
				partStart = i
			}
			if !(unicode.IsUpper(curRune) || unicode.IsDigit(curRune)) && unicode.IsUpper(lastRune) && partStart != lastRuneStart {
				parts = append(parts, namePart[partStart:lastRuneStart])
				partStart = lastRuneStart
			}
			lastRuneStart = i
			lastRune = curRune
		}
		parts = append(parts, namePart[partStart:])
	}
	return parts
}

// ToSnakeCase converts an identifier to RFC-0040 canonical snake_case style.
// Works independent of which case the identifier is originally in.
func ToSnakeCase(name string) string {
	parts := nameParts(name)
	for i := range parts {
		parts[i] = strings.ToLower(parts[i])
	}
	return strings.Join(parts, "_")
}

// ToUpperCamelCase converts an identifier to RFC-0040 UpperCamelCase style.
// Works independent of which case the identifier is originally in. Note that
// canonical identifiers use title case for abbreviations, so e.g. HTTPExample
// will become HttpExample.
func ToUpperCamelCase(name string) string {
	parts := nameParts(name)
	for i := range parts {
		parts[i] = strings.Title(strings.ToLower(parts[i]))
		if parts[i] == "" {
			parts[i] = "_"
		}
	}
	return strings.Join(parts, "")
}

// ToLowerCamelCase converts an identifier to RFC-0040 lowerCamelCase style.
// Works independent of which case the identifier is originally in. Note that
// canonical identifiers use title case for abbreviations, so e.g. ExampleHTTP
// will become ExampleHttp.
func ToLowerCamelCase(name string) string {
	parts := nameParts(name)
	for i := range parts {
		if i == 0 {
			parts[i] = strings.ToLower(parts[i])
		} else {
			parts[i] = strings.Title(strings.ToLower(parts[i]))
		}
		if parts[i] == "" {
			parts[i] = "_"
		}
	}
	return strings.Join(parts, "")
}

// ToFriendlyCase converts an identifier to RFC-0040 "friendly case" style (like
// snake case, but with spaces). Works independent of which case the identifier
// is originally in.
func ToFriendlyCase(name string) string {
	parts := nameParts(name)
	for i := range parts {
		parts[i] = strings.ToLower(parts[i])
	}
	return strings.Join(parts, " ")
}

// ConstNameToAllCapsSnake converts a const name from kCamelCase to
// ALL_CAPS_SNAKE style
func ConstNameToAllCapsSnake(name string) string {
	parts := nameParts(RemoveLeadingK(name))
	for i := range parts {
		parts[i] = strings.ToUpper(parts[i])
	}
	return strings.Join(parts, "_")
}

// ConstNameToKCamelCase converts a const name to kCamelCase style
func ConstNameToKCamelCase(name string) string {
	return "k" + ToUpperCamelCase(RemoveLeadingK(name))
}

// RemoveLeadingK removes a leading 'k' if the second character is upper-case,
// otherwise returns the argument
func RemoveLeadingK(name string) string {
	if len(name) >= 2 && name[0] == 'k' {
		secondRune, _ := utf8.DecodeRuneInString(name[1:])
		if unicode.IsUpper(secondRune) {
			name = name[1:]
		}
	}
	return name
}
