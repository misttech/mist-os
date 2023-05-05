// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package result

import (
	"fmt"
	"net/http"
	"sort"
	"strings"
	"time"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/directory"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/license"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project/readme"
)

type Check struct {
	Name      string          `json: "name"`
	Allowlist map[string]bool `json:"allowlist"`
}

func RunChecks() error {
	if err := AllFuchsiaAuthorSourceFilesMustHaveCopyrightHeaders(); err != nil {
		return err
	}
	if err := AllLicenseTextsMustBeRecognized(); err != nil {
		return err
	}
	if err := AllLicensePatternUsagesMustBeApproved(); err != nil {
		return err
	}
	if err := AllComplianceWorksheetLinksAreGood(); err != nil {
		return err
	}
	if err := AllProjectsMustHaveALicense(); err != nil {
		return err
	}
	if err := AllFilesAndFoldersMustBeIncludedInAProject(); err != nil {
		fmt.Printf("========\nWarning: this will soon become an error\n\n%v\n========\n", err)
	}
	if err := AllReadmeFuchsiaFilesMustBeFormattedCorrectly(); err != nil {
		fmt.Printf("========\nWarning: this will soon become an error\n\n%v\n========\n", err)
	}
	return nil
}

// =============================================================================

func AllFuchsiaAuthorSourceFilesMustHaveCopyrightHeaders() error {
	var b strings.Builder
	b.WriteString("All source files owned by The Fuchsia Authors must contain a copyright header.\n")
	b.WriteString("The following files have missing or incorrectly worded copyright header information:\n\n")

	var fuchsia *project.Project
	for _, p := range project.FilteredProjects {
		if p.Root == "." || p.Root == Config.FuchsiaDir {
			fuchsia = p
			break
		}
	}

	if fuchsia == nil {
		return fmt.Errorf("Couldn't find Fuchsia project to verify this check!!\n")
	}
	count := 0
	for _, f := range fuchsia.SearchableRegularFiles {
		fdList, err := f.Data()
		if err != nil {
			return fmt.Errorf("Found a file that hasn't been parsed yet?? %v | %v\n", f.AbsPath(), err)
		}
	OUTER:
		for _, fd := range fdList {
			for _, p := range license.AllCopyrightPatterns {
				if _, ok := p.PreviousMatches[fd.Hash()]; ok {
					continue OUTER
				}
			}
			b.WriteString(fmt.Sprintf("-> %v\n", fd.File().RelPath()))
			count = count + 1
		}
	}
	b.WriteString(fmt.Sprintf("\nPlease add the standard Fuchsia copyright header info to the above %v files.\n", count))
	if count > 0 {
		return fmt.Errorf(b.String())
	}
	return nil
}

func AllLicenseTextsMustBeRecognized() error {
	name := "AllLicenseTextsMustBeRecognized"

	var foundUnrecognizedMatch bool
	var b strings.Builder

	// Retrieve allowlists from config files
	allowlist := make(map[string]bool, 0)
	for _, c := range Config.Checks {
		if c.Name == name {
			for k, v := range c.Allowlist {
				allowlist[k] = v
			}
		}
	}

	b.WriteString("Found unrecognized license texts - please add the relevant license pattern(s) to //tools/check-licenses/license/patterns/* and have it(them) reviewed by the OSRB team:\n\n")
	for _, m := range license.Unrecognized.Matches {
		if _, ok := allowlist[m.File().RelPath()]; ok {
			continue
		}

		foundUnrecognizedMatch = true
		b.WriteString(fmt.Sprintf("-> Line %v of %v\n", m.LineNumber(), m.File().RelPath()))
		b.WriteString(fmt.Sprintf("\n%v\n\n", string(m.Data())))
	}

	if foundUnrecognizedMatch {
		return fmt.Errorf(b.String())
	}
	return nil
}

func AllLicensePatternUsagesMustBeApproved() error {
	name := "AllLicensePatternUsagesMustBeApproved"

	var b strings.Builder

	// Retrieve allowlists from config files
	allowlist := make(map[string]bool, 0)
	for _, c := range Config.Checks {
		if c.Name == name {
			for k, v := range c.Allowlist {
				allowlist[k] = v
			}
		}
	}

OUTER:
	for _, sr := range license.AllSearchResults {
		if _, ok := allowlist[sr.LicenseData.File().RelPath()]; ok {
			continue
		}

		// TODO(fxbug.dev/126193): Remove this clause.
		if sr.Pattern.Name == "_unrecognized" {
			continue
		}
		if sr.Pattern.Name == "_empty" {
			continue
		}
		if sr.Pattern.Category == "approved_production" {
			continue
		}
		if sr.Pattern.Category == "approved_development" {
			if !(strings.Contains(Config.BuildInfoProduct, "user") || strings.Contains(Config.BuildInfoBoard, "user")) {
				continue
			}
		}

		filepath := sr.LicenseData.File().RelPath()
		for _, allowlist := range sr.Pattern.Allowlist {
			for _, entry := range allowlist.Entries {
				for _, project := range entry.Projects {
					if sr.ProjectRoot == project {
						continue OUTER
					}
				}
			}
		}
		b.WriteString(fmt.Sprintf("File %v from project %s was not approved to use license pattern %v\n",
			filepath, sr.ProjectRoot, sr.Pattern.RelPath))
	}

	result := b.String()
	if len(result) > 0 {
		return fmt.Errorf("Encountered license texts that were not approved for usage:\n%v", result)
	}
	return nil
}

