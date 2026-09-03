#!/usr/bin/env bash
# Render the Homebrew formula for a release.
#   formula.sh VERSION SHA_DARWIN_ARM SHA_DARWIN_INTEL SHA_LINUX_ARM SHA_LINUX_INTEL
set -euo pipefail

version="${1:?version}"
darwin_arm="${2:?sha256 for aarch64-apple-darwin}"
darwin_intel="${3:?sha256 for x86_64-apple-darwin}"
linux_arm="${4:?sha256 for aarch64-unknown-linux-musl}"
linux_intel="${5:?sha256 for x86_64-unknown-linux-musl}"
base="https://github.com/yungibly/cubby/releases/download/v$version"

cat <<RUBY
class Cubby < Formula
  desc "Keep copies of your dotfiles in a store that mirrors your home directory"
  homepage "https://github.com/yungibly/cubby"
  version "$version"
  license "MIT"

  on_macos do
    on_arm do
      url "$base/cubby-$version-aarch64-apple-darwin.tar.gz"
      sha256 "$darwin_arm"
    end
    on_intel do
      url "$base/cubby-$version-x86_64-apple-darwin.tar.gz"
      sha256 "$darwin_intel"
    end
  end

  on_linux do
    on_arm do
      url "$base/cubby-$version-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$linux_arm"
    end
    on_intel do
      url "$base/cubby-$version-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$linux_intel"
    end
  end

  def install
    bin.install "cubby"
    bash_completion.install "completions/cubby.bash" => "cubby"
    zsh_completion.install "completions/cubby.zsh" => "_cubby"
    fish_completion.install "completions/cubby.fish"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/cubby --version")
    ENV["CUBBY_HOME"] = testpath
    system bin/"cubby", "init"
    assert_predicate testpath/".dotfiles/.cubby.toml", :exist?
  end
end
RUBY
