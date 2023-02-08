// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package result

import (
	"go.fuchsia.dev/fuchsia/tools/check-licenses/directory"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/license"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"
)

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
	DedupedLicenseData [][]*file.FileData

	// license
	AllPatterns          []*license.Pattern
	AllCopyrightPatterns []*license.Pattern
	AllSearchResults     []*license.SearchResult
	AllowlistPatternMap  map[string][]string
	Unrecognized         *license.Pattern
	Empty                *license.Pattern
}

func getWorldStruct() *world {
	return &world{
		AllFiles: file.AllFiles,

		RootDirectory:  directory.RootDirectory,
		AllDirectories: directory.AllDirectories,

		RootProject:        project.RootProject,
		AllProjects:        project.AllProjects,
		FilteredProjects:   project.FilteredProjects,
		DedupedLicenseData: project.DedupedLicenseData,

		AllPatterns:          license.AllPatterns,
		AllCopyrightPatterns: license.AllCopyrightPatterns,
		AllSearchResults:     license.AllSearchResults,
		AllowlistPatternMap:  license.AllowlistPatternMap,
		Unrecognized:         license.Unrecognized,
		Empty:                license.Empty,
	}
}
