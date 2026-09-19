#!/bin/sh
# Trigger the tagged-release GitHub Actions workflow without committing changes.
set -eu

usage() {
    printf 'Usage: %s [--detach]\n' "$0" >&2
    printf 'Create and push the version tag derived from Cargo.toml.\n' >&2
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
script_path=$project_root/build-aux/release-flatpak-github.sh

case "${1-}" in
    --detach)
        if [ "$#" -ne 1 ]; then
            usage
            exit 2
        fi

        release_dir=$project_root/target/release
        run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
        log_path=$release_dir/flatpak-github-release-$run_id.log
        pid_path=$release_dir/flatpak-github-release-$run_id.pid
        mkdir -p "$release_dir"

        # A private log can contain diagnostics from credential helpers.
        umask 077
        nohup "$script_path" >"$log_path" 2>&1 < /dev/null &
        pid=$!
        printf '%s\n' "$pid" >"$pid_path"
        printf 'Started Flatpak GitHub release launcher (PID %s).\n' "$pid"
        printf 'Log: %s\nPID file: %s\n' "$log_path" "$pid_path"
        exit 0
        ;;
    --help|-h)
        usage
        exit 0
        ;;
    '')
        ;;
    *)
        usage
        exit 2
        ;;
esac

cd "$project_root"

for command in git gh grep sed; do
    command -v "$command" >/dev/null 2>&1 || die "Required command is unavailable: $command"
done

if [ -n "$(git status --porcelain)" ]; then
    die 'Working tree is not clean; commit, stash, or discard changes before creating a release tag.'
fi

branch=$(git branch --show-current)
if [ "$branch" != main ]; then
    die "Release tags must be created from main, not ${branch:-a detached HEAD}."
fi

package_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
if ! printf '%s\n' "$package_version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    die "Cargo.toml must contain a numeric MAJOR.MINOR.PATCH version; got ${package_version:-nothing}."
fi
release_tag=v$package_version

grep -Fq "version: '$package_version'" meson.build || \
    die "meson.build does not declare version $package_version."
grep -Fq "<release version=\"$package_version\"" \
    data/io.github.mendrik_private.Diorama.metainfo.xml.in || \
    die "AppStream metadata does not contain release version $package_version."

if ! gh auth status --hostname github.com >/dev/null 2>&1; then
    die 'GitHub CLI authentication is unavailable or invalid. Run: gh auth login --hostname github.com'
fi

git fetch --quiet origin main
if ! git merge-base --is-ancestor HEAD origin/main; then
    die 'HEAD is not an ancestor of origin/main; update main before creating a release tag.'
fi

if git show-ref --tags --verify --quiet "refs/tags/$release_tag"; then
    die "Local tag $release_tag already exists; refusing to move or reuse it."
fi
if git ls-remote --exit-code --tags origin "refs/tags/$release_tag" >/dev/null 2>&1; then
    die "Remote tag $release_tag already exists; refusing to overwrite it."
fi

git tag -a "$release_tag" -m "Release $release_tag"
if ! git push origin "refs/tags/$release_tag"; then
    die "Pushing $release_tag failed. The newly-created local tag remains for inspection."
fi

printf 'Pushed %s. The GitHub Actions release workflow is now building the Flatpak.\n' "$release_tag"
printf 'Follow it at: https://github.com/mendrik-private/diorama/actions/workflows/release.yml\n'
printf 'The GitHub Release is published after that workflow succeeds.\n'