func AllComplianceWorksheetLinksAreGood() error {
	if !Config.CheckURLs {
		fmt.Println("Not checking URLs")
		return nil
	}

	numBadLinks := 0
	checkedURLs := make(map[string]bool, 0)
	for _, sr := range license.AllSearchResults {
		if strings.HasPrefix(sr.Pattern.Name, "_") {
			fmt.Printf("Not checking URL for pattern %s\n", sr.Pattern.Name)
			continue
		}
		url := sr.LicenseData.URL()
		if checkedURLs[url] {
			continue
		}
		if strings.Contains(url, "3rd party library") {
			continue
		}
		if url != "" {
			fmt.Printf(" -> %s\n", url)
			resp, err := http.Get(url)
			if err != nil {
				fmt.Printf("%v-%v %v: %v \n", sr.ProjectRoot, sr.Pattern.Name, url, err)
				numBadLinks = numBadLinks + 1
			} else if resp.Status != "200 OK" {
				fmt.Printf("%v-%v %v: %v\n", sr.ProjectRoot, sr.Pattern.Name, url, resp.Status)
				numBadLinks = numBadLinks + 1
			}
			checkedURLs[url] = true
			time.Sleep(200 * time.Millisecond)
		}
	}

	if numBadLinks > 0 {
		return fmt.Errorf("Encountered %d bad license URLs.\n", numBadLinks)
	}
	return nil

}

func AllProjectsMustHaveALicense() error {
	name := "AllProjectsMustHaveALicense"

	var b strings.Builder
	b.WriteString("All projects should include relevant license information, and a README.fuchsia file pointing to the file.\n")
	b.WriteString("The following projects were found without any license information:\n\n")

	// Retrieve allowlists from config files
	allowlist := make(map[string]bool, 0)
	for _, c := range Config.Checks {
		if c.Name == name {
			for k, v := range c.Allowlist {
				allowlist[k] = v
			}
		}
	}

	badReadmes := make([]string, 0)
	for _, p := range project.FilteredProjects {
		if _, ok := allowlist[p.Root]; ok {
			continue
		}

		if len(p.LicenseFiles) == 0 {
			badReadmes = append(badReadmes, fmt.Sprintf("-> %v (README.fuchsia file: %v)\n", p.Root, p.ReadmeFile.ReadmePath))
		}
	}
	sort.Strings(badReadmes)

	if len(badReadmes) > 0 {
		for _, s := range badReadmes {
			b.WriteString(s)
		}
	}
	b.WriteString("\nPlease add a LICENSE file to the above projects, and point to them in the associated README.fuchsia file.\n")
	if len(badReadmes) > 0 {
		return fmt.Errorf(b.String())
	}
	return nil
}

func AllFilesAndFoldersMustBeIncludedInAProject() error {
	name := "AllFilesAndFoldersMustBeIncludedInAProject"

	var b strings.Builder
	var recurse func(*directory.Directory)
	count := 0

	// Retrieve allowlists from config files
	allowlist := make(map[string]bool, 0)
	for _, c := range Config.Checks {
		if c.Name == name {
			for k, v := range c.Allowlist {
				allowlist[k] = v
			}
		}
	}

	b.WriteString("All files and folders must have proper license attribution.\n")
	b.WriteString("This means a license file needs to accompany all first and third party projects,\n")
	b.WriteString("and a README.fuchsia file must exist and specify where the license file lives.\n")
	b.WriteString("The following directories are not included in a project:\n\n")
	recurse = func(d *directory.Directory) {
		if d.Project == project.UnknownProject && len(d.Files) > 0 {
			if _, ok := allowlist[d.Path]; !ok {
				b.WriteString(fmt.Sprintf("-> %s\n", d.Path))
				count = count + 1
				return
			}
		}
		for _, child := range d.Children {
			recurse(child)
		}
	}
	recurse(directory.RootDirectory)

	b.WriteString("\nPlease add a LICENSE file to the above projects, and point to them in an associated README.fuchsia file.\n")
	b.WriteString("If this is urgent and you cannot complete the above steps, add the above paths to the relevant allowlist\n")
	b.WriteString("in tools/check-licenses/result/_config.json (last resort).\n")

	if count > 0 {
		return fmt.Errorf(b.String())
	}
	return nil
}

func AllReadmeFuchsiaFilesMustBeFormattedCorrectly() error {
	var b strings.Builder

	b.WriteString("All README.fuchsia files must be formatted correctly, using known directives.\n")
	b.WriteString("https://fuchsia.dev/fuchsia-src/development/source_code/third-party-metadata\n")
	b.WriteString("The following README files are malformed:\n\n")

	count := 0
	for _, r := range readme.AllReadmes {
		if len(r.MalformedLines) > 0 {
			b.WriteString(fmt.Sprintf(" -> %s [%s]\n",
				r.ReadmePath, r.MalformedLines[0]))
			count = count + 1
		}
	}
	b.WriteString("\nPlease fix the above README files.\n")

	if count == 0 {
		return nil
	}
	return fmt.Errorf(b.String())
}
