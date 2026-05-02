# Maintainer: you
pkgname=keep
pkgver=1.0.0
pkgrel=1
pkgdesc="File sync and storage tool — mirror files to a versioned storage directory"
arch=('x86_64' 'aarch64')
license=('MIT')
depends=('rsync')

package() {
	install -Dm755 "$startdir/keep" "$pkgdir/usr/bin/keep"
	"$pkgdir/usr/bin/keep" completion zsh  | install -Dm644 /dev/stdin "$pkgdir/usr/share/zsh/site-functions/_keep"
	"$pkgdir/usr/bin/keep" completion bash | install -Dm644 /dev/stdin "$pkgdir/usr/share/bash-completion/completions/keep"
	"$pkgdir/usr/bin/keep" completion fish | install -Dm644 /dev/stdin "$pkgdir/usr/share/fish/vendor_completions.d/keep.fish"
}
