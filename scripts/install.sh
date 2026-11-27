#!/bin/sh
# Installs the langspan binary from a LangSpan release into ~/.local/bin.
#
#   curl -fsSL https://github.com/Langspan/LangSpan.dev/releases/latest/download/install.sh | sh
#   curl -fsSL https://github.com/Langspan/LangSpan.dev/releases/latest/download/install.sh | sh -s -- v0.4.0
#
# With no argument it installs the latest release; with a version such as
# v0.4.0 it installs that one. It downloads SHA256SUMS and your platform's
# archive over HTTPS, checks the archive's SHA-256 before extracting it, and
# copies langspan to ~/.local/bin. It needs no root, changes no shell startup
# file and doesn't run `langspan setup`; it prints the next steps instead.
#
# On macOS, a file a browser downloads is marked for Gatekeeper, which rejects
# the release binary because it isn't signed with an Apple Developer ID. curl
# doesn't set that mark, so the binary this script installs runs as is.
set -eu

repo_url=https://github.com/Langspan/LangSpan.dev
bin_dir=${HOME:?HOME is not set; set it to your home folder and run the installer again}/.local/bin
tmp=

fail() {
	printf 'langspan install: %s\n' "$1" >&2
	shift
	for line in "$@"; do
		printf '  %s\n' "$line" >&2
	done
	exit 1
}

cleanup() {
	if [ -n "$tmp" ]; then
		rm -rf -- "$tmp"
	fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

need() {
	command -v "$1" >/dev/null 2>&1 ||
		fail "$1 is not installed." "Install $1 and run the installer again."
}

# The release target for this machine, from uname.
detect_target() {
	os=$(uname -s)
	arch=$(uname -m)
	case "$os/$arch" in
	Darwin/x86_64)
		# A shell under Rosetta reports x86_64 on Apple silicon.
		if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
			echo aarch64-apple-darwin
		else
			echo x86_64-apple-darwin
		fi
		;;
	Darwin/arm64 | Darwin/aarch64) echo aarch64-apple-darwin ;;
	Linux/x86_64 | Linux/amd64) echo x86_64-unknown-linux-gnu ;;
	Linux/aarch64 | Linux/arm64) echo aarch64-unknown-linux-gnu ;;
	*)
		fail "no LangSpan release for this platform ($os $arch)." \
			"Releases cover macOS on arm64 and x86_64, and Linux on x86_64 and aarch64." \
			"To build from source, see $repo_url/blob/main/docs/install.md#build-from-source"
		;;
	esac
}

# Downloads URL $1 to file $2, HTTPS only, at most $3 bytes.
download() {
	curl --proto '=https' --tlsv1.2 -fsSL --connect-timeout 20 --max-time 600 \
		--max-filesize "$3" -o "$2" "$1" ||
		fail "could not download $1" \
			"Check your network connection and that the release exists:" \
			"$repo_url/releases" \
			"Then run the installer again."
}

sha256_of() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | awk '{ print $1 }'
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" | awk '{ print $1 }'
	else
		fail "no SHA-256 tool found (sha256sum or shasum)." \
			"Install one of them and run the installer again; nothing was installed."
	fi
}

if [ "$#" -gt 1 ]; then
	fail "expected at most one argument, a version such as v0.4.0; got $#."
fi
version=${1:-}
case "$version" in
"" | latest)
	base="$repo_url/releases/latest/download"
	;;
v[0-9]*.[0-9]*.[0-9]* | [0-9]*.[0-9]*.[0-9]*)
	case "$version" in
	*[!0-9A-Za-z.-]*) fail "'$version' is not a version; give one such as v0.4.0." ;;
	esac
	version="v${version#v}"
	base="$repo_url/releases/download/$version"
	;;
*)
	fail "'$version' is not a version; give one such as v0.4.0, or nothing for the latest release."
	;;
esac

for tool in uname curl awk tar mktemp dirname; do
	need "$tool"
