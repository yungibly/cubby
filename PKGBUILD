# Maintainer: you
pkgname=cubby
pkgver=1.0.2
pkgrel=1
pkgdesc="File sync and storage tool — mirror files to a versioned storage directory"
arch=('x86_64' 'aarch64')
license=('MIT')
depends=('rsync')

package() {
	install -Dm755 "$startdir/cubby" "$pkgdir/usr/bin/cubby"
	"$pkgdir/usr/bin/cubby" completion zsh  | install -Dm644 /dev/stdin "$pkgdir/usr/share/zsh/site-functions/_cubby"
	"$pkgdir/usr/bin/cubby" completion bash | install -Dm644 /dev/stdin "$pkgdir/usr/share/bash-completion/completions/cubby"
	"$pkgdir/usr/bin/cubby" completion fish | install -Dm644 /dev/stdin "$pkgdir/usr/share/fish/vendor_completions.d/cubby.fish"
}
