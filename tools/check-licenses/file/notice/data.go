// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package notice

import (
	"fmt"
)

type Data struct {
	LibraryName string `json:"libraryName"`
	LicenseText []byte `json:"licenseText"`
	LineNumber  int    `json:"lineNumber"`
}

func mergeDuplicates(licenses []*Data) []*Data {
	set := make(map[string]bool, 0)
	dedupedLicenses := make([]*Data, 0)

	for _, l := range licenses {
		key := fmt.Sprintf("%s ||| %s", l.LibraryName, string(l.LicenseText))
		if _, ok := set[key]; !ok {
			set[key] = true
			dedupedLicenses = append(dedupedLicenses, l)
		}
	}

	return dedupedLicenses
}