done
target=$(detect_target)
tmp=$(mktemp -d)

download "$base/SHA256SUMS" "$tmp/SHA256SUMS" 65536

# The one archive for this target, and its digest. Names are checked so a
# listed name can't reach outside the temporary folder.
listing=$(awk -v suffix="-$target.tar.gz" '
	length($2) > length(suffix) && substr($2, length($2) - length(suffix) + 1) == suffix { print $1, $2 }
' "$tmp/SHA256SUMS")
case "$listing" in
"" | *"
"*)
	fail "SHA256SUMS at $base lists no single archive for $target." \
		"Check $repo_url/releases for a release that has one."
	;;
esac
expected=${listing%% *}
archive=${listing#* }
case "$archive" in
langspan-v*-"$target".tar.gz) ;;
*) fail "SHA256SUMS names an unexpected archive '$archive'; nothing was installed." ;;
esac
case "$archive$expected" in
*[!0-9A-Za-z._-]*) fail "SHA256SUMS has an unexpected line for '$archive'; nothing was installed." ;;
esac

download "$base/$archive" "$tmp/$archive" 268435456

actual=$(sha256_of "$tmp/$archive")
if [ "$actual" != "$expected" ]; then
	fail "the checksum of $archive does not match SHA256SUMS; nothing was installed." \
		"expected $expected" \
		"got      $actual" \
		"The download is incomplete or was altered. Run the installer again;" \
		"if it fails the same way, report it at $repo_url/issues"
fi

stem=${archive%.tar.gz}
mkdir "$tmp/unpacked"
tar -xzf "$tmp/$archive" -C "$tmp/unpacked" "$stem/langspan" ||
	fail "could not extract langspan from $archive; nothing was installed." \
		"Run the installer again; if it fails the same way, report it at $repo_url/issues"
binary="$tmp/unpacked/$stem/langspan"
if [ ! -f "$binary" ] || [ -L "$binary" ]; then
	fail "$archive holds no langspan binary; nothing was installed." \
		"Report it at $repo_url/issues"
fi

mkdir -p "$bin_dir"
# Copy next to the destination, then rename, so a running langspan is
# never overwritten in place and a failed copy leaves the old one.
cp "$binary" "$bin_dir/.langspan.install.$$"
chmod 0755 "$bin_dir/.langspan.install.$$"
mv -f "$bin_dir/.langspan.install.$$" "$bin_dir/langspan"

printf 'Installed %s to %s/langspan\n\nNext steps:\n' "$stem" "$bin_dir"
# The langspan this shell would run is the first one on PATH. Folders are
# compared after resolving symlinks, so a trailing slash or a linked folder
# still counts as this folder.
first=$(command -v langspan 2>/dev/null || true)
here=$(CDPATH='' cd -- "$bin_dir" && pwd -P)
there=
case "$first" in
*/*) there=$(CDPATH='' cd -- "$(dirname -- "$first")" 2>/dev/null && pwd -P || true) ;;
esac
step=1
if [ "$there" != "$here" ]; then
	if [ -n "$there" ]; then
		printf '  %s. %s runs first, not this install. Put %s first on PATH\n' "$step" "$first" "$bin_dir"
		printf '     for this shell and new ones:\n'
	else
		printf '  %s. Add %s to PATH for this shell and new ones:\n' "$step" "$bin_dir"
	fi
	# shellcheck disable=SC2016 # the line is printed for the user to run
	printf '       export PATH="$HOME/.local/bin:$PATH"\n'
	# shellcheck disable=SC2016
	printf '       echo '\''export PATH="$HOME/.local/bin:$PATH"'\'' >> ~/.%src\n' "$(basename "${SHELL:-sh}")"
	step=$((step + 1))
fi
printf '  %s. Close Claude Code, then connect your agents: langspan setup\n' "$step"
printf '  %s. See a refusal without an agent: langspan demo\n' "$((step + 1))"
