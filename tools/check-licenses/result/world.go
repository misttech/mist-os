// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package result

import (
	"fmt"
	"sort"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/directory"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/license"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"
)

// World is used to store all of the current run's information in a single object
// that can be processed by go templates.
//
// TODO(fxbug.dev/126804): consider moving this into a separate build step.
type world struct {
	// file
	AllFiles map[string]*file.File

	// directory
	RootDirectory  *directory.Directory
	AllDirectories map[string]*directory.Directory

	// project
	RootProject        *project.Project
	AllProjects        map[string]*project.Project
	FilteredProjects   map[string]*project.Project
	LicenseData        []*DedupedLicense
	DedupedLicenseData []*DedupedLicense

	// license
	AllPatterns                 []*license.Pattern
	AllCopyrightPatterns        []*license.Pattern
	AllSearchResults            []*license.SearchResult
	AllLicenseFileSearchResults []*license.SearchResult
	AllowlistPatternMap         map[string][]string
	Unrecognized                *license.Pattern
	Empty                       *license.Pattern
}

type DedupedLicense struct {
	LibraryNames []string
	Text         string
}

func getWorldStruct() *world {
	dedupedLicenseDataMap := make(map[string]*DedupedLicense, 0)
	licenseDataList := make([]*DedupedLicense, 0)
	dedupedLicenseDataList := make([]*DedupedLicense, 0)

	// Dedup all license texts by putting them in a hashmap.
	for _, p := range project.FilteredProjects {
		for _, l := range p.LicenseFiles {
			data, err := l.Data()
			if err != nil {
				fmt.Printf("Failed to get data for %s\n", l.RelPath())
				continue
			}

			for _, d := range data {
				hash := d.Hash()
				dl := &DedupedLicense{
					LibraryNames: []string{d.LibraryName()},
					Text:         string(d.Data()),
				}
				licenseDataList = append(licenseDataList, dl)

				if value, ok := dedupedLicenseDataMap[hash]; !ok {
					dedupedLicenseDataMap[hash] = dl
				} else {
					value.LibraryNames = append(value.LibraryNames, d.LibraryName())
				}
			}
		}
	}

	// Deterministically loop over the deduped data by storing the results
	// in a sorted list.
	for _, v := range dedupedLicenseDataMap {
		sort.Strings(v.LibraryNames)
		dedupedLicenseDataList = append(dedupedLicenseDataList, v)
	}
	sort.Slice(licenseDataList, func(i, j int) bool {
		return licenseDataList[i].Text < licenseDataList[j].Text
	})
	sort.Slice(dedupedLicenseDataList, func(i, j int) bool {
		return dedupedLicenseDataList[i].Text < dedupedLicenseDataList[j].Text
	})

	return &world{
		AllFiles: file.AllFiles,

		RootDirectory:  directory.RootDirectory,
		AllDirectories: directory.AllDirectories,

		RootProject:        project.RootProject,
		AllProjects:        project.AllProjects,
		FilteredProjects:   project.FilteredProjects,
		LicenseData:        licenseDataList,
		DedupedLicenseData: dedupedLicenseDataList,

		AllPatterns:                 license.AllPatterns,
		AllCopyrightPatterns:        license.AllCopyrightPatterns,
		AllSearchResults:            license.AllSearchResults,
		AllLicenseFileSearchResults: license.AllLicenseFileSearchResults,
		AllowlistPatternMap:         license.AllowlistPatternMap,
		Unrecognized:                license.Unrecognized,
		Empty:                       license.Empty,
	}
}
