# Maintainer: you
pkgname=keep
pkgver=1.0.0
pkgrel=1
pkgdesc="Sync and store program for configuration files or whatever you want."
arch=('x86_64' 'aarch64')
license=('MIT')
depends=('rsync')
_github_user="yungibly"
source_x86_64=("keep-linux-amd64::https://github.com/${_github_user}/keep/releases/download/v${pkgver}/keep-linux-amd64")
source_aarch64=("keep-linux-arm64::https://github.com/${_github_user}/keep/releases/download/v${pkgver}/keep-linux-arm64")
sha256sums_x86_64=('SKIP')
sha256sums_aarch64=('SKIP')

package() {
	if [ "$CARCH" = "x86_64" ]; then
		install -Dm755 "$srcdir/keep-linux-amd64" "$pkgdir/usr/bin/keep"
	else
		install -Dm755 "$srcdir/keep-linux-arm64" "$pkgdir/usr/bin/keep"
	fi

	"$pkgdir/usr/bin/keep" completion zsh  | install -Dm644 /dev/stdin "$pkgdir/usr/share/zsh/site-functions/_keep"
	"$pkgdir/usr/bin/keep" completion bash | install -Dm644 /dev/stdin "$pkgdir/usr/share/bash-completion/completions/keep"
	"$pkgdir/usr/bin/keep" completion fish | install -Dm644 /dev/stdin "$pkgdir/usr/share/fish/vendor_completions.d/keep.fish"
}
