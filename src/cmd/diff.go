package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
)

var diffCmd = &cobra.Command{
	Use:   "diff <file|dir>...",
	Short: "Show differences between live files and stored versions",
	Long: `Show a line-by-line diff between each live file and its stored version.
For directories, all differing files are shown. Paths are resolved relative
to your current working directory.`,
	Args: cobra.MinimumNArgs(1),
	RunE: runDiff,
	ValidArgsFunction: func(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
		return nil, cobra.ShellCompDirectiveDefault
	},
}

func init() {
	rootCmd.AddCommand(diffCmd)
}

func diffFiles(liveFile, storeFile, label string) {
	// check identical first
	liveHash, err1 := hashFile(liveFile)
	storeHash, err2 := hashFile(storeFile)
	if err1 == nil && err2 == nil && string(liveHash) == string(storeHash) {
		return // skip identical files silently in directory mode
	}

	fmt.Printf("\n%s  %s\n", yellow("~"), bold(label))
	fmt.Printf("%s\n", dim(strings.Repeat("─", 60)))

	cmd := exec.Command("diff",
		"--color=always",
		"--unified=3",
		"--label", "stored",
		"--label", "live",
		storeFile, liveFile,
	)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Run() // exits 1 when files differ — expected, ignore error
}

func runDiff(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()
	cwd, _ := os.Getwd()

	for _, target := range args {
		abs, err := filepath.Abs(filepath.Join(cwd, target))
		if err != nil {
			errorf("cannot resolve %s: %v", target, err)
			continue
		}

		if !strings.HasPrefix(abs, home+string(filepath.Separator)) {
			errorf("%s is outside $HOME", abs)
			continue
		}

		rel := strings.TrimPrefix(abs, home+string(filepath.Separator))
		storePath := filepath.Join(store, rel)

		liveInfo, liveErr := os.Stat(abs)
		if os.IsNotExist(liveErr) {
			errorf("live path not found: ~/%s", rel)
			continue
		}
		if _, err := os.Stat(storePath); os.IsNotExist(err) {
			errorf("not in storage: %s", rel)
			continue
		}

		if liveInfo.IsDir() {
			// walk the store side, diff each file against live
			any := false
			err := filepath.Walk(storePath, func(sp string, info os.FileInfo, err error) error {
				if err != nil || info.IsDir() {
					return nil
				}
				fileRel, _ := filepath.Rel(storePath, sp)
				liveFile := filepath.Join(abs, fileRel)
				displayLabel := "~/" + rel + "/" + fileRel

				if _, err := os.Stat(liveFile); os.IsNotExist(err) {
					fmt.Printf("\n%s  %s  %s\n", red("-"), bold(displayLabel), dim("(missing from live)"))
					any = true
					return nil
				}

				liveHash, err1 := hashFile(liveFile)
				storeHash, err2 := hashFile(sp)
				if err1 != nil || err2 != nil {
					return nil
				}
				if string(liveHash) != string(storeHash) {
					diffFiles(liveFile, sp, displayLabel)
					any = true
				}
				return nil
			})
			if err != nil {
				errorf("error walking %s: %v", target, err)
				continue
			}

			// also check for files in live that aren't in the store
			filepath.Walk(abs, func(lp string, info os.FileInfo, err error) error {
				if err != nil || info.IsDir() {
					return nil
				}
				fileRel, _ := filepath.Rel(abs, lp)
				sp := filepath.Join(storePath, fileRel)
				displayLabel := "~/" + rel + "/" + fileRel
				if _, err := os.Stat(sp); os.IsNotExist(err) {
					fmt.Printf("\n%s  %s  %s\n", green("+"), bold(displayLabel), dim("(not in storage)"))
					any = true
				}
				return nil
			})

			if !any {
				fmt.Printf("%s  %s  %s\n", green("="), bold("~/"+rel+"/"), dim("(identical)"))
			}
		} else {
			// single file
			liveHash, err1 := hashFile(abs)
			storeHash, err2 := hashFile(storePath)
			if err1 == nil && err2 == nil && string(liveHash) == string(storeHash) {
				fmt.Printf("%s  %s  %s\n", green("="), bold("~/"+rel), dim("(identical)"))
				continue
			}
			diffFiles(abs, storePath, "~/"+rel)
		}
	}

	return nil
}
