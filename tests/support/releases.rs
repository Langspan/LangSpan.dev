//! Which releases the tests ask a frozen event-type list and golden stores
//! of (issue #566). Both are made from a release's tag, after it is
//! published, so the version this build carries is never asked for them:
//! publishing a release changes no commit, and so no pull request's checks.
//! The next release pull request bumps the version, and from then on the
//! previous release is older and required, on that pull request first.

/// Whether `release` is older than `current`, the version this build
/// carries, comparing the numbers of `MAJOR.MINOR.PATCH` (0.10.0 is newer
/// than 0.9.0).
pub fn required(release: &str, current: &str) -> bool {
    numbers(release) < numbers(current)
}

fn numbers(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| {
            part.parse().unwrap_or_else(|_| {
                panic!("{version} is not a MAJOR.MINOR.PATCH version: fix the CHANGELOG.md heading or the manifest")
            })
        })
        .collect()
}
