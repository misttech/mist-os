// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package notice

import (
	"bufio"
	"bytes"
	"fmt"
	"strings"
)

// Some NOTICE files use a single delimiter.
//
// --------------------------------------------------------------------------------
// library name
//
// license text
//
// --------------------------------------------------------------------------------
// library name
// library name
// library name
//
// license text
//
// etc

const (
	oneDelimiterParserStateLibraryName = iota
	oneDelimiterParserStateLicense     = iota
)

func ParseOneDelimiter(path string, content []byte) ([]*Data, error) {
	var licenseDelimiter = []byte("--------------------------------------------------------------------------------")

	r := bytes.NewReader(content)

	var builder strings.Builder
	var licenses []*Data
	license := &Data{}

	parserState := oneDelimiterParserStateLibraryName

	scanner := bufio.NewScanner(r)
	lineNumber := 0
	for scanner.Scan() {
		lineNumber = lineNumber + 1
		line := scanner.Bytes()
		switch parserState {
		case oneDelimiterParserStateLibraryName:
			if bytes.Equal(line, []byte("")) {
				parserState = oneDelimiterParserStateLicense
			} else {
				if license.LibraryName == "" {
					license.LibraryName = string(line)
					license.LineNumber = lineNumber
				} else {
					license.LibraryName = fmt.Sprintf("%s\n%s", license.LibraryName, string(line))
				}
			}

		case oneDelimiterParserStateLicense:
			if bytes.Equal(line, licenseDelimiter) {
				license.LicenseText = []byte(builder.String())
				licenses = append(licenses, license)
				license = &Data{}
				builder.Reset()
				parserState = oneDelimiterParserStateLibraryName
			} else {
				builder.Write(line)
				builder.WriteString("\n")
			}
		}
	}

	return mergeDuplicates(licenses), nil
}
