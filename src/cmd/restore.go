package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
)

var (
	yes    bool
	dryRunRestore bool
)

var restoreCmd = &cobra.Command{
	Use:   "restore <file|dir>...",
	Short: "Restore files from your storage directory",
	Long: `Restore files and directories from your storage directory back to
their original locations under $HOME. Paths are resolved relative to your
current working directory, mirrored into the storage directory.`,
	Args: cobra.MinimumNArgs(1),
	RunE: runRestore,
	ValidArgsFunction: func(cmd *cobra.Command, args []string, toComplete string) ([]string, cobra.ShellCompDirective) {
		return nil, cobra.ShellCompDirectiveDefault
	},
}

func init() {
	restoreCmd.Flags().BoolVarP(&yes, "yes", "y", false, "skip overwrite prompts")
	restoreCmd.Flags().BoolVarP(&dryRunRestore, "dry-run", "n", false, "simulate without making changes")
}

func runRestore(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()
	cwd, _ := os.Getwd()
	debugf("home=%s store=%s cwd=%s", home, store, cwd)

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
		src := filepath.Join(store, rel)
		dst := abs
		debugf("target=%s abs=%s rel=%s src=%s dst=%s", target, abs, rel, src, dst)

		info, err := os.Stat(src)
		if err != nil {
			errorf("%s not found in storage directory", rel)
			continue
		}

		if info.IsDir() {
			if dryRunRestore {
				printDryRunHeader(store+"/"+rel, "~/"+rel)
				syncDir(src, dst, true)
				continue
			}
			if err := os.MkdirAll(dst, 0755); err != nil {
				errorf("%v", err)
				continue
			}
			if err := syncDir(src, dst, false); err != nil {
				errorf("restore failed for %s: %v", target, err)
				continue
			}
		} else {
			if dryRunRestore {
				printDryRunHeader(store+"/"+rel, "~/"+rel)
				dryRunFile(src, dst)
				continue
			}
			if _, err := os.Stat(dst); err == nil && !yes {
				if !confirm(fmt.Sprintf("overwrite ~/%s?", rel)) {
					continue
				}
			}
			if err := os.MkdirAll(filepath.Dir(dst), 0755); err != nil {
				errorf("%v", err)
				continue
			}
			if err := copyFile(src, dst); err != nil {
				errorf("restore failed for %s: %v", target, err)
				continue
			}
		}

		printRestore(rel, store)
		logEntry("restore", rel)
	}

	return nil
}
